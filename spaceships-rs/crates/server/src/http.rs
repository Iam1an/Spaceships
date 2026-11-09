//! The HTTP layer — the port of the Express app in `server/index.js:17-199`.
//!
//! Same paths, same methods, same JSON shapes, same status codes.
//!
//! # Key order is part of the contract
//!
//! Responses are built from `#[derive(Serialize)]` structs whose fields are in
//! the same order as the JS object literals, because `serde_json`'s `json!`
//! macro sorts keys (its `Map` is a `BTreeMap` unless `preserve_order` is on)
//! and `JSON.stringify` does not. `#[serde(flatten)]` is used for the
//! `{ ok: true, ...result }` spreads and preserves order, since serde streams
//! flattened entries straight into the parent map.
//!
//! # Status codes, faithfully
//!
//! Express catches per-route with different fallbacks, and they are *not*
//! uniform. `/api/colors`, `/api/unlocks`, `/api/unlock/:feature`,
//! `/api/credits`, `/api/credits/history` and `/api/credits/spend` all use
//! `e.status ?? 401`, so an unexpected SQL failure in one of those surfaces as
//! **401**, not 500. `/api/register`, `/api/login`, `/api/solo-result`,
//! `/api/trial-result` and `/api/campaign-result` use `?? 500`. Both are
//! reproduced rather than normalized.
//!
//! # Bodies
//!
//! Every handler reads `req.body ?? {}`, so a missing, empty, or
//! wrong-content-type body is an empty object rather than an error. Bodies are
//! therefore parsed leniently here too: anything that is not a JSON object
//! becomes one with no keys. (Express's own 400 for *malformed* JSON is the one
//! behaviour not reproduced — it returns an HTML error page, which nothing in
//! this repo reads.)

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower::ServiceExt;
use tower_http::services::ServeDir;

use crate::auth::{hash_password, verify_against_dummy, verify_password, Auth};
use crate::db::{
    login_result, ApiError, CreditTx, Db, EarnedAchievement, LeaderboardRow, LoginResult,
    ProfileView, PurchaseOutcome, UnlockCosts, Unlocks, UNLOCK_COSTS_STRUCT,
};
use crate::lobby::Lobby;

/// Everything the handlers share.
#[derive(Clone)]
pub struct AppState {
    /// Pilot database.
    pub db: Arc<Db>,
    /// JWT keys.
    pub auth: Arc<Auth>,
    /// Room and connection state.
    pub lobby: Arc<Lobby>,
    /// `dist/` with a `public/` fallback, exactly as the two chained
    /// `express.static` calls behave.
    pub static_files: ServeDir<ServeDir>,
}

/// Builds the router: the API, the WebSocket upgrade, and static files.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/register", post(register))
        .route("/api/login", post(login))
        .route("/api/colors", put(put_colors))
        // axum 0.8 uses `{param}` where axum 0.7 and Express use `:param`.
        .route("/api/profile/{username}", get(profile))
        .route("/api/leaderboard", get(leaderboard))
        .route("/api/unlocks", get(unlocks))
        .route("/api/unlock/{feature}", post(unlock))
        .route("/api/credits", get(credits))
        .route("/api/credits/history", get(credits_history))
        .route("/api/credits/spend", post(credits_spend))
        .route("/api/solo-result", post(solo_result))
        .route("/api/trial-result", post(trial_result))
        .route("/api/campaign-result", post(campaign_result))
        .route("/ws", get(crate::ws::ws_route))
        .fallback(static_files)
        .with_state(state)
}

/// Serves `dist/`, falling back to `public/`.
///
/// The header rewrite reproduces the `express.static` options the JS passes:
/// `etag: false`, `lastModified: false`, and `Cache-Control: no-store`. The
/// comment there explains why — during dev iteration every reload should fetch
/// fresh assets so HTML/CSS edits show up without a hard refresh.
///
/// The charset fixup matches Express's `send`, which appends `; charset=UTF-8`
/// to any type `mime.charsets.lookup()` resolves to UTF-8 — every `text/*` plus
/// the JSON and JavaScript types. It matters because an HTTP `Content-Type`
/// charset **overrides** a document's `<meta charset>`, so omitting it is not
/// quite a no-op even though `dist/index.html` declares its own.
async fn static_files(State(state): State<AppState>, req: axum::extract::Request) -> Response {
    let mut res = match state.static_files.oneshot(req).await {
        Ok(res) => res.into_response(),
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    };
    let headers = res.headers_mut();
    headers.remove(header::ETAG);
    headers.remove(header::LAST_MODIFIED);
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Some(fixed) = headers
        .get(header::CONTENT_TYPE)
        .and_then(with_utf8_charset)
    {
        headers.insert(header::CONTENT_TYPE, fixed);
    }
    res
}

