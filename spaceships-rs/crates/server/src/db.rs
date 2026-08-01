//! SQLite persistence — the port of `server/db.js`.
//!
//! # The file this talks to is live
//!
//! `pilots.db` holds real accounts with real bcrypt hashes and real credit
//! balances, and the Node server keeps serving from it until this binary
//! replaces it. Everything here is therefore *additive and compatible*: the
//! schema statements are the same `CREATE TABLE IF NOT EXISTS` plus the same
//! idempotent `ALTER TABLE ... ADD COLUMN` list the JS runs, in the same order,
//! so opening an existing database is a no-op and opening a fresh one produces
//! a byte-compatible schema. No column is renamed, retyped, dropped, or
//! reordered.
//!
//! The database path comes from `PILOTS_DB`, defaulting to `pilots.db` in the
//! working directory (which is where `path.join(__dirname, '..', 'pilots.db')`
//! resolves when the JS server is started from the repo root). Tests always set
//! it to a copy.
//!
//! # Autocommit, deliberately
//!
//! `better-sqlite3` runs each prepared statement in its own implicit
//! transaction unless the caller opens one, and `server/db.js` never opens one
//! — `recordMatchResult` updates the pilot row, inserts a credit transaction,
//! updates the rank and awards achievements as four-plus separate commits.
//! This module does the same. Wrapping them would be an improvement, but it
//! would also be a silent behavioural change to a system whose failure mode
//! (a crash between two writes) has never been observed, so it is left alone
//! and noted here instead.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::achievements::{def_for, PilotStats, Progress, ACHIEVEMENT_DEFS};
use crate::jsfmt::{kdr_str, serialize_opt_js_f64, JsNum};

// ─────────────────────────────────────────────────────────────────────────────
// Errors
// ─────────────────────────────────────────────────────────────────────────────

/// An error with the HTTP status the JS attaches via
/// `Object.assign(new Error(m), { status })`.
#[derive(Debug, Clone)]
pub struct ApiError {
    /// HTTP status code to respond with.
    pub status: u16,
    /// `error` field of the JSON body, verbatim.
    pub message: String,
}

