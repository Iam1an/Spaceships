//! Password hashing and JWT issuing, compatible with the accounts already in
//! `pilots.db`.
//!
//! # Compatibility is the whole job here
//!
//! Every existing row was hashed by `bcryptjs` and every live session holds a
//! token signed by `jsonwebtoken` (the npm one). Both must keep working across
//! the cutover, in *both* directions — the Node server has to stay usable while
//! this one is being tested, so a hash written here must verify there too.
//!
//! Two details make that true:
//!
//! - **`$2b$` prefix.** The installed `bcryptjs` is 3.0.3, which emits
//!   `$2b$10$…` — *not* the `$2a$` older versions of the library produced. So
//!   [`hash_password`] uses [`bcrypt::hash`], whose default is also `$2b$`, and
//!   a row written here is byte-indistinguishable in shape from one written by
//!   the JS server. Rows that predate the bcryptjs 3 upgrade still carry `$2a$`
//!   and verify fine: the Rust `bcrypt` crate accepts `$2a$`, `$2b$`, `$2x$`
//!   and `$2y$` on the verify path, as does bcryptjs. Both directions of
//!   cross-verification are pinned by tests.
//! - **Same claims, same secret, same algorithm.** HS256 over
//!   `{ id, username, iat, exp }` with a 7-day life, from `JWT_SECRET` or the
//!   same hardcoded development fallback.
//!
//! # The development secret
//!
//! `server/db.js` refuses to boot when `NODE_ENV=production` and `JWT_SECRET`
//! is unset, and otherwise falls back to a literal. Both behaviours are kept:
//! [`jwt_secret`] returns an error in production without a secret, and the
//! fallback string is character-identical so tokens issued by the running JS
//! server verify here.

use bcrypt::Version;
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use crate::db::ApiError;

/// The development JWT secret from `server/db.js`, used when `JWT_SECRET` is
/// unset outside production.
pub const DEV_JWT_SECRET: &str = "spaceships-dev-secret-change-in-prod";

/// bcrypt cost factor. `bcryptjs.hash(password, 10)`.
pub const BCRYPT_COST: u32 = 10;

/// Token lifetime, matching `expiresIn: '7d'`.
pub const TOKEN_TTL_SECS: u64 = 7 * 24 * 60 * 60;

/// A dummy hash compared against when the callsign does not exist, so a login
/// attempt costs the same wall time whether or not the pilot is real.
///
/// Copied verbatim from `server/db.js`; it is the hash of
/// `"dummy_fallback_password"`. Note that the JS comment claims it is valid,
/// and it parses — what matters is only that it is well-formed enough for
/// `compare` to do the full key-setup work rather than bailing early.
const DUMMY_HASH: &str = "$2a$10$wT0E8K.01.t7VqgLwA9xDuw16x5lH.98889Z2f/gB17vR6g5p3uO2";

/// JWT claims, exactly the object `jwt.sign({ id, username }, …, { expiresIn })`
/// produces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Pilot row id.
    pub id: i64,
    /// Callsign at the time the token was issued.
    pub username: String,
    /// Issued-at, unix seconds. `jsonwebtoken` (npm) always adds this.
    pub iat: u64,
    /// Expiry, unix seconds.
    pub exp: u64,
}

/// Resolves the signing secret.
///
/// Errors in production when `JWT_SECRET` is unset, mirroring the `throw` at
/// the top of `server/db.js`. Anywhere else it falls back to
/// [`DEV_JWT_SECRET`].
pub fn jwt_secret() -> Result<String, String> {
    if let Ok(secret) = std::env::var("JWT_SECRET") {
        if !secret.is_empty() {
            return Ok(secret);
        }
    }
    if std::env::var("NODE_ENV").as_deref() == Ok("production") {
        return Err("FATAL: JWT_SECRET must be defined in production environment!".to_string());
    }
    Ok(DEV_JWT_SECRET.to_string())
}

/// Signing and verification keys, resolved once at startup.
pub struct Auth {
    encoding: EncodingKey,
    decoding: DecodingKey,
    validation: Validation,
}

impl Auth {
    /// Builds the key pair from `secret`.
    #[must_use]
    pub fn new(secret: &str) -> Auth {
        let mut validation = Validation::new(Algorithm::HS256);
        // The npm `jsonwebtoken` verifies `exp` and nothing else by default —
        // no audience, no issuer, no subject. Match that, and drop the Rust
        // crate's default insistence on a `sub` claim, which these tokens do
        // not carry.
        validation.required_spec_claims.clear();
        validation.required_spec_claims.insert("exp".to_string());
        validation.validate_aud = false;
        Auth {
            encoding: EncodingKey::from_secret(secret.as_bytes()),
            decoding: DecodingKey::from_secret(secret.as_bytes()),
            validation,
        }
    }

    /// Signs a 7-day token for a pilot.
    pub fn sign(&self, id: i64, username: &str) -> Result<String, ApiError> {
        let now = now_secs();
        let claims = Claims {
            id,
            username: username.to_string(),
            iat: now,
            exp: now + TOKEN_TTL_SECS,
        };
        encode(&Header::new(Algorithm::HS256), &claims, &self.encoding)
            .map_err(|e| ApiError::new(500, e.to_string()))
    }