/// Appends `; charset=UTF-8` to a content type that should carry one and does
/// not already.
///
/// Returns `None` when nothing needs changing, so the caller can skip the
/// header write entirely.
fn with_utf8_charset(value: &HeaderValue) -> Option<HeaderValue> {
    let text = value.to_str().ok()?;
    if text.contains("charset") {
        return None;
    }
    let wants_charset = text.starts_with("text/")
        || text.starts_with("application/javascript")
        || text.starts_with("text/javascript")
        || text.starts_with("application/json")
        || text.starts_with("image/svg+xml");
    if !wants_charset {
        return None;
    }
    HeaderValue::from_str(&format!("{text}; charset=UTF-8")).ok()
}

// ─────────────────────────────────────────────────────────────────────────────
// Response envelopes
// ─────────────────────────────────────────────────────────────────────────────

/// `{ ok: false, error }` — the shape every failing route returns.
#[derive(Serialize)]
struct ErrBody {
    ok: bool,
    error: String,
    /// Only `/api/unlock/:feature` sets this, and only for 402.
    #[serde(skip_serializing_if = "Option::is_none")]
    balance: Option<i64>,
}

fn err(status: u16, message: impl Into<String>) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (
        code,
        Json(ErrBody {
            ok: false,
            error: message.into(),
            balance: None,
        }),
    )
        .into_response()
}

/// Applies the per-route `e.status ?? N` fallback.
fn err_with_default(e: &ApiError, default: u16) -> Response {
    // `ApiError` always carries a status, but the ones minted from a raw SQL
    // failure carry 500, which is exactly the value the `?? 500` routes want
    // and *not* what the `?? 401` routes produce. Re-map that case.
    let status = if e.status == 500 { default } else { e.status };
    err(status, e.message.clone())
}

fn ok_json<T: Serialize>(body: T) -> Response {
    Json(body).into_response()
}

/// `{ ok: true, ...payload }`.
#[derive(Serialize)]
struct OkSpread<T: Serialize> {
    ok: bool,
    #[serde(flatten)]
    payload: T,
}

fn ok_spread<T: Serialize>(payload: T) -> Response {
    ok_json(OkSpread { ok: true, payload })
}

// ─────────────────────────────────────────────────────────────────────────────
// JS coercion helpers
// ─────────────────────────────────────────────────────────────────────────────

/// `req.body ?? {}` — anything that is not a JSON object reads as empty.
fn body_object(bytes: &Bytes) -> Value {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(v @ Value::Object(_)) => v,
        _ => Value::Object(serde_json::Map::new()),
    }
}

/// JavaScript's `String(x)` for the value kinds a JSON body can hold.
fn js_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// JavaScript truthiness.
fn js_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// JavaScript's `Number(x)`. `NaN` for anything non-numeric, which every caller
/// then funnels through `|| 0` or `Number.isFinite`.
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
        Some(_) => f64::NAN,
    }
}

/// `Number(x) || 0`.
fn js_number_or_zero(v: Option<&Value>) -> f64 {
    let n = js_number(v);
    if n == 0.0 || n.is_nan() {
        0.0
    } else {
        n
    }
}

/// The bearer-token half of `extractPilotId`.
fn bearer(headers: &axum::http::HeaderMap) -> Result<&str, ApiError> {
    let raw = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let token = raw.strip_prefix("Bearer ").unwrap_or("");
    if token.is_empty() {
        return Err(ApiError::new(401, "Not authenticated"));
    }
    Ok(token)
}

/// `extractPilotId` — bearer token to pilot id, or a 401.
fn pilot_id(state: &AppState, headers: &axum::http::HeaderMap) -> Result<i64, ApiError> {
    let token = bearer(headers)?;
    Ok(state.auth.verify(token)?.id)
}

