//! The Spaceships game server: HTTP API, static files, WebSocket lobby, and
//! SQLite persistence.
//!
//! This replaces `server/index.js` (1004 lines) and `server/db.js` (608 lines).
//! It serves the **unmodified** browser client in `public/` and reads the
//! **existing** `pilots.db`, so it is a drop-in swap for `node server/index.js`
//! rather than a new system.
//!
//! # Layout
//!
//! | module | replaces |
//! |--------|----------|
//! | [`http`] | the Express app and `express.static` (`index.js:17-199`) |
//! | [`ws`] | the hand-rolled RFC 6455 implementation (`index.js:203-407`) |
//! | [`lobby`] | rooms, matchmaking, the match clock, relay (`index.js:409-1005`) |
//! | [`db`] | `db.js` — schema, queries, credits, results |
//! | [`achievements`] | `ACHIEVEMENT_DEFS` and its checks |
//! | [`auth`] | bcrypt and JWT |
//! | [`field`] | asteroid generation, delegated to [`spaceships_sim`] |
//! | [`wire`] | inbound frame parsing with the JS server's tolerance |
//! | [`jsfmt`] | `JSON.stringify` and `toFixed` number formatting |
//!
//! # Authority: what changed and what did not
//!
//! The JS server trusts the client's `hit` messages outright. A client asserts
//! "I hit player 7 with a missile" and the server applies 50 damage, subject
//! only to bookkeeping checks — rate limit, spawn protection, no friendly fire,
//! no self-damage without a bot id. There is no test that the shooter fired,
//! that a projectile existed, that the two ships were in range, or that
//! anything was in line of sight.
//!
//! **That is preserved exactly.** Tightening it requires the server to own
//! projectile simulation, and a half-measure would desynchronise the shipped
//! browser client, which resolves its own hits locally and would then see
//! damage it did not predict (or fail to see damage it did). [`spaceships_sim`]
//! exists to make server-authoritative combat possible later; this port is not
//! the place to switch it on. See [`lobby::Lobby::on_hit`].
//!
//! The same applies to `self-damage` (a client can report 100 damage a frame
//! and kill itself), `asteroid-hit` (unvalidated, 1 damage per report),
//! `colors` (relayed with no validation), and `ship-model` (which checks the
//! URL is same-origin but deliberately does **not** check that the sender owns
//! the admin ship — a decision the JS records in a comment and this port
//! keeps).
//!
//! Three things did change, all flagged:
//!
//! 1. **Asteroids no longer generate inside the moon.** The JS server's
//!    `clipsMothership` checks only the two motherships, which sit outside the
//!    field radius and can never reject a placement, while the moon it should
//!    have avoided is absent from `server/index.js` entirely. Generation now
//!    goes through [`spaceships_sim::asteroids`], which avoids it. See
//!    [`field`].
//! 2. **A second `start` no longer ends the match early.** The JS installs a
//!    fresh 5-minute `setTimeout` without clearing the first and resets
//!    `matchOver` to false, so the stale timer ends the restarted match. The
//!    old timers are aborted here.
//! 3. **The WebSocket frame-dedup cache is gone**, along with the HTTP-prefix
//!    skip. Both existed to work around a Node 25 regression. Keeping the dedup
//!    would drop legitimate repeated frames.
//!
//! # Configuration
//!
//! | variable | default | meaning |
//! |----------|---------|---------|
//! | `PORT` | `4000` | listen port, same as the JS |
//! | `PILOTS_DB` | `pilots.db` | SQLite path, relative to the working directory |
//! | `JWT_SECRET` | dev fallback | required when `NODE_ENV=production` |
//! | `STATIC_DIST` | `dist` | preferred static root |
//! | `STATIC_PUBLIC` | `public` | fallback static root |

#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub mod achievements;
pub mod auth;
pub mod db;
pub mod field;
pub mod http;
pub mod jsfmt;
pub mod lobby;
pub mod wire;
pub mod ws;

use std::path::Path;
use std::sync::Arc;

use axum::Router;

use crate::auth::Auth;
use crate::db::Db;
use crate::http::AppState;
use crate::lobby::Lobby;

/// How to build the server.
pub struct Config {
    /// SQLite path.
    pub db_path: std::path::PathBuf,
    /// JWT signing secret.
    pub jwt_secret: String,
    /// Preferred static root (`dist/`).
    pub dist_dir: std::path::PathBuf,
    /// Fallback static root (`public/`).
    pub public_dir: std::path::PathBuf,
}

impl Config {
    /// Reads the configuration from the environment, with the JS server's
    /// defaults.
    pub fn from_env() -> Result<Config, String> {
        Ok(Config {
            db_path: std::env::var("PILOTS_DB")
                .unwrap_or_else(|_| "pilots.db".to_string())
                .into(),
            jwt_secret: auth::jwt_secret()?,
            dist_dir: std::env::var("STATIC_DIST")
                .unwrap_or_else(|_| "dist".to_string())
                .into(),
            public_dir: std::env::var("STATIC_PUBLIC")
                .unwrap_or_else(|_| "public".to_string())
                .into(),
        })
    }
}

/// An assembled server: the router plus the state it shares.
///
/// The `db` and `lobby` handles are returned rather than kept private because
/// the match-end payout is otherwise unreachable from a test — `end_match` runs
/// off a 300-second timer, and no test is going to wait five minutes for it.
pub struct Built {
    /// The router to hand to [`serve`].
    pub router: Router,
    /// The pilot database.
    pub db: Arc<Db>,
    /// Room and connection state.
    pub lobby: Arc<Lobby>,
}

/// Opens the database and builds the shared state.
///
/// Splitting this from the listener is what lets the integration tests bind an
/// ephemeral port against a scratch copy of `pilots.db`.
pub fn build(config: &Config) -> Result<Built, String> {
    let db = Arc::new(Db::open(&config.db_path).map_err(|e| e.message)?);
    let lobby = Arc::new(Lobby::new(Arc::clone(&db)));
    let state = AppState {
        db: Arc::clone(&db),
        auth: Arc::new(Auth::new(&config.jwt_secret)),
        lobby: Arc::clone(&lobby),
        static_files: http::static_service(&config.dist_dir, &config.public_dir),
    };
    Ok(Built {
        router: http::router(state),
        db,
        lobby,
    })
}

/// Binds `addr` and serves until the process exits.
///
/// `TCP_NODELAY` is set on every accepted connection, which the JS does too
/// (`socket.setNoDelay(true)` in the upgrade handler). Without it Nagle
/// coalesces the 20 Hz `state` broadcasts into ~100 ms bursts and the game
/// visibly stutters.
pub async fn serve(listener: tokio::net::TcpListener, app: Router) -> std::io::Result<()> {
    use axum::serve::ListenerExt;
    let listener = listener.tap_io(|stream| {
        if let Err(e) = stream.set_nodelay(true) {
            eprintln!("failed to set TCP_NODELAY: {e}");
        }
    });
    axum::serve(listener, app).await
}

/// Whether a path exists and is a directory, for the startup banner.
#[must_use]
pub fn dir_exists(p: &Path) -> bool {
    p.is_dir()
}