impl ApiError {
    /// Builds an error carrying an explicit status.
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        ApiError {
            status,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

impl From<rusqlite::Error> for ApiError {
    /// A SQL failure is a 500 with the driver's message, which is how an
    /// unexpected `better-sqlite3` throw surfaces through the Express
    /// `catch (e) { res.status(e.status ?? 500) }` arms.
    fn from(e: rusqlite::Error) -> Self {
        ApiError::new(500, e.to_string())
    }
}

/// Result alias for database operations.
pub type Result<T> = std::result::Result<T, ApiError>;

// ─────────────────────────────────────────────────────────────────────────────
// Credit rates and unlock costs
// ─────────────────────────────────────────────────────────────────────────────

/// Credits per player kill. `server/db.js` `CR_PER_KILL`.
pub const CR_PER_KILL: i64 = 5;
/// Credits per bot kill in solo modes. `CR_PER_BOT_KILL`.
pub const CR_PER_BOT_KILL: i64 = 2;
/// Flat bonus for winning a match. `CR_WIN_BONUS`.
pub const CR_WIN_BONUS: i64 = 50;

/// Credits paid for completing campaign missions 1, 2 and 3.
const CAMPAIGN_MISSION_CREDITS: [i64; 3] = [500, 1000, 2000];

/// `UNLOCK_COSTS`, in the JS object's key order — the `/api/unlocks` response
/// embeds this object verbatim and the shop renders it in order.
pub const UNLOCK_COSTS: [(&str, i64); 5] = [
    ("hull", 250),
    ("accent", 400),
    ("trail", 500),
    ("trail_shape", 200),
    ("admin_ship", 125_000),
];

/// The cost table as a serializable map that preserves the JS key order.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct UnlockCosts {
    /// Hull colour customization.
    pub hull: i64,
    /// Accent colour customization.
    pub accent: i64,
    /// Engine trail colour.
    pub trail: i64,
    /// Engine trail shape.
    pub trail_shape: i64,
    /// The admin ship model.
    pub admin_ship: i64,
}

/// `UNLOCK_COSTS` as the struct the API serializes.
pub const UNLOCK_COSTS_STRUCT: UnlockCosts = UnlockCosts {
    hull: 250,
    accent: 400,
    trail: 500,
    trail_shape: 200,
    admin_ship: 125_000,
};

// ─────────────────────────────────────────────────────────────────────────────
// Rank thresholds
// ─────────────────────────────────────────────────────────────────────────────

/// `computeRank` — rank title for a lifetime kill count.
///
/// Note the table is not monotonic in the way the names suggest: `Admiral` sits
/// at 500 kills, *below* `Captain` at 750 and `Commodore` at 1000. That is what
/// `server/db.js` does, and pilots already carry those titles in the `rank`
/// column, so it is preserved exactly rather than "fixed".
#[must_use]
pub fn compute_rank(total_kills: i64) -> &'static str {
    match total_kills {
        k if k >= 10000 => "Grand Admiral",
        k if k >= 5000 => "Fleet Admiral",
        k if k >= 2500 => "Vice Admiral",
        k if k >= 1500 => "Rear Admiral",
        k if k >= 1000 => "Commodore",
        k if k >= 750 => "Captain",
        k if k >= 500 => "Admiral",
        k if k >= 250 => "Commander",
        k if k >= 100 => "Veteran",
        k if k >= 50 => "Ace",
        k if k >= 25 => "Flight Officer",
        k if k >= 10 => "Pilot",
        k if k >= 5 => "Recruit",
        _ => "Cadet",
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Row types
// ─────────────────────────────────────────────────────────────────────────────

/// A full `pilots` row.
#[derive(Debug, Clone)]
pub struct Pilot {
    /// Primary key.
    pub id: i64,
    /// Callsign, as stored (the column is `COLLATE NOCASE`, so lookups are
    /// case-insensitive but the stored casing is preserved).
    pub username: String,
    /// bcrypt hash.
    pub hashed_password: String,
    /// Rank title.
    pub rank: String,
    /// Best single-match kill count.
    pub high_score: i64,
    /// Matches played.
    pub games_played: i64,
    /// Unix seconds.
    pub created_at: i64,
    /// Saved hull colour, `#rrggbb`.
    pub ship_color: String,
    /// Saved accent colour, `#rrggbb`.
    pub ship_accent_color: String,
    /// Stat columns the achievement checks read.
    pub stats: PilotStats,
    /// Credit balance.
    pub credits: i64,
}

/// The column list every `SELECT * FROM pilots` in the JS effectively reads,
/// spelled out so a future `ALTER TABLE` cannot silently shift positions.
const PILOT_COLUMNS: &str = "id, username, hashed_password, rank, high_score, games_played, \
     created_at, ship_color, ship_accent_color, total_kills, total_deaths, matches_won, \
     matches_lost, bots_killed, trial1_best, trial2_best, trial3_best, trial4_best, credits, \
     unlock_hull, unlock_accent, unlock_trail, unlock_trail_shape, unlock_admin_ship, \
     campaign1_best_lives, campaign2_best_lives, campaign3_best_lives, campaign_boss_kills, \
     campaign_total_completions";

fn map_pilot(row: &rusqlite::Row<'_>) -> rusqlite::Result<Pilot> {
    Ok(Pilot {
        id: row.get(0)?,
        username: row.get(1)?,
        hashed_password: row.get(2)?,
        rank: row.get(3)?,
        high_score: row.get(4)?,
        games_played: row.get(5)?,
        created_at: row.get(6)?,
        ship_color: row.get(7)?,
        ship_accent_color: row.get(8)?,
        stats: PilotStats {
            total_kills: row.get(9)?,
            total_deaths: row.get(10)?,
            matches_won: row.get(11)?,
            matches_lost: row.get(12)?,
            bots_killed: row.get(13)?,
            trial_best: [row.get(14)?, row.get(15)?, row.get(16)?, row.get(17)?],
            high_score: row.get(4)?,
            games_played: row.get(5)?,
            unlock_hull: row.get::<_, i64>(19)? != 0,
            unlock_accent: row.get::<_, i64>(20)? != 0,
            unlock_trail: row.get::<_, i64>(21)? != 0,
            unlock_trail_shape: row.get::<_, i64>(22)? != 0,
            unlock_admin_ship: row.get::<_, i64>(23)? != 0,
            campaign_best_lives: [row.get(24)?, row.get(25)?, row.get(26)?],
            campaign_boss_kills: row.get(27)?,
            campaign_total_completions: row.get(28)?,
        },
        credits: row.get(18)?,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// API response shapes
// ─────────────────────────────────────────────────────────────────────────────

/// The object `loginPilot` resolves to, spread into the `/api/login` response.
///
/// Field order matches the JS object literal, because that is the order
/// `JSON.stringify` emits and the response is compared byte-for-byte in tests.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginResult {
    /// Signed JWT, valid 7 days.
    pub token: String,
    /// Callsign.
    pub username: String,
    /// Rank title.
    pub rank: String,
    /// Best single-match kills.
    pub high_score: i64,
    /// Matches played.
    pub games_played: i64,
    /// Hull colour, defaulted when the column is empty.
    pub ship_color: String,
    /// Accent colour, defaulted when the column is empty.
    pub accent_color: String,
    /// Lifetime kills.
    pub total_kills: i64,
    /// Lifetime deaths.
    pub total_deaths: i64,
    /// Matches won.
    pub matches_won: i64,
    /// Matches lost.
    pub matches_lost: i64,
    /// Bots destroyed.
    pub bots_killed: i64,
    /// Kill/death ratio as a two-decimal string.
    pub kdr: String,
    /// Best trial 1 time, or `null`.
    #[serde(serialize_with = "serialize_opt_js_f64")]
    pub trial1_best: Option<f64>,
    /// Best trial 2 time, or `null`.
    #[serde(serialize_with = "serialize_opt_js_f64")]
    pub trial2_best: Option<f64>,
    /// Best trial 3 time, or `null`.
    #[serde(serialize_with = "serialize_opt_js_f64")]
    pub trial3_best: Option<f64>,
    /// Best trial 4 time, or `null`.
    #[serde(serialize_with = "serialize_opt_js_f64")]
    pub trial4_best: Option<f64>,
    /// Credit balance.
    pub credits: i64,
    /// Hull customization owned.
    pub unlock_hull: bool,
    /// Accent customization owned.
    pub unlock_accent: bool,
    /// Trail colour owned.
    pub unlock_trail: bool,
    /// Trail shape owned.
    pub unlock_trail_shape: bool,
    /// Admin ship owned.
    pub unlock_admin_ship: bool,
    /// Best lives remaining in mission 1, or `null`.
    pub campaign1_best_lives: Option<i64>,
    /// Best lives remaining in mission 2, or `null`.
    pub campaign2_best_lives: Option<i64>,
    /// Best lives remaining in mission 3, or `null`.
    pub campaign3_best_lives: Option<i64>,
    /// Capital ships destroyed.
    pub campaign_boss_kills: i64,
    /// Campaign missions completed, all-time.
    pub campaign_total_completions: i64,
}

/// One row of the `achievements` array in a profile.
#[derive(Debug, Clone, Serialize)]
pub struct AchievementView {
    /// Stable identifier.
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// Display title.
    pub label: &'static str,
    /// Emoji.
    pub icon: &'static str,
    /// Description.
    pub desc: &'static str,
    /// Whether this pilot has it.
    pub earned: bool,
    /// Unix seconds it was earned, or `null`.
    #[serde(rename = "earnedAt")]
    pub earned_at: Option<i64>,
    /// Progress bar data for unearned achievements, else `null`.
    pub progress: Option<Progress>,
}

/// The `profile` object returned by `/api/profile/:username`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileView {
    /// Callsign.
    pub username: String,
    /// Rank title.
    pub rank: String,
    /// Best single-match kills.
    pub high_score: i64,
    /// Matches played.
    pub games_played: i64,
    /// Lifetime kills.
    pub total_kills: i64,
    /// Lifetime deaths.
    pub total_deaths: i64,
    /// Matches won.
    pub matches_won: i64,
    /// Matches lost.
    pub matches_lost: i64,
    /// Bots destroyed.
    pub bots_killed: i64,
    /// Kill/death ratio as a two-decimal string.
    pub kdr: String,
    /// Best trial 1 time, or `null`.
    #[serde(serialize_with = "serialize_opt_js_f64")]
    pub trial1_best: Option<f64>,
    /// Best trial 2 time, or `null`.
    #[serde(serialize_with = "serialize_opt_js_f64")]
    pub trial2_best: Option<f64>,
    /// Best trial 3 time, or `null`.
    #[serde(serialize_with = "serialize_opt_js_f64")]
    pub trial3_best: Option<f64>,
    /// Best trial 4 time, or `null`.
    #[serde(serialize_with = "serialize_opt_js_f64")]
    pub trial4_best: Option<f64>,
    /// Every achievement, earned or not, in table order.
    pub achievements: Vec<AchievementView>,
    /// Credit balance.
    pub credits: i64,
    /// Hull customization owned.
    pub unlock_hull: bool,
    /// Accent customization owned.
    pub unlock_accent: bool,
    /// Trail colour owned.
    pub unlock_trail: bool,
    /// Trail shape owned.
    pub unlock_trail_shape: bool,
    /// Admin ship owned.
    pub unlock_admin_ship: bool,
    /// Best lives remaining in mission 1, or `null`.
    pub campaign1_best_lives: Option<i64>,
    /// Best lives remaining in mission 2, or `null`.
    pub campaign2_best_lives: Option<i64>,
    /// Best lives remaining in mission 3, or `null`.
    pub campaign3_best_lives: Option<i64>,
    /// Capital ships destroyed.
    pub campaign_boss_kills: i64,
    /// Campaign missions completed, all-time.
    pub campaign_total_completions: i64,
    /// Registration time, unix seconds.
    pub created_at: i64,
}

/// One row of the leaderboard.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaderboardRow {
    /// 1-based position.
    pub position: usize,
    /// Callsign.
    pub username: String,
    /// Rank title. Renamed from `rank` to `pilotRank` by the JS mapper, because
    /// the table already uses "rank" for the leaderboard position.
    pub pilot_rank: String,
    /// Lifetime kills.
    pub total_kills: i64,
    /// Lifetime deaths.
    pub total_deaths: i64,
    /// Matches won.
    pub matches_won: i64,
    /// Matches lost.
    pub matches_lost: i64,
    /// Matches played.
    pub games_played: i64,
    /// Best single-match kills.
    pub high_score: i64,
    /// Kill/death ratio as a two-decimal string.
    pub kdr: String,
}

/// One row of `/api/credits/history`.
///
/// The keys are the **raw column names**: the JS returns `better-sqlite3` row
/// objects straight from `SELECT amount, reason, created_at`, so `created_at`
/// reaches the browser in snake_case while every other endpoint is camelCase.
#[derive(Debug, Clone, Serialize)]
pub struct CreditTx {
    /// Signed credit delta; negative for spends.
    pub amount: i64,
    /// Free-text reason, e.g. `match:kills(3),win_bonus` or `unlock:hull`.
    pub reason: String,
    /// Unix seconds.
    pub created_at: i64,
}

/// Which customization features a pilot owns.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Unlocks {
    /// Hull colour.
    pub unlock_hull: bool,
    /// Accent colour.
    pub unlock_accent: bool,
    /// Trail colour.
    pub unlock_trail: bool,
    /// Trail shape.
    pub unlock_trail_shape: bool,
    /// Admin ship model.
    pub unlock_admin_ship: bool,
}

/// An achievement that was just unlocked, as the API and the `match-credits`
/// WebSocket message carry it.
#[derive(Debug, Clone, Serialize)]
pub struct EarnedAchievement {
    /// Stable identifier.
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// Display title.
    pub label: &'static str,
    /// Emoji.
    pub icon: &'static str,
    /// Credits paid.
    pub reward: i64,
}

/// The `{ newAchievements, creditsEarned }` pair every result recorder returns.
#[derive(Debug, Clone, Default)]
pub struct RecordOutcome {
    /// Achievements unlocked by this result.
    pub new_achievements: Vec<EarnedAchievement>,
    /// Credits paid out, match/mission rewards plus achievement rewards.
    pub credits_earned: i64,
}

/// Outcome of `purchaseUnlock`.
#[derive(Debug, Clone)]
pub enum PurchaseOutcome {
    /// The feature was already owned; nothing was charged.
    AlreadyOwned {
        /// Unchanged balance.
        balance: i64,
    },
    /// The feature was bought.
    Bought {
        /// Balance after the debit.
        balance: i64,
        /// Achievements the purchase unlocked (`high_roller`).
        new_achievements: Vec<EarnedAchievement>,
    },
    /// Not enough credits. Answered with HTTP 402.
    Insufficient {
        /// Current balance, echoed back so the shop can show the shortfall.
        balance: i64,
    },
    /// No such feature. Answered with HTTP 400 and no balance.
    UnknownFeature,
}

// ─────────────────────────────────────────────────────────────────────────────
// The database
// ─────────────────────────────────────────────────────────────────────────────

/// A handle to `pilots.db`.
///
/// `rusqlite::Connection` is `Send` but not `Sync`, and the JS server is
/// single-threaded anyway, so one mutex-guarded connection reproduces its
/// serialization exactly. Every call is short; the HTTP layer runs them on the
/// blocking pool.
pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    /// Opens (or creates) the database and brings the schema up to date.
    ///
    /// Runs the same `CREATE TABLE IF NOT EXISTS` statements and the same
    /// ignore-on-failure `ALTER TABLE` migration list as `server/db.js`, then
    /// the same startup backfill.
    pub fn open(path: impl AsRef<Path>) -> Result<Db> {
        let conn = Connection::open(path)?;
        let db = Db {
            conn: Mutex::new(conn),
        };
        db.migrate()?;
        db.backfill_achievements()?;
        Ok(db)
    }

