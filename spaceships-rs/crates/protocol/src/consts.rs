//! Server constants that are observable through the wire protocol.
//!
//! These are transcribed from `server/index.js` and exist here so a Rust port
//! and its tests agree on the numbers the current JS server bakes into the
//! messages it sends. They are *not* general game-balance tuning — anything that
//! only affects local simulation belongs in `spaceships-sim`, not here.

/// Starting and maximum hit points for a ship. Every `hp` message is clamped to
/// `0..=SHIP_MAX_HP`.
pub const SHIP_MAX_HP: i32 = 100;

/// Delay between a `death` broadcast and the matching `respawn` broadcast.
pub const RESPAWN_DELAY_MS: u64 = 2000;

/// Spawn protection window applied on match start and on every respawn. Hits
/// landing inside it are dropped server-side.
pub const SPAWN_INVULN_MS: u64 = 2000;

/// Match length. Also the initial value the `match-state` timer counts down
/// from.
pub const MATCH_DURATION_MS: u64 = 300_000;

/// Damage a missile `hit` applies.
pub const MISSILE_DAMAGE: i32 = 50;

/// Damage a bullet or beam `hit` applies.
pub const GUN_DAMAGE: i32 = 10;

/// Damage a single `asteroid-hit` applies, regardless of weapon.
pub const ASTEROID_HIT_DAMAGE: i32 = 1;

/// Minimum interval between accepted missile `hit` reports from one shooter.
pub const MISSILE_HIT_MIN_INTERVAL_MS: u64 = 400;

/// Minimum interval between accepted bullet/beam `hit` reports from one shooter.
pub const GUN_HIT_MIN_INTERVAL_MS: u64 = 40;

/// Rate at which clients emit `state` messages (20 Hz).
pub const STATE_HZ: f64 = 20.0;

/// Length of a room code, in uppercase ASCII letters.
pub const ROOM_CODE_LEN: usize = 4;

/// Maximum length of a sanitized display name.
pub const MAX_NAME_LEN: usize = 16;

/// Number of asteroids generated for a space match.
pub const ASTEROID_COUNT: usize = 60;

/// Radius of the generated asteroid field.
pub const ASTEROID_FIELD_RADIUS: f64 = 400.0;