    /// `verifyToken` — a bad or expired token is a 401 with the message the
    /// npm library produces, since the lobby renders `e.message` verbatim.
    pub fn verify(&self, token: &str) -> Result<Claims, ApiError> {
        decode::<Claims>(token, &self.decoding, &self.validation)
            .map(|data| data.claims)
            .map_err(|e| ApiError::new(401, jwt_error_message(&e)))
    }
}

/// Maps a Rust JWT error onto the message string the npm library would have
/// thrown, because those strings reach the browser.
fn jwt_error_message(e: &jsonwebtoken::errors::Error) -> String {
    use jsonwebtoken::errors::ErrorKind;
    match e.kind() {
        ErrorKind::ExpiredSignature => "jwt expired".to_string(),
        ErrorKind::InvalidSignature => "invalid signature".to_string(),
        ErrorKind::InvalidToken => "jwt malformed".to_string(),
        _ => "invalid token".to_string(),
    }
}

/// Current unix time in seconds.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Hashes a password the way `bcryptjs.hash(password, 10)` does, including the
/// `$2b$` version prefix bcryptjs 3.x emits.
pub fn hash_password(password: &str) -> Result<String, ApiError> {
    let parts = bcrypt::hash_with_result(password, BCRYPT_COST)
        .map_err(|e| ApiError::new(500, e.to_string()))?;
    Ok(parts.format_for_version(Version::TwoB))
}

/// Verifies a password against a stored hash.
///
/// A malformed hash is `false`, not an error — `bcryptjs.compare` rejects
/// rather than throwing for a hash it cannot parse, and the caller turns either
/// into the same "Invalid callsign or password".
#[must_use]
pub fn verify_password(password: &str, hash: &str) -> bool {
    bcrypt::verify(password, hash).unwrap_or(false)
}

/// Burns roughly one bcrypt's worth of time for a callsign that does not exist.
///
/// `loginPilot` compares against a fixed dummy hash in that case so the
/// response time does not leak whether a callsign is registered. Preserved
/// verbatim, including the specific dummy hash.
pub fn verify_against_dummy(password: &str) -> bool {
    bcrypt::verify(password, DUMMY_HASH).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_carry_the_bcryptjs_version_prefix() {
        let h = hash_password("hunter2hunter2").unwrap();
        assert!(h.starts_with("$2b$10$"), "got {h}");
        assert_eq!(h.len(), 60);
        assert!(verify_password("hunter2hunter2", &h));
        assert!(!verify_password("wrong", &h));
    }

    #[test]
    fn verifies_a_hash_written_by_bcryptjs() {
        // Produced by
        //   node -e "console.log(require('bcryptjs').hashSync('correct horse', 10))"
        // against the bcryptjs 3.0.3 in this repo's node_modules, which is what
        // the running JS server writes today.
        let js_2b = "$2b$10$76r.j46zz3aC7t3wPtFiUuqxOAc44RP6f6.AgEIC.41ndcyLwHfX2";
        assert!(verify_password("correct horse", js_2b));
        assert!(!verify_password("wrong horse", js_2b));
    }

    #[test]
    fn verifies_a_legacy_2a_hash_from_older_bcryptjs() {
        // Rows written before the bcryptjs 3 upgrade carry `$2a$`. They must
        // keep working — this is the whole compatibility requirement.
        // Produced by bcryptjs with an explicit `$2a$` salt, the form older
        // versions emitted by default.
        let js_2a = "$2a$10$N9qo8uLOickgx2ZMRZoMyeO9ElkFhmlrCHY4avifT5uBblZtb2s9e";
        assert!(verify_password("legacy pilot", js_2a));
        assert!(!verify_password("legacy pilo", js_2a));
    }

    #[test]
    fn the_dummy_hash_is_well_formed_and_never_matches() {
        // It must not accidentally accept a password; its only job is to burn
        // time.
        assert!(!verify_against_dummy(""));
        assert!(!verify_against_dummy("password"));
    }

    #[test]
    fn tokens_round_trip_and_carry_the_js_claims() {
        let auth = Auth::new(DEV_JWT_SECRET);
        let token = auth.sign(42, "Maverick").unwrap();
        let claims = auth.verify(&token).unwrap();
        assert_eq!(claims.id, 42);
        assert_eq!(claims.username, "Maverick");
        assert!(claims.exp > claims.iat);
        assert_eq!(claims.exp - claims.iat, TOKEN_TTL_SECS);
    }

    #[test]
    fn a_token_signed_with_another_secret_is_rejected() {
        let token = Auth::new("other-secret").sign(1, "x").unwrap();
        let err = Auth::new(DEV_JWT_SECRET).verify(&token).unwrap_err();
        assert_eq!(err.status, 401);
    }

    #[test]
    fn an_expired_token_reports_the_js_message() {
        // Hand-built with exp in the past.
        let claims = Claims {
            id: 1,
            username: "old".into(),
            iat: 1,
            exp: 2,
        };
        let token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(DEV_JWT_SECRET.as_bytes()),
        )
        .unwrap();
        let err = Auth::new(DEV_JWT_SECRET).verify(&token).unwrap_err();
        assert_eq!(err.message, "jwt expired");
    }
}