    /// Opens an in-memory database with the current schema, for tests.
    pub fn open_in_memory() -> Result<Db> {
        let db = Db {
            conn: Mutex::new(Connection::open_in_memory()?),
        };
        db.migrate()?;
        Ok(db)
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let guard = self
            .conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&guard)
    }

    // ── Schema ──────────────────────────────────────────────────────────────

    fn migrate(&self) -> Result<()> {
        self.with(|c| {
            c.execute_batch(
                "CREATE TABLE IF NOT EXISTS pilots (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    username       TEXT    NOT NULL UNIQUE COLLATE NOCASE,
    hashed_password TEXT   NOT NULL,
    rank           TEXT    NOT NULL DEFAULT 'Cadet',
    high_score     INTEGER NOT NULL DEFAULT 0,
    games_played   INTEGER NOT NULL DEFAULT 0,
    created_at     INTEGER NOT NULL DEFAULT (unixepoch())
  )",
            )?;

            // Idempotent migrations — same list, same order as `server/db.js`.
            // Each is expected to fail with "duplicate column name" on an
            // existing database; the JS swallows that with a bare `catch {}`
            // and so does this.
            for sql in MIGRATIONS {
                let _ = c.execute(sql, []);
            }

            c.execute_batch(
                "CREATE TABLE IF NOT EXISTS achievements (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    pilot_id  INTEGER NOT NULL REFERENCES pilots(id),
    type      TEXT NOT NULL,
    earned_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE(pilot_id, type)
  )",
            )?;
            let _ = c.execute(
                "ALTER TABLE achievements ADD COLUMN credited INTEGER NOT NULL DEFAULT 0",
                [],
            );

            c.execute_batch(
                "CREATE TABLE IF NOT EXISTS credit_transactions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    pilot_id   INTEGER NOT NULL REFERENCES pilots(id),
    amount     INTEGER NOT NULL,
    reason     TEXT    NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
  )",
            )?;
            Ok(())
        })
    }

    // ── Lookups ─────────────────────────────────────────────────────────────

    /// `stmtByUsername` — case-insensitive thanks to the column's `COLLATE
    /// NOCASE`.
    pub fn pilot_by_username(&self, username: &str) -> Result<Option<Pilot>> {
        self.with(|c| {
            let sql = format!("SELECT {PILOT_COLUMNS} FROM pilots WHERE username = ?");
            Ok(c.prepare_cached(&sql)?
                .query_row(params![username], map_pilot)
                .optional()?)
        })
    }

    /// `stmtById`.
    pub fn pilot_by_id(&self, id: i64) -> Result<Option<Pilot>> {
        self.with(|c| {
            let sql = format!("SELECT {PILOT_COLUMNS} FROM pilots WHERE id = ?");
            Ok(c.prepare_cached(&sql)?
                .query_row(params![id], map_pilot)
                .optional()?)
        })
    }

    /// `stmtInsert` — returns the new pilot's rowid.
    pub fn insert_pilot(&self, username: &str, hash: &str) -> Result<i64> {
        self.with(|c| {
            c.prepare_cached("INSERT INTO pilots (username, hashed_password) VALUES (?, ?)")?
                .execute(params![username, hash])?;
            Ok(c.last_insert_rowid())
        })
    }

    // ── Legacy stat helpers ─────────────────────────────────────────────────

    /// `recordGamePlayed`. Exported and imported by `server/index.js` but never
    /// called there; ported for completeness.
    pub fn record_game_played(&self, pilot_id: i64) -> Result<()> {
        self.with(|c| {
            c.prepare_cached("UPDATE pilots SET games_played = games_played + 1 WHERE id = ?")?
                .execute(params![pilot_id])?;
            Ok(())
        })
    }

    /// `recordHighScore`. Also imported but never called.
    pub fn record_high_score(&self, pilot_id: i64, kills: i64) -> Result<()> {
        self.with(|c| {
            c.prepare_cached("UPDATE pilots SET high_score = ? WHERE id = ? AND ? > high_score")?
                .execute(params![kills, pilot_id, kills])?;
            Ok(())
        })
    }

    /// `savePilotColors` — invalid hex falls back to the defaults rather than
    /// erroring, exactly as the JS regex test does.
    pub fn save_pilot_colors(&self, pilot_id: i64, ship: &str, accent: &str) -> Result<()> {
        let hull = if is_hex_color(ship) { ship } else { "#9fb6cc" };
        let acc = if is_hex_color(accent) {
            accent
        } else {
            "#2a3340"
        };
        self.with(|c| {
            c.prepare_cached(
                "UPDATE pilots SET ship_color = ?, ship_accent_color = ? WHERE id = ?",
            )?
            .execute(params![hull, acc, pilot_id])?;
            Ok(())
        })
    }

    // ── Credits ─────────────────────────────────────────────────────────────

    /// `getCredits` — 0 for a missing pilot, matching `?.credits ?? 0`.
    pub fn get_credits(&self, pilot_id: i64) -> Result<i64> {
        self.with(|c| {
            Ok(c.prepare_cached("SELECT credits FROM pilots WHERE id = ?")?
                .query_row(params![pilot_id], |r| r.get::<_, i64>(0))
                .optional()?
                .unwrap_or(0))
        })
    }

    /// `_addCredits` — balance update plus ledger row.
    fn add_credits(
        &self,
        conn: &Connection,
        pilot_id: i64,
        amount: i64,
        reason: &str,
    ) -> Result<()> {
        conn.prepare_cached("UPDATE pilots SET credits = credits + ? WHERE id = ?")?
            .execute(params![amount, pilot_id])?;
        conn.prepare_cached(
            "INSERT INTO credit_transactions (pilot_id, amount, reason) VALUES (?, ?, ?)",
        )?
        .execute(params![pilot_id, amount, reason])?;
        Ok(())
    }

    /// `awardCredits` — clamps to at least 1, like the JS `Math.max(1, ...)`.
    pub fn award_credits(&self, pilot_id: i64, amount: i64, reason: &str) -> Result<i64> {
        let safe = amount.max(1);
        self.with(|c| self.add_credits(c, pilot_id, safe, reason))?;
        self.get_credits(pilot_id)
    }

    /// `spendCredits` — `Ok(new_balance)` on success, `Err(current_balance)`
    /// when the pilot cannot afford it.
    ///
    /// The amount is clamped up to 1 first (`Math.max(1, Math.floor(...))`), so
    /// a spend of 0 costs 1 credit. Preserved.
    pub fn spend_credits(
        &self,
        pilot_id: i64,
        amount: i64,
        reason: &str,
    ) -> Result<std::result::Result<i64, i64>> {
        let safe = amount.max(1);
        let current = self.get_credits(pilot_id)?;
        if current < safe {
            return Ok(Err(current));
        }
        self.with(|c| {
            c.prepare_cached("UPDATE pilots SET credits = MAX(0, credits - ?) WHERE id = ?")?
                .execute(params![safe, pilot_id])?;
            c.prepare_cached(
                "INSERT INTO credit_transactions (pilot_id, amount, reason) VALUES (?, ?, ?)",
            )?
            .execute(params![pilot_id, -safe, reason])?;
            Ok(())
        })?;
        Ok(Ok(self.get_credits(pilot_id)?))
    }

    /// `getCreditHistory` — newest first.
    pub fn credit_history(&self, pilot_id: i64, limit: i64) -> Result<Vec<CreditTx>> {
        self.with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT amount, reason, created_at FROM credit_transactions \
                 WHERE pilot_id = ? ORDER BY created_at DESC LIMIT ?",
            )?;
            let rows = stmt.query_map(params![pilot_id, limit], |r| {
                Ok(CreditTx {
                    amount: r.get(0)?,
                    reason: r.get(1)?,
                    created_at: r.get(2)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    // ── Achievements ────────────────────────────────────────────────────────

    /// `checkAndAwardAchievements` — awards, credits, and reports every
    /// achievement `stats` newly qualifies for.
    ///
    /// `stats` is a snapshot: the JS passes in the row it read *before* the
    /// loop and never refreshes it, so an achievement awarded here cannot
    /// cascade into another within the same call. Preserved.
    fn check_and_award(
        &self,
        conn: &Connection,
        pilot_id: i64,
        stats: &PilotStats,
    ) -> Result<Vec<EarnedAchievement>> {
        let existing: Vec<String> = {
            let mut stmt = conn.prepare_cached(
                "SELECT type, earned_at, credited FROM achievements WHERE pilot_id = ? \
                 ORDER BY earned_at ASC",
            )?;
            let rows = stmt.query_map(params![pilot_id], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        let mut newly = Vec::new();
        for def in ACHIEVEMENT_DEFS.iter() {
            if (def.check)(stats) && !existing.iter().any(|t| t == def.kind) {
                conn.prepare_cached(
                    "INSERT OR IGNORE INTO achievements (pilot_id, type) VALUES (?, ?)",
                )?
                .execute(params![pilot_id, def.kind])?;
                conn.prepare_cached(
                    "UPDATE achievements SET credited = 1 WHERE pilot_id = ? AND type = ?",
                )?
                .execute(params![pilot_id, def.kind])?;
                if def.reward > 0 {
                    self.add_credits(
                        conn,
                        pilot_id,
                        def.reward,
                        &format!("achievement:{}", def.kind),
                    )?;
                }
                newly.push(EarnedAchievement {
                    kind: def.kind,
                    label: def.label,
                    icon: def.icon,
                    reward: def.reward,
                });
            }
        }
        Ok(newly)
    }

    /// `backfillAchievements` — the startup pass.
    ///
    /// Two jobs, both idempotent: award anything a pilot already qualifies for
    /// but predates the achievement system, and recompute every rank in case
    /// the thresholds moved. Then pay out any achievement row still marked
    /// `credited = 0`, which is how achievements earned before the credits
    /// system get their reward.
    pub fn backfill_achievements(&self) -> Result<BackfillReport> {
        let pilots = self.with(|c| {
            let sql = format!("SELECT {PILOT_COLUMNS} FROM pilots");
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map([], map_pilot)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })?;

        let mut report = BackfillReport::default();
        for pilot in &pilots {
            let earned = self.with(|c| self.check_and_award(c, pilot.id, &pilot.stats))?;
            report.new_achievements += earned.len();
            let correct = compute_rank(pilot.stats.total_kills);
            if correct != pilot.rank {
                self.with(|c| {
                    c.prepare_cached("UPDATE pilots SET rank = ? WHERE id = ?")?
                        .execute(params![correct, pilot.id])?;
                    Ok(())
                })?;
                report.rank_fixes += 1;
            }
        }

        let uncredited: Vec<(i64, String)> = self.with(|c| {
            let mut stmt =
                c.prepare("SELECT pilot_id, type FROM achievements WHERE credited = 0")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })?;
        for (pilot_id, kind) in &uncredited {
            if let Some(def) = def_for(kind) {
                if def.reward > 0 {
                    self.with(|c| {
                        self.add_credits(c, *pilot_id, def.reward, &format!("achievement:{kind}"))
                    })?;
                    report.credits_awarded += def.reward;
                }
            }
            self.with(|c| {
                c.prepare_cached(
                    "UPDATE achievements SET credited = 1 WHERE pilot_id = ? AND type = ?",
                )?
                .execute(params![pilot_id, kind])?;
                Ok(())
            })?;
        }
        report.credited_rows = uncredited.len();
        Ok(report)
    }

    // ── Results ─────────────────────────────────────────────────────────────

    /// `recordMatchResult`.
    ///
    /// Applies the stat deltas, pays match credits (kills, bot kills, win
    /// bonus), recomputes the rank, then awards achievements against the
    /// *updated* row.
    pub fn record_match_result(
        &self,
        pilot_id: i64,
        kills: i64,
        deaths: i64,
        won: Option<bool>,
        bots_killed: i64,
    ) -> Result<RecordOutcome> {
        let won_inc = i64::from(won == Some(true));
        let lost_inc = i64::from(won == Some(false));
        self.with(|c| {
            c.prepare_cached(
                "UPDATE pilots SET
    games_played = games_played + 1,
    total_kills  = total_kills  + ?,
    total_deaths = total_deaths + ?,
    matches_won  = matches_won  + ?,
    matches_lost = matches_lost + ?,
    bots_killed  = bots_killed  + ?,
    high_score   = CASE WHEN ? > high_score THEN ? ELSE high_score END
  WHERE id = ?",
            )?
            .execute(params![
                kills,
                deaths,
                won_inc,
                lost_inc,
                bots_killed,
                kills,
                kills,
                pilot_id
            ])?;
            Ok(())
        })?;

        let match_cr = kills * CR_PER_KILL
            + bots_killed * CR_PER_BOT_KILL
            + if won == Some(true) { CR_WIN_BONUS } else { 0 };
        if match_cr > 0 {
            let mut parts: Vec<String> = Vec::new();
            if kills > 0 {
                parts.push(format!("kills({kills})"));
            }
            if bots_killed > 0 {
                parts.push(format!("bots({bots_killed})"));
            }
            if won == Some(true) {
                parts.push("win_bonus".to_string());
            }
            let reason = format!("match:{}", parts.join(","));
            self.with(|c| self.add_credits(c, pilot_id, match_cr, &reason))?;
        }

        let Some(pilot) = self.pilot_by_id(pilot_id)? else {
            return Ok(RecordOutcome {
                new_achievements: Vec::new(),
                credits_earned: match_cr,
            });
        };
        let new_rank = compute_rank(pilot.stats.total_kills);
        if new_rank != pilot.rank {
            self.with(|c| {
                c.prepare_cached("UPDATE pilots SET rank = ? WHERE id = ?")?
                    .execute(params![new_rank, pilot_id])?;
                Ok(())
            })?;
        }
        let new_achs = self.with(|c| self.check_and_award(c, pilot_id, &pilot.stats))?;
        let ach_cr: i64 = new_achs.iter().map(|a| a.reward).sum();
        Ok(RecordOutcome {
            new_achievements: new_achs,
            credits_earned: match_cr + ach_cr,
        })
    }

    /// `recordTrialTime` — records a personal best and awards any achievement
    /// it unlocks. Trial completions pay nothing directly; the credits come
    /// entirely from achievements.
    pub fn record_trial_time(
        &self,
        pilot_id: i64,
        trial_num: i64,
        time: f64,
    ) -> Result<RecordOutcome> {
        let idx = trial_num - 1;
        if !(0..=3).contains(&idx) {
            return Ok(RecordOutcome::default());
        }
        let col = ["trial1_best", "trial2_best", "trial3_best", "trial4_best"][idx as usize];
        self.with(|c| {
            let sql = format!(
                "UPDATE pilots SET {col} = ? WHERE id = ? AND ({col} IS NULL OR {col} > ?)"
            );
            c.prepare_cached(&sql)?
                .execute(params![time, pilot_id, time])?;
            Ok(())
        })?;
        let Some(pilot) = self.pilot_by_id(pilot_id)? else {
            return Ok(RecordOutcome::default());
        };
        let new_achs = self.with(|c| self.check_and_award(c, pilot_id, &pilot.stats))?;
        let ach_cr: i64 = new_achs.iter().map(|a| a.reward).sum();
        Ok(RecordOutcome {
            new_achievements: new_achs,
            credits_earned: ach_cr,
        })
    }

    /// `recordCampaignResult` — bumps the boss/completion counters, records a
    /// best-lives high-water mark, and pays the mission bounty.
    pub fn record_campaign_result(
        &self,
        pilot_id: i64,
        mission_num: i64,
        lives_remaining: i64,
    ) -> Result<RecordOutcome> {
        let idx = mission_num - 1;
        if !(0..=2).contains(&idx) {
            return Ok(RecordOutcome::default());
        }
        let lives = lives_remaining.clamp(0, 3);
        let col = [
            "campaign1_best_lives",
            "campaign2_best_lives",
            "campaign3_best_lives",
        ][idx as usize];
        self.with(|c| {
            c.prepare_cached(
                "UPDATE pilots SET
    campaign_boss_kills        = campaign_boss_kills + 1,
    campaign_total_completions = campaign_total_completions + 1
  WHERE id = ?",
            )?
            .execute(params![pilot_id])?;
            // Best = *most* lives remaining, so the guard is `<`, not `>`.
            let sql = format!(
                "UPDATE pilots SET {col} = ? WHERE id = ? AND ({col} IS NULL OR {col} < ?)"
            );
            c.prepare_cached(&sql)?
                .execute(params![lives, pilot_id, lives])?;
            Ok(())
        })?;
        let mission_cr = CAMPAIGN_MISSION_CREDITS[idx as usize];
        self.with(|c| {
            self.add_credits(
                c,
                pilot_id,
                mission_cr,
                &format!("campaign:mission{mission_num}"),
            )
        })?;
        let Some(pilot) = self.pilot_by_id(pilot_id)? else {
            return Ok(RecordOutcome {
                new_achievements: Vec::new(),
                credits_earned: mission_cr,
            });
        };
        let new_achs = self.with(|c| self.check_and_award(c, pilot_id, &pilot.stats))?;
        let ach_cr: i64 = new_achs.iter().map(|a| a.reward).sum();
        Ok(RecordOutcome {
            new_achievements: new_achs,
            credits_earned: mission_cr + ach_cr,
        })
    }

    // ── Views ───────────────────────────────────────────────────────────────

    /// `getPilotProfile` — `None` when the callsign is unknown.
    pub fn pilot_profile(&self, username: &str) -> Result<Option<ProfileView>> {
        let Some(pilot) = self.pilot_by_username(username)? else {
            return Ok(None);
        };
        let earned: Vec<(String, i64)> = self.with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT type, earned_at, credited FROM achievements WHERE pilot_id = ? \
                 ORDER BY earned_at ASC",
            )?;
            let rows = stmt.query_map(params![pilot.id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })?;

        let s = &pilot.stats;
        let achievements = ACHIEVEMENT_DEFS
            .iter()
            .map(|def| {
                let hit = earned.iter().find(|(t, _)| t == def.kind);
                AchievementView {
                    kind: def.kind,
                    label: def.label,
                    icon: def.icon,
                    desc: def.desc,
                    earned: hit.is_some(),
                    earned_at: hit.map(|(_, at)| *at),
                    // `(!earned && def.progress) ? def.progress(pilot) : null`
                    progress: match (hit.is_some(), def.progress) {
                        (false, Some(f)) => f(s),
                        _ => None,
                    },
                }
            })
            .collect();

        Ok(Some(ProfileView {
            username: pilot.username.clone(),
            rank: pilot.rank.clone(),
            high_score: pilot.high_score,
            games_played: pilot.games_played,
            total_kills: s.total_kills,
            total_deaths: s.total_deaths,
            matches_won: s.matches_won,
            matches_lost: s.matches_lost,
            bots_killed: s.bots_killed,
            kdr: kdr_str(s.total_kills, s.total_deaths),
            trial1_best: s.trial_best[0],
            trial2_best: s.trial_best[1],
            trial3_best: s.trial_best[2],
            trial4_best: s.trial_best[3],
            achievements,
            credits: pilot.credits,
            unlock_hull: s.unlock_hull,
            unlock_accent: s.unlock_accent,
            unlock_trail: s.unlock_trail,
            unlock_trail_shape: s.unlock_trail_shape,
            unlock_admin_ship: s.unlock_admin_ship,
            campaign1_best_lives: s.campaign_best_lives[0],
            campaign2_best_lives: s.campaign_best_lives[1],
            campaign3_best_lives: s.campaign_best_lives[2],
            campaign_boss_kills: s.campaign_boss_kills,
            campaign_total_completions: s.campaign_total_completions,
            created_at: pilot.created_at,
        }))
    }

    /// `getLeaderboard` — top 50 by kills, then wins.
    pub fn leaderboard(&self) -> Result<Vec<LeaderboardRow>> {
        self.with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT username, rank, total_kills, total_deaths, matches_won, matches_lost, \
                 games_played, high_score
  FROM pilots
  ORDER BY total_kills DESC, matches_won DESC
  LIMIT 50",
            )?;
            let rows = stmt.query_map([], |r| {
                let total_kills: i64 = r.get(2)?;
                let total_deaths: i64 = r.get(3)?;
                Ok(LeaderboardRow {
                    position: 0,
                    username: r.get(0)?,
                    pilot_rank: r.get(1)?,
                    total_kills,
                    total_deaths,
                    matches_won: r.get(4)?,
                    matches_lost: r.get(5)?,
                    games_played: r.get(6)?,
                    high_score: r.get(7)?,
                    kdr: kdr_str(total_kills, total_deaths),
                })
            })?;
            let mut out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            for (i, row) in out.iter_mut().enumerate() {
                row.position = i + 1;
            }
            Ok(out)
        })
    }

    /// `getCustomizationUnlocks` — all false for an unknown pilot id, matching
    /// the JS `!!r?.unlock_*`.
    pub fn unlocks(&self, pilot_id: i64) -> Result<Unlocks> {
        self.with(|c| {
            let row = c
                .prepare_cached(
                    "SELECT unlock_hull, unlock_accent, unlock_trail, unlock_trail_shape, \
                     unlock_admin_ship FROM pilots WHERE id = ?",
                )?
                .query_row(params![pilot_id], |r| {
                    Ok(Unlocks {
                        unlock_hull: r.get::<_, i64>(0)? != 0,
                        unlock_accent: r.get::<_, i64>(1)? != 0,
                        unlock_trail: r.get::<_, i64>(2)? != 0,
                        unlock_trail_shape: r.get::<_, i64>(3)? != 0,
                        unlock_admin_ship: r.get::<_, i64>(4)? != 0,
                    })
                })
                .optional()?;
            Ok(row.unwrap_or(Unlocks {
                unlock_hull: false,
                unlock_accent: false,
                unlock_trail: false,
                unlock_trail_shape: false,
                unlock_admin_ship: false,
            }))
        })
    }

    /// `purchaseUnlock`.
    pub fn purchase_unlock(&self, pilot_id: i64, feature: &str) -> Result<PurchaseOutcome> {
        let Some(&(_, cost)) = UNLOCK_COSTS.iter().find(|(k, _)| *k == feature) else {
            return Ok(PurchaseOutcome::UnknownFeature);
        };
        let owned = self.unlocks(pilot_id)?;
        let already = match feature {
            "hull" => owned.unlock_hull,
            "accent" => owned.unlock_accent,
            "trail" => owned.unlock_trail,
            "trail_shape" => owned.unlock_trail_shape,
            "admin_ship" => owned.unlock_admin_ship,
            _ => false,
        };
        if already {
            return Ok(PurchaseOutcome::AlreadyOwned {
                balance: self.get_credits(pilot_id)?,
            });
        }
        let balance = match self.spend_credits(pilot_id, cost, &format!("unlock:{feature}"))? {
            Ok(b) => b,
            Err(b) => return Ok(PurchaseOutcome::Insufficient { balance: b }),
        };
        let col = match feature {
            "hull" => "unlock_hull",
            "accent" => "unlock_accent",
            "trail" => "unlock_trail",
            "trail_shape" => "unlock_trail_shape",
            "admin_ship" => "unlock_admin_ship",
            _ => unreachable!("feature was matched against UNLOCK_COSTS above"),
        };
        self.with(|c| {
            c.execute(
                &format!("UPDATE pilots SET {col} = 1 WHERE id = ?"),
                params![pilot_id],
            )?;
            Ok(())
        })?;
        let new_achs = match self.pilot_by_id(pilot_id)? {
            Some(p) => self.with(|c| self.check_and_award(c, pilot_id, &p.stats))?,
            None => Vec::new(),
        };
        Ok(PurchaseOutcome::Bought {
            balance,
            new_achievements: new_achs,
        })
    }
}

/// What the startup backfill did, for the log line.
#[derive(Debug, Default, Clone, Copy)]
pub struct BackfillReport {
    /// Achievements newly awarded.
    pub new_achievements: usize,
    /// Ranks corrected.
    pub rank_fixes: usize,
    /// Achievement rows flipped from `credited = 0`.
    pub credited_rows: usize,
    /// Credits paid out by that flip.
    pub credits_awarded: i64,
}

/// `#rrggbb`, the exact shape `/^#[0-9a-fA-F]{6}$/` accepts.
fn is_hex_color(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// The `migrations` array from `server/db.js`, verbatim and in order.
const MIGRATIONS: &[&str] = &[
    "ALTER TABLE pilots ADD COLUMN ship_color TEXT NOT NULL DEFAULT '#9fb6cc'",
    "ALTER TABLE pilots ADD COLUMN ship_accent_color TEXT NOT NULL DEFAULT '#2a3340'",
    "ALTER TABLE pilots ADD COLUMN total_kills INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE pilots ADD COLUMN total_deaths INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE pilots ADD COLUMN matches_won INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE pilots ADD COLUMN matches_lost INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE pilots ADD COLUMN bots_killed INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE pilots ADD COLUMN trial1_best REAL",
    "ALTER TABLE pilots ADD COLUMN trial2_best REAL",
    "ALTER TABLE pilots ADD COLUMN trial3_best REAL",
    "ALTER TABLE pilots ADD COLUMN trial4_best REAL",
    "ALTER TABLE pilots ADD COLUMN credits INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE pilots ADD COLUMN unlock_colors INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE pilots ADD COLUMN unlock_trail INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE pilots ADD COLUMN unlock_hull INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE pilots ADD COLUMN unlock_accent INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE pilots ADD COLUMN unlock_trail_shape INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE pilots ADD COLUMN campaign1_best_lives INTEGER",
    "ALTER TABLE pilots ADD COLUMN campaign2_best_lives INTEGER",
    "ALTER TABLE pilots ADD COLUMN campaign3_best_lives INTEGER",
    "ALTER TABLE pilots ADD COLUMN campaign_boss_kills INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE pilots ADD COLUMN campaign_total_completions INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE pilots ADD COLUMN unlock_admin_ship INTEGER NOT NULL DEFAULT 0",
];

/// Builds the `/api/login` payload from a pilot row plus a freshly signed
/// token.
pub fn login_result(pilot: &Pilot, token: String) -> LoginResult {
    let s = &pilot.stats;
    LoginResult {
        token,
        username: pilot.username.clone(),
        rank: pilot.rank.clone(),
        high_score: pilot.high_score,
        games_played: pilot.games_played,
        // `pilot.ship_color || '#9fb6cc'` — an empty string is falsy in JS, so
        // it falls back too, not just NULL.
        ship_color: if pilot.ship_color.is_empty() {
            "#9fb6cc".to_string()
        } else {
            pilot.ship_color.clone()
        },
        accent_color: if pilot.ship_accent_color.is_empty() {
            "#2a3340".to_string()
        } else {
            pilot.ship_accent_color.clone()
        },
        total_kills: s.total_kills,
        total_deaths: s.total_deaths,
        matches_won: s.matches_won,
        matches_lost: s.matches_lost,
        bots_killed: s.bots_killed,
        kdr: kdr_str(s.total_kills, s.total_deaths),
        trial1_best: s.trial_best[0],
        trial2_best: s.trial_best[1],
        trial3_best: s.trial_best[2],
        trial4_best: s.trial_best[3],
        credits: pilot.credits,
        unlock_hull: s.unlock_hull,
        unlock_accent: s.unlock_accent,
        unlock_trail: s.unlock_trail,
        unlock_trail_shape: s.unlock_trail_shape,
        unlock_admin_ship: s.unlock_admin_ship,
        campaign1_best_lives: s.campaign_best_lives[0],
        campaign2_best_lives: s.campaign_best_lives[1],
        campaign3_best_lives: s.campaign_best_lives[2],
        campaign_boss_kills: s.campaign_boss_kills,
        campaign_total_completions: s.campaign_total_completions,
    }
}

/// Silences the unused-import warning for [`JsNum`], which the response structs
/// reach only through `serialize_opt_js_f64`.
#[allow(dead_code)]
type _JsNumIsUsedByProgress = JsNum;