/// Runs a blocking database closure on the blocking pool.
async fn blocking<T, F>(f: F) -> Result<T, ApiError>
where
    F: FnOnce() -> Result<T, ApiError> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => r,
        Err(e) => Err(ApiError::new(500, e.to_string())),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Auth routes
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct RegisterOk {
    ok: bool,
    username: String,
}

/// `POST /api/register` — 201 on success.
async fn register(State(state): State<AppState>, body: Bytes) -> Response {
    let body = body_object(&body);
    let username = body.get("username").map(js_string).unwrap_or_default();
    let password_raw = body.get("password");

    // `registerPilot`: strip everything outside `[A-Za-z0-9_-]` — note that,
    // unlike the WebSocket `name` sanitizer, a space is **not** allowed here.
    let clean: String = username
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    let clean = clean.trim().to_string();
    if clean.chars().count() < 3 || clean.chars().count() > 20 {
        return err(400, "Callsign must be 3\u{2013}20 alphanumeric characters");
    }
    let password_ok = password_raw.is_some_and(js_truthy)
        && password_raw
            .map(js_string)
            .unwrap_or_default()
            .chars()
            .count()
            >= 6;
    if !password_ok {
        return err(400, "Password must be at least 6 characters");
    }
    let password = password_raw.map(js_string).unwrap_or_default();

    let db = Arc::clone(&state.db);
    let clean2 = clean.clone();
    let result = blocking(move || {
        if db.pilot_by_username(&clean2)?.is_some() {
            return Err(ApiError::new(409, "Callsign already taken"));
        }
        let hash = hash_password(&password)?;
        db.insert_pilot(&clean2, &hash)?;
        Ok(())
    })
    .await;

    match result {
        Ok(()) => (
            StatusCode::CREATED,
            Json(RegisterOk {
                ok: true,
                username: clean,
            }),
        )
            .into_response(),
        Err(e) => err_with_default(&e, 500),
    }
}

/// `POST /api/login`.
async fn login(State(state): State<AppState>, body: Bytes) -> Response {
    let body = body_object(&body);
    let username = body.get("username").map(js_string).unwrap_or_default();
    let password = body.get("password").map(js_string).unwrap_or_default();

    let db = Arc::clone(&state.db);
    let auth = Arc::clone(&state.auth);
    let result = blocking(move || {
        let pilot = db.pilot_by_username(&username)?;
        // Compare against a dummy hash for an unknown callsign so the response
        // time does not reveal whether it exists.
        let ok = match &pilot {
            Some(p) => verify_password(&password, &p.hashed_password),
            None => verify_against_dummy(&password),
        };
        let Some(pilot) = pilot else {
            return Err(ApiError::new(401, "Invalid callsign or password"));
        };
        if !ok {
            return Err(ApiError::new(401, "Invalid callsign or password"));
        }
        let token = auth.sign(pilot.id, &pilot.username)?;
        Ok(login_result(&pilot, token))
    })
    .await;

    match result {
        Ok(r) => ok_spread::<LoginResult>(r),
        Err(e) => err_with_default(&e, 500),
    }
}

#[derive(Serialize)]
struct JustOk {
    ok: bool,
}

/// `PUT /api/colors` — bearer auth, catches with `?? 401`.
async fn put_colors(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Response {
    let id = match pilot_id(&state, &headers) {
        Ok(id) => id,
        Err(e) => return err_with_default(&e, 401),
    };
    let body = body_object(&body);
    let ship = body.get("shipColor").map(js_string).unwrap_or_default();
    let accent = body.get("accentColor").map(js_string).unwrap_or_default();
    let db = Arc::clone(&state.db);
    match blocking(move || db.save_pilot_colors(id, &ship, &accent)).await {
        Ok(()) => ok_json(JustOk { ok: true }),
        Err(e) => err_with_default(&e, 401),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Public reads
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ProfileBody {
    ok: bool,
    profile: ProfileView,
}

/// `GET /api/profile/:username` — public; 404 for an unknown callsign.
async fn profile(State(state): State<AppState>, Path(username): Path<String>) -> Response {
    let db = Arc::clone(&state.db);
    match blocking(move || db.pilot_profile(&username)).await {
        Ok(Some(profile)) => ok_json(ProfileBody { ok: true, profile }),
        Ok(None) => err(404, "Pilot not found"),
        // This route's catch has no `?? status`, so everything unexpected is a
        // flat 500.
        Err(e) => err(500, e.message),
    }
}

#[derive(Serialize)]
struct LeaderboardBody {
    ok: bool,
    leaderboard: Vec<LeaderboardRow>,
}

/// `GET /api/leaderboard`.
async fn leaderboard(State(state): State<AppState>) -> Response {
    let db = Arc::clone(&state.db);
    match blocking(move || db.leaderboard()).await {
        Ok(rows) => ok_json(LeaderboardBody {
            ok: true,
            leaderboard: rows,
        }),
        Err(e) => err(500, e.message),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Unlocks and credits
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct UnlocksBody {
    ok: bool,
    costs: UnlockCosts,
    #[serde(flatten)]
    owned: Unlocks,
}

/// `GET /api/unlocks` — the cost table plus what this pilot owns.
async fn unlocks(State(state): State<AppState>, headers: axum::http::HeaderMap) -> Response {
    let id = match pilot_id(&state, &headers) {
        Ok(id) => id,
        Err(e) => return err_with_default(&e, 401),
    };
    let db = Arc::clone(&state.db);
    match blocking(move || db.unlocks(id)).await {
        Ok(owned) => ok_json(UnlocksBody {
            ok: true,
            costs: UNLOCK_COSTS_STRUCT,
            owned,
        }),
        Err(e) => err_with_default(&e, 401),
    }
}

#[derive(Serialize)]
struct UnlockOk {
    ok: bool,
    #[serde(rename = "alreadyOwned")]
    already_owned: bool,
    balance: i64,
    #[serde(rename = "newAchievements")]
    new_achievements: Vec<EarnedAchievement>,
}

/// `POST /api/unlock/:feature`.
///
/// 400 for an unknown feature (no `balance` key at all — the JS branches on
/// `result.balance !== undefined`), 402 when the pilot cannot afford it.
async fn unlock(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(feature): Path<String>,
) -> Response {
    let id = match pilot_id(&state, &headers) {
        Ok(id) => id,
        Err(e) => return err_with_default(&e, 401),
    };
    let db = Arc::clone(&state.db);
    let outcome = match blocking(move || db.purchase_unlock(id, &feature)).await {
        Ok(o) => o,
        Err(e) => return err_with_default(&e, 401),
    };
    match outcome {
        PurchaseOutcome::UnknownFeature => (
            StatusCode::BAD_REQUEST,
            Json(ErrBody {
                ok: false,
                error: "Unknown feature".to_string(),
                balance: None,
            }),
        )
            .into_response(),
        PurchaseOutcome::Insufficient { balance } => (
            StatusCode::PAYMENT_REQUIRED,
            Json(ErrBody {
                ok: false,
                error: "Insufficient credits".to_string(),
                balance: Some(balance),
            }),
        )
            .into_response(),
        PurchaseOutcome::AlreadyOwned { balance } => ok_json(UnlockOk {
            ok: true,
            already_owned: true,
            balance,
            // `result.newAchievements ?? []` — the already-owned branch never
            // sets the key.
            new_achievements: Vec::new(),
        }),
        PurchaseOutcome::Bought {
            balance,
            new_achievements,
        } => ok_json(UnlockOk {
            ok: true,
            already_owned: false,
            balance,
            new_achievements,
        }),
    }
}

#[derive(Serialize)]
struct CreditsBody {
    ok: bool,
    credits: i64,
}

/// `GET /api/credits`.
async fn credits(State(state): State<AppState>, headers: axum::http::HeaderMap) -> Response {
    let id = match pilot_id(&state, &headers) {
        Ok(id) => id,
        Err(e) => return err_with_default(&e, 401),
    };
    let db = Arc::clone(&state.db);
    match blocking(move || db.get_credits(id)).await {
        Ok(credits) => ok_json(CreditsBody { ok: true, credits }),
        Err(e) => err_with_default(&e, 401),
    }
}

#[derive(Deserialize)]
struct HistoryQuery {
    limit: Option<String>,
}

#[derive(Serialize)]
struct HistoryBody {
    ok: bool,
    history: Vec<CreditTx>,
}

/// `GET /api/credits/history` — `limit` clamped to `1..=100`, default 50.
async fn credits_history(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<HistoryQuery>,
) -> Response {
    let id = match pilot_id(&state, &headers) {
        Ok(id) => id,
        Err(e) => return err_with_default(&e, 401),
    };
    // `Math.min(100, Math.max(1, Number(req.query.limit) || 50))`. A missing or
    // unparseable limit is NaN, which `|| 50` turns into 50.
    let raw = q.limit.map(Value::String);
    let n = js_number_or_zero(raw.as_ref());
    let n = if n == 0.0 { 50.0 } else { n };
    let limit = n.clamp(1.0, 100.0) as i64;
    let db = Arc::clone(&state.db);
    match blocking(move || db.credit_history(id, limit)).await {
        Ok(history) => ok_json(HistoryBody { ok: true, history }),
        Err(e) => err_with_default(&e, 401),
    }
}

#[derive(Serialize)]
struct BalanceBody {
    ok: bool,
    balance: i64,
}

/// `POST /api/credits/spend`.
async fn credits_spend(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Response {
    let id = match pilot_id(&state, &headers) {
        Ok(id) => id,
        Err(e) => return err_with_default(&e, 401),
    };
    let body = body_object(&body);
    let amount = js_number_or_zero(body.get("amount"));
    let reason_raw = body.get("reason");
    let reason: String = if reason_raw.is_some_and(js_truthy) {
        reason_raw.map(js_string).unwrap_or_default()
    } else {
        "purchase".to_string()
    };
    let reason: String = reason.chars().take(120).collect();
    if amount < 1.0 {
        return err(400, "Invalid amount");
    }
    let db = Arc::clone(&state.db);
    let amount = amount as i64;
    match blocking(move || db.spend_credits(id, amount, &reason)).await {
        Ok(Ok(balance)) => ok_json(BalanceBody { ok: true, balance }),
        Ok(Err(balance)) => (
            StatusCode::PAYMENT_REQUIRED,
            Json(ErrBody {
                ok: false,
                error: "Insufficient credits".to_string(),
                balance: Some(balance),
            }),
        )
            .into_response(),
        Err(e) => err_with_default(&e, 401),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Result recording
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ResultBody {
    ok: bool,
    #[serde(rename = "newAchievements")]
    new_achievements: Vec<EarnedAchievement>,
    #[serde(rename = "creditsEarned")]
    credits_earned: i64,
    #[serde(rename = "totalCredits")]
    total_credits: i64,
}

/// `POST /api/solo-result`.
async fn solo_result(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Response {
    let id = match pilot_id(&state, &headers) {
        Ok(id) => id,
        Err(e) => return err_with_default(&e, 500),
    };
    let body = body_object(&body);
    // The JS destructures with defaults, then re-clamps: `Math.max(0, Number(x)
    // || 0)`. A missing key and a garbage key both end up at 0.
    let kills = js_number_or_zero(body.get("kills")).max(0.0) as i64;
    let deaths = js_number_or_zero(body.get("deaths")).max(0.0) as i64;
    let bots = js_number_or_zero(body.get("botsKilled")).max(0.0) as i64;
    // `won === true || won === false ? won : null` — only a real boolean counts.
    let won = match body.get("won") {
        Some(Value::Bool(b)) => Some(*b),
        _ => None,
    };
    let db = Arc::clone(&state.db);
    let out = blocking(move || {
        let outcome = db.record_match_result(id, kills, deaths, won, bots)?;
        let total = db.get_credits(id)?;
        Ok((outcome, total))
    })
    .await;
    match out {
        Ok((outcome, total)) => ok_json(ResultBody {
            ok: true,
            new_achievements: outcome.new_achievements,
            credits_earned: outcome.credits_earned,
            total_credits: total,
        }),
        Err(e) => err_with_default(&e, 500),
    }
}

/// `POST /api/trial-result` — trials 1–4, positive finite time.
async fn trial_result(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Response {
    let id = match pilot_id(&state, &headers) {
        Ok(id) => id,
        Err(e) => return err_with_default(&e, 500),
    };
    let body = body_object(&body);
    let num = js_number(body.get("trialNum"));
    let t = js_number(body.get("time"));
    if !(1.0..=4.0).contains(&num) || num.fract() != 0.0 || !t.is_finite() || t <= 0.0 {
        return err(400, "Invalid trial data");
    }
    let db = Arc::clone(&state.db);
    let num = num as i64;
    let out = blocking(move || {
        let outcome = db.record_trial_time(id, num, t)?;
        let total = db.get_credits(id)?;
        Ok((outcome, total))
    })
    .await;
    match out {
        Ok((outcome, total)) => ok_json(ResultBody {
            ok: true,
            new_achievements: outcome.new_achievements,
            credits_earned: outcome.credits_earned,
            total_credits: total,
        }),
        Err(e) => err_with_default(&e, 500),
    }
}

/// `POST /api/campaign-result` — missions 1–3, `livesRemaining` in `0..=3`.
async fn campaign_result(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Response {
    let id = match pilot_id(&state, &headers) {
        Ok(id) => id,
        Err(e) => return err_with_default(&e, 500),
    };
    let body = body_object(&body);
    let num = js_number(body.get("missionNum"));
    let lives = js_number(body.get("livesRemaining"));
    if !(1.0..=3.0).contains(&num)
        || num.fract() != 0.0
        || !lives.is_finite()
        || !(0.0..=3.0).contains(&lives)
    {
        return err(400, "Invalid campaign data");
    }
    let db = Arc::clone(&state.db);
    let num = num as i64;
    // `Math.round` — ties go to +Infinity in JS, which is `f64::round`'s
    // behaviour only for positive values; `lives` is already clamped to 0..=3.
    let lives = lives.round() as i64;
    let out = blocking(move || {
        let outcome = db.record_campaign_result(id, num, lives)?;
        let total = db.get_credits(id)?;
        Ok((outcome, total))
    })
    .await;
    match out {
        Ok((outcome, total)) => ok_json(ResultBody {
            ok: true,
            new_achievements: outcome.new_achievements,
            credits_earned: outcome.credits_earned,
            total_credits: total,
        }),
        Err(e) => err_with_default(&e, 500),
    }
}

/// Builds the `dist/` → `public/` static chain.
#[must_use]
pub fn static_service(dist: &std::path::Path, public: &std::path::Path) -> ServeDir<ServeDir> {
    ServeDir::new(dist).fallback(ServeDir::new(public))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_number_matches_the_coercion_table() {
        assert!(js_number(None).is_nan());
        assert_eq!(js_number(Some(&Value::Null)), 0.0);
        assert_eq!(js_number(Some(&Value::Bool(true))), 1.0);
        assert_eq!(js_number(Some(&Value::from(7))), 7.0);
        assert_eq!(js_number(Some(&Value::String("7".into()))), 7.0);
        assert_eq!(js_number(Some(&Value::String("".into()))), 0.0);
        assert!(js_number(Some(&Value::String("abc".into()))).is_nan());
    }

    #[test]
    fn number_or_zero_folds_nan_and_zero_together() {
        assert_eq!(js_number_or_zero(None), 0.0);
        assert_eq!(js_number_or_zero(Some(&Value::String("abc".into()))), 0.0);
        assert_eq!(js_number_or_zero(Some(&Value::from(3))), 3.0);
    }

    #[test]
    fn a_missing_or_broken_body_reads_as_an_empty_object() {
        assert_eq!(
            body_object(&Bytes::from_static(b"")),
            Value::Object(Default::default())
        );
        assert_eq!(
            body_object(&Bytes::from_static(b"[]")),
            Value::Object(Default::default())
        );
        assert_eq!(
            body_object(&Bytes::from_static(b"{oops")),
            Value::Object(Default::default())
        );
        assert!(body_object(&Bytes::from_static(br#"{"a":1}"#))
            .get("a")
            .is_some());
    }

    #[test]
    fn the_error_envelope_omits_balance_unless_set() {
        let s = serde_json::to_string(&ErrBody {
            ok: false,
            error: "Unknown feature".into(),
            balance: None,
        })
        .unwrap();
        assert_eq!(s, r#"{"ok":false,"error":"Unknown feature"}"#);
    }

    #[test]
    fn ok_spread_keeps_ok_first() {
        #[derive(Serialize)]
        struct P {
            a: i32,
            b: i32,
        }
        let s = serde_json::to_string(&OkSpread {
            ok: true,
            payload: P { a: 1, b: 2 },
        })
        .unwrap();
        assert_eq!(s, r#"{"ok":true,"a":1,"b":2}"#);
    }
}
