//! Simulation state, the tick contract, and the frame the renderer consumes.
//!
//! This module is **data only**. It says what a match *is*; the physics,
//! collision, homing, and AI modules say what happens to it. Nothing here
//! integrates a position or resolves a hit.
//!
//! # The shape of a tick
//!
//! ```text
//!   Input[]  ──┐
//!   NetEvent[]─┼──▶ tick(&mut World, .., dt) ──▶ Frame ──▶ JS renderer
//!   World    ──┘            │                      │
//!                           └── mutates World      └── NetIntent[] ──▶ WebSocket
//! ```
//!
//! [`World`] is the authoritative state and never leaves Rust. [`Frame`] is a
//! disposable snapshot built fresh each tick, shaped for one cheap copy across
//! the WASM boundary. See [`TickFn`] for the exact signature and its contract.
//!
//! # `f64` in the world, `f32` in the frame
//!
//! The simulation is `f64` throughout, matching [`crate::math::Vec3`]:
//!
//! - The JS runs entirely in doubles — every `Number` is IEEE-754 binary64 — and
//!   during the port the Rust simulation has to agree with the JS one closely
//!   enough to A/B them. Narrowing to `f32` would introduce divergence that is
//!   indistinguishable from a porting bug, which is exactly the signal we need
//!   to keep clean.
//! - The wire format is JSON, so values already round-trip as doubles. A `f32`
//!   world would round every inbound position on arrival.
//! - Determinism is unaffected by the choice. `+ - * /` and `sqrt` are
//!   correctly rounded at both widths, so both are bit-reproducible; what is
//!   *not* reproducible across platforms is `sin`/`cos`/`powf`, and that hazard
//!   is identical either way (see the crate docs). `f64` merely makes the
//!   drift-per-operation smaller, so an accumulated difference takes longer to
//!   become visible.
//!
//! [`Frame`] is `f32`. It exists to be copied to JavaScript sixty times a
//! second, and every number in it ends up in a `Float32Array` feeding a
//! `THREE.Vector3` or a GPU attribute buffer — WebGL has no doubles. Narrowing
//! at the boundary therefore loses nothing visible and halves the bytes crossing
//! it. The narrowing is strictly one-way: `Frame` values are never read back
//! into [`World`], so the reduced precision can never feed the simulation.
//!
//! # Why `Frame` is shaped the way it is
//!
//! One `Vec` per entity kind, each holding a flat `#[repr(C)]`, `Copy` record —
//! never a nested collection, never a `String`, never a `Box`. That gives:
//!
//! - **One allocation per kind per frame**, not one per entity, and the buffers
//!   are reusable via [`Frame::clear`].
//! - **A contiguous byte range per kind**, so the WASM layer can hand JS a typed
//!   array view over the whole slice instead of marshalling entity by entity.
//! - **Stable strides**, so the JS side can read fields at fixed offsets.
//!
//! Text never crosses: pilot names live in the JS lobby state, keyed by id, and
//! the simulation only ever deals in ids.
//!
//! # What is deliberately absent
//!
//! No Three.js object, DOM node, audio handle, material, camera, colour,
//! texture, particle, or socket. Cosmetic-only quantities (asteroid spin, hit
//! flash) *are* here, because they must agree across clients or two players see
//! different rocks — but they are marked as such and never influence collision.

use crate::math::Vec3;
use crate::rng::Rng;
use crate::rules::{AimProfile, Rules, BOSS_HITBOX_COUNT, BOSS_ID_BASE};

/// Re-export of the orientation type, which now lives beside [`Vec3`] in
/// [`crate::math`] along with the algebra that operates on it.
///
/// It used to be defined here, as storage only, with a doc comment saying that
/// multiplication, `slerp`, and `from_axis_angle` "belong beside `Vec3` in
/// `math`, which this module does not own". Three modules then wrote their own
/// copies of that algebra. The type and its operations are now in one place;
/// this alias keeps `world::Quat` a valid path for every existing caller.
pub use crate::math::Quat;

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------

/// Fixed simulation rate.
///
/// The JS client sends `state` updates at 20 Hz
/// ([`crate::rules::MatchRules::state_send_interval`]) while rendering at
/// display rate. The simulation step is a separate, faster fixed rate; callers
/// accumulate real elapsed time and run a whole number of ticks, keeping the
/// remainder for the next frame.
pub const TICK_HZ: f64 = 60.0;

/// Duration of one fixed simulation step, in seconds.
pub const TICK_DT: f64 = 1.0 / TICK_HZ;

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Entity id. Players, bots, and boss hitboxes share one id space.
///
/// The ranges in use today:
///
/// | Range | Who | Source |
/// |---|---|---|
/// | `1..` | Human players | `server/index.js:411` (`nextId`) |
/// | `..-1` | Server-spawned balance bots | `server/index.js:750` (`-(nextId++)`) |
/// | `1..=9` | Solo skirmish / training bots | `main.js:2900`, `:2907`, `:2911` |
/// | `100..` | Campaign wave bots | `main.js:2330` (`campaignNextBotId`) |
/// | `9000..9019` | Boss hitboxes | [`BOSS_ID_BASE`] |
///
/// Solo bot ids and multiplayer player ids overlap, which is safe only because
/// a match is never both. Nothing in the simulation may assume a sign or a
/// range beyond [`is_boss_hitbox`].
pub type EntityId = i32;

/// Whether `id` is one of the campaign boss's hitboxes.
///
/// `main.js:1627` open-codes this test at three call sites.
#[must_use]
pub fn is_boss_hitbox(id: EntityId) -> bool {
    id >= BOSS_ID_BASE && id < BOSS_ID_BASE + BOSS_HITBOX_COUNT as EntityId
}

/// Team index. Two teams, 0 and 1.
///
/// Represented as `Option<Team>` on an entity because unassigned is a real
/// state: `scores` entries carry `team: null` until the server's `players`
/// message arrives (`main.js:581`), and friendly fire is only rejected when
/// *both* sides have a team (`server/index.js:941`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Team {
    /// Team 0. Spawns at `-z` on both maps.
    #[default]
    Zero,
    /// Team 1. Spawns at `+z`, rotated 180° about `y`.
    One,
}

impl Team {
    /// Team index, for scoreboard arrays.
    #[must_use]
    pub fn index(self) -> usize {
        match self {
            Team::Zero => 0,
            Team::One => 1,
        }
    }

    /// The opposing team.
    #[must_use]
    pub fn other(self) -> Team {
        match self {
            Team::Zero => Team::One,
            Team::One => Team::Zero,
        }
    }

    /// Team from an index, or `None` if out of range.
    #[must_use]
    pub fn from_index(i: usize) -> Option<Team> {
        match i {
            0 => Some(Team::Zero),
            1 => Some(Team::One),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Mode and map
// ---------------------------------------------------------------------------

/// Which map a match is played on. Decides gravity-adjacent rules: the space
/// map has a moon and motherships, the terrain map has ground, airfields, and a
/// kill plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MapKind {
    /// Open space around a moon at the origin. `main.js:102`
    /// (`MAP_TYPE = opts.map || 'space'`).
    #[default]
    Space,
    /// Heightmapped terrain with two airfields. `terrain.js`.
    Terrain,
}

/// What kind of match this is. Drives which optional state blocks exist and
/// which damage path is authoritative.
///
/// `main.js:137`–`:139` and `main.js:2264` derive this from `opts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Networked team deathmatch. The server owns HP and respawn.
    #[default]
    Multiplayer,
    /// Solo warm-up against a single bot. `main.js:2897`.
    Training,
    /// Solo 5v5 against bots. `main.js:2901`.
    Skirmish,
    /// Checkpoint time trial, 1–4. `main.js:356` (`TRIAL_NUM`).
    Trials(u8),
    /// Scripted mission, 1–3, ending in the capital ship fight.
    /// `main.js:139` (`CAMPAIGN_MISSION`).
    Campaign(u8),
    /// The scripted first-run tutorial. Notably, it suppresses *all* collision
    /// self-damage (`main.js:2214`, `:2243`, `:2255`, `:1314`).
    Tutorial,
}

impl Mode {
    /// Whether this mode runs entirely on this machine.
    ///
    /// The JS spells this `isSolo` and branches on it at every damage site.
    #[must_use]
    pub fn is_solo(self) -> bool {
        !matches!(self, Mode::Multiplayer)
    }

    /// Whether the match clock and team kill counters run.
    /// `main.js:2270` (`matchActive`).
    #[must_use]
    pub fn has_match_clock(self) -> bool {
        matches!(self, Mode::Multiplayer | Mode::Training | Mode::Skirmish)
    }

    /// Whether collision self-damage applies. False only in the tutorial, which
    /// would otherwise kill the player during the "fly into a rock" step.
    #[must_use]
    pub fn has_collision_damage(self) -> bool {
        !matches!(self, Mode::Tutorial)
    }
}

/// Who decides hit points.
///
/// This is the asymmetry that produced a real, already-fixed-once bug. In
/// multiplayer the server owns asteroid HP (`asteroid-hit` in, `asteroid-hp` /
/// `asteroid-destroyed` out — `server/index.js:817`) and ship HP
/// (`server/index.js:910`). In solo there is no server, so the client mirrors
/// the same rules locally (`damageAsteroidLocal`, `main.js:2179`, whose comment
/// records that without it *every solo asteroid hit was sent to a socket that
/// does not exist and silently dropped*).
///
/// The fix is not to pick a side, it is to stop having two code paths. The
/// simulation always applies damage to its own [`World::asteroids`] and
/// [`World::ships`]; `authority` only decides whether it *also* emits a
/// [`NetIntent`] and waits for the authoritative value to come back as a
/// [`NetEvent`], or treats its own result as final.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Authority {
    /// This machine is authoritative. Damage resolves immediately.
    #[default]
    Local,
    /// A server is authoritative. Damage is predicted locally and reconciled
    /// when the matching [`NetEvent`] arrives.
    Server,
}

// ---------------------------------------------------------------------------
// Ships
// ---------------------------------------------------------------------------

/// Which gun a ship has selected. Toggled with `P` (`main.js:1034`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GunMode {
    /// Travelling bolt: cheap, fast cooldown.
    #[default]
    Bullet,
    /// Hitscan beam: 3× the ammo, 5× the cooldown, no travel time.
    Beam,
}

/// What drives a ship.
///
/// In the JS these are four different storage locations — the local player is a
/// pile of module-level `let`s, remotes live in a `Map`, bots carry a closure,
/// and boss hitboxes are fake `Map` entries. Here they are one record with a
/// discriminant, which is what lets a single damage path serve all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShipKind {
    /// The player on this machine.
    #[default]
    Local,
    /// Another player, whose pose arrives over the network and is interpolated.
    Remote,
    /// An AI-driven ship simulated here.
    Bot,
    /// One of the campaign boss's [`BOSS_HITBOX_COUNT`] damage points. Carries
    /// no flight model: its pose is slaved to the capital ship
    /// (`main.js:2664`).
    BossHitbox,
}

/// State for interpolating a networked ship between `state` messages.
///
/// `main.js:827`–`:853` (ingest) and `main.js:1721`–`:1726` (interpolation).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RemoteInterp {
    /// Latest received position; the rendered pose chases this.
    pub target_pos: Vec3,
    /// Latest received orientation.
    pub target_quat: Quat,
    /// False until the first `state` arrives, at which point the ship is
    /// snapped rather than interpolated (`main.js:849`).
    pub has_target: bool,
    /// Simulation time of the last `state`, used to estimate velocity.
    ///
    /// The JS uses `performance.now()` here (`main.js:831`), which is wall
    /// clock and therefore forbidden in this crate. It becomes [`World::time`].
    pub last_state_time: f64,
    /// Position at `last_state_time`.
    pub last_state_pos: Vec3,
    /// False until the first velocity estimate, which is taken raw instead of
    /// blended (`main.js:836`).
    pub vel_seeded: bool,
    /// Whether the remote reported boosting, for trail effects.
    pub boost: bool,
}

/// Bot AI finite state machine. `bot.js:41` (`state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BotFsm {
    /// Closing on the nearest opponent.
    #[default]
    Seek,
    /// In range and shooting.
    Attack,
    /// Breaking off along a random axis.
    Evade,
}

/// Per-bot AI state. `bot.js:41`–`:56`, which holds it in closure variables.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BotState {
    /// Current behaviour.
    pub fsm: BotFsm,
    /// Time spent in the current behaviour.
    pub state_timer: f64,
    /// Countdown to the next allowed shot.
    pub fire_timer: f64,
    /// Missiles remaining.
    pub missiles_left: u8,
    /// Countdown to the next allowed missile.
    pub missile_timer: f64,
    /// How long the bot has been below
    /// [`crate::rules::BotRules::stuck_speed_threshold`].
    pub stuck_time: f64,
    /// Random unit direction held for the duration of an evade. `bot.js:57`
    /// (`chooseEvadeDir`).
    pub evade_axis: Vec3,
    /// Random-walking aim error, scaled by range before use. `bot.js:167`.
    pub aim_offset: Vec3,
    /// Smoothed intercept point the bot is actually aiming at. `bot.js:49`.
    pub tracked_lead: Vec3,
    /// False until `tracked_lead` has been seeded from a real intercept
    /// solution. `bot.js:50`.
    pub tracked_lead_seeded: bool,
    /// Hard difficulty: faster gun, more missiles. `main.js:2495`.
    pub hard_mode: bool,
    /// True for campaign wave bots, which do not respawn (`main.js:3281`).
    pub is_campaign_bot: bool,
}

/// One ship: kinematics, hull, weapons, and whatever drives it.
///
/// Every shootable thing in the world is one of these, including the boss's
/// hitboxes. That is the whole reason a single damage rule can be enforced.
/// Not `Copy`: [`Self::touching_asteroids`] is a `Vec`. That is deliberate —
/// the alternative is a fixed-capacity inline set, and a ship wedged in a rock
/// cluster can legitimately overlap more rocks than any fixed bound worth
/// picking. `Ship` is moved and borrowed, never copied on a hot path.
#[derive(Debug, Clone, PartialEq)]
pub struct Ship {
    /// Stable identity, unique within a match.
    pub id: EntityId,
    /// Team, or `None` while unassigned. Friendly fire is rejected only when
    /// both sides have one (`server/index.js:941`).
    pub team: Option<Team>,
    /// What drives this ship.
    pub kind: ShipKind,

    /// World position.
    pub pos: Vec3,
    /// Orientation. The nose points along the local `+z` axis.
    pub quat: Quat,
    /// World velocity. Not simply `forward * throttle` — drift, boost, and
    /// collision restitution all push it off the nose.
    pub vel: Vec3,
    /// Smoothed throttle, chasing `target_throttle`. `main.js:1217`.
    pub throttle: f64,
    /// Commanded throttle, clamped to
    /// `0..=`[`crate::rules::ShipRules::max_throttle`]. `main.js:1216`.
    pub target_throttle: f64,
    /// Ramped keyboard steering, x. `main.js:992` (`arrowKx`). Ramps rather
    /// than snapping so arrow keys are not strictly worse than a mouse.
    pub arrow_kx: f64,
    /// Ramped keyboard steering, y. `main.js:992` (`arrowKy`).
    pub arrow_ky: f64,

    /// Current hit points.
    pub hp: i32,
    /// False between death and respawn. A dead ship is skipped by every weapon
    /// (`bullets.js:139`) and stops being integrated.
    pub alive: bool,
    /// Counts down to respawn while dead.
    pub respawn_timer: f64,
    /// Counts down after spawning; while positive, all damage is rejected.
    ///
    /// **This field exists on every ship, including bots.** In the JS only the
    /// local player has one — see
    /// [`crate::rules::CombatRules::spawn_invuln`].
    pub invuln_timer: f64,

    /// Selected gun.
    pub gun_mode: GunMode,
    /// Countdown to the next allowed shot.
    pub fire_timer: f64,
    /// Ammo remaining; fractional because it regenerates continuously.
    pub ammo: f64,
    /// Time since the last shot, gating ammo regeneration. `main.js:1090`.
    pub ammo_idle: f64,
    /// Missiles remaining.
    pub missiles_left: u8,
    /// Flare charges remaining.
    pub flares_left: u8,

    /// Boost meter, in seconds of boost remaining.
    pub boost_meter: f64,
    /// Time since boost was last requested, gating recharge. `main.js:1266`.
    pub boost_idle: f64,
    /// Brake charge, `0..=1`. `main.js:1299`.
    pub brake_charge: f64,
    /// Remaining brake-release boost. `main.js:1261`.
    pub brake_boost_timer: f64,
    /// Charge level the release boost was launched at, which scales its
    /// strength. `main.js:1303`.
    pub brake_boost_charge: f64,
    /// Time held at full brake charge, counting toward self-damage.
    /// `main.js:1313`.
    pub brake_overcharge_time: f64,
    /// Fractional self-damage accumulator; whole points are spent as they
    /// accrue so damage arrives in integers. `main.js:1315`.
    pub self_damage_accum: f64,
    /// Whether the brake was held last tick, for release edge detection.
    /// `main.js:1300`.
    pub prev_braking: bool,

    /// Time since taking damage, gating health regeneration. `main.js:1116`.
    pub health_idle_damage: f64,
    /// Time since firing, also gating health regeneration — both clocks must
    /// clear. `main.js:1117`.
    pub health_idle_shot: f64,
    /// Accumulator for the regeneration interval. `main.js:1118`.
    pub health_regen_tick: f64,

    /// Extra hit radius this ship's *own* shots get, from its control scheme.
    /// See [`crate::rules::ShipRules::hit_radius_coarse_aim_bonus`]. Stored per
    /// ship because it is a property of the shooter, not of the world.
    pub coarse_aim: bool,
    /// Hit radius override, used only by boss hitboxes. `None` means "use
    /// [`crate::rules::ShipRules::hit_radius`]". `main.js:2945`
    /// (`hitRadius: 28`), which today only `bullets.js:144` honours.
    pub hit_radius_override: Option<f64>,

    /// Ids of asteroids currently overlapping this ship.
    ///
    /// Collision damage is edge-triggered: you take 15–29 on the frame you
    /// *enter* a rock, not every frame you are inside it (`main.js:2214`). This
    /// is a `Vec` rather than a set because it holds at most a handful of ids
    /// and because a `HashSet` iteration order is a determinism hazard.
    pub touching_asteroids: Vec<u32>,
    /// Whether the ship was touching the moon last tick — same edge trigger,
    /// but the moon kills outright. `main.js:2222`.
    pub touching_moon: bool,
    /// Whether the ship was below the terrain kill plane last tick.
    /// `main.js:2258`.
    pub touching_ground: bool,

    /// Cosmetic damage flash, decays at 4/s. Not collision-relevant, but it
    /// must agree across clients or two players see different hits.
    /// `asteroids.js:101` uses the same decay for rocks.
    pub hit_flash: f64,

    /// Network interpolation state, for [`ShipKind::Remote`].
    pub interp: RemoteInterp,
    /// AI state, for [`ShipKind::Bot`].
    pub bot: BotState,
}

impl Ship {
    /// A ship at rest at `pos`, at full health, with a full load-out and a
    /// fresh spawn-invulnerability window.
    ///
    /// This is the one place a ship's starting values are decided, so
    /// `spawnBot` (`main.js:2471`), `getOrCreateRemote` (`main.js:687`) and
    /// `reviveSelf` (`main.js:755`) cannot drift apart the way they have.
    #[must_use]
    pub fn spawn(id: EntityId, kind: ShipKind, pos: Vec3, quat: Quat, rules: &Rules) -> Ship {
        Ship {
            id,
            team: None,
            kind,
            pos,
            quat,
            vel: Vec3::ZERO,
            throttle: 0.0,
            target_throttle: 0.0,
            arrow_kx: 0.0,
            arrow_ky: 0.0,

            hp: rules.ship.max_hp,
            alive: true,
            respawn_timer: 0.0,
            invuln_timer: rules.combat.spawn_invuln,

            gun_mode: GunMode::Bullet,
            fire_timer: 0.0,
            ammo: rules.weapons.max_ammo,
            ammo_idle: rules.weapons.ammo_regen_delay,
            missiles_left: rules.weapons.missile_max,
            flares_left: rules.weapons.flare_max,

            boost_meter: rules.ship.max_boost,
            boost_idle: rules.ship.boost_regen_delay,
            brake_charge: 0.0,
            brake_boost_timer: 0.0,
            brake_boost_charge: 0.0,
            brake_overcharge_time: 0.0,
            self_damage_accum: 0.0,
            prev_braking: false,

            health_idle_damage: rules.combat.health_regen_delay,
            health_idle_shot: rules.combat.health_regen_delay,
            health_regen_tick: 0.0,

            coarse_aim: false,
            hit_radius_override: None,

            touching_asteroids: Vec::new(),
            touching_moon: false,
            touching_ground: false,
            hit_flash: 0.0,

            interp: RemoteInterp::default(),
            bot: BotState::default(),
        }
    }

    /// The radius a weapon must reach to hit this ship.
    ///
    /// One function, so no weapon can invent its own answer. `shooter_coarse`
    /// is the *shooter's* control scheme; see
    /// [`crate::rules::ShipRules::hit_radius_coarse_aim_bonus`].
    #[must_use]
    pub fn hit_radius(&self, rules: &Rules, shooter_coarse: bool) -> f64 {
        if let Some(r) = self.hit_radius_override {
            return r;
        }
        if shooter_coarse {
            rules.ship.hit_radius + rules.ship.hit_radius_coarse_aim_bonus
        } else {
            rules.ship.hit_radius
        }
    }

    /// Whether this ship can currently be damaged at all.
    ///
    /// Dead ships and ships inside their spawn window are immune, for players
    /// and bots alike. The JS applies the second half of this to the local
    /// player only (`main.js:3232` vs. `main.js:3201`).
    #[must_use]
    pub fn is_damageable(&self) -> bool {
        self.alive && self.invuln_timer <= 0.0
    }

    /// This pilot's control scheme, as the enum the rules index by.
    ///
    /// [`Ship::coarse_aim`] is the stored form because
    /// [`crate::bullets::Sweep`] only ever needs the bool; aim assist indexes
    /// [`crate::rules::AimAssistRules::tuning`], which wants the enum.
    #[must_use]
    pub fn aim_profile(&self) -> AimProfile {
        if self.coarse_aim {
            AimProfile::Coarse
        } else {
            AimProfile::Precise
        }
    }
}

// ---------------------------------------------------------------------------
// Projectiles
// ---------------------------------------------------------------------------

/// A bullet in flight.
///
/// **There is one bullet list.** The JS simulates bot bullets twice: `bullets.js`
/// carries a visual bolt that never damages anything for a bot
/// (`bullets.js:95` gates on `isLocal`), while `bot.js:314`
/// (`updateProjectiles`) runs a parallel invisible sim that does the damage with
/// a different hit radius and no swept test. Collapsing them is the single
/// largest correctness win in the port.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bullet {
    /// Stable key, so the renderer can keep a mesh attached to a bolt across
    /// frames instead of rebuilding the scene graph. Allocated from
    /// [`World::next_projectile_key`].
    pub key: u64,
    /// Position at the end of this tick.
    pub pos: Vec3,
    /// Position at the end of the previous tick, for the swept test.
    ///
    /// At 780 u/s and a 50 ms frame a bullet moves 39 units, so a point test
    /// misses a 5-unit asteroid on ~87 % of frames. `bullets.js:75`
    /// (`sweptHit`) exists but is applied to the moon only (`bullets.js:129`).
    pub prev_pos: Vec3,
    /// Velocity. Constant: bullets are unaffected by anything.
    pub vel: Vec3,
    /// Remaining lifetime.
    pub life: f64,
    /// Who fired it. Never damages its owner (`server/index.js:940`).
    pub owner: EntityId,
    /// The owner's team at launch, so a mid-flight team change cannot turn a
    /// bullet friendly. `missiles.js:305` stores the same thing for missiles.
    pub owner_team: Option<Team>,
    /// Whether the owner had coarse aim, so the hit radius follows the shot
    /// rather than being re-derived at impact.
    pub owner_coarse_aim: bool,
    /// Damage on hit. Carried per bullet so the boss's turret rounds
    /// ([`crate::rules::WeaponRules::boss_bullet_damage`]) share this list
    /// instead of needing the separate `bossBullets` array at `main.js:2327`.
    pub damage: i32,
}

/// What a missile is chasing.
///
/// The flare case is the interesting one: a missile that gets within
/// [`crate::rules::WeaponRules::flare_seduction_dist`] of someone else's flare
/// retargets onto it (`missiles.js:316`), which is the entire counter-measure
/// mechanic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissileTarget {
    /// Homing on a ship.
    Ship(EntityId),
    /// Seduced by a flare, keyed by [`Flare::key`].
    Flare(u64),
}

/// A missile in flight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Missile {
    /// Stable key for renderer mesh reuse.
    pub key: u64,
    /// Position.
    pub pos: Vec3,
    /// Unit facing. Velocity is always `dir * missile_speed`; a missile has no
    /// independent momentum (`missiles.js:352`).
    pub dir: Vec3,
    /// Current target, or `None` if it lost lock (`missiles.js:331`). An
    /// unlocked missile flies straight until it expires.
    pub target: Option<MissileTarget>,
    /// Remaining lifetime; detonates at zero.
    pub life: f64,
    /// Time since launch, used only by the visual exhaust pulse.
    pub age: f64,
    /// Who fired it.
    pub owner: EntityId,
    /// The owner's team at launch.
    pub owner_team: Option<Team>,
}

/// A single flare from a countermeasure burst.
///
/// One `Q` press releases [`crate::rules::WeaponRules::flare_count`] of these
/// (`missiles.js:238`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Flare {
    /// Stable key, referenced by [`MissileTarget::Flare`].
    pub key: u64,
    /// Position.
    pub pos: Vec3,
    /// Velocity. Decays as `vel *= flare_drag^dt` (`missiles.js:467`).
    pub vel: Vec3,
    /// Remaining burn time.
    pub life: f64,
    /// Time since release, driving the visual flicker.
    pub age: f64,
    /// Who released it. A missile ignores its own owner's flares
    /// (`missiles.js:321`), otherwise flares would be a suicide button.
    pub owner: EntityId,
}

// ---------------------------------------------------------------------------
// World geometry
// ---------------------------------------------------------------------------

/// Asteroid size tier. Index into [`crate::rules::ASTEROID_TIERS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AsteroidTier {
    /// 5–7 units, 5 HP, 45 % of the field.
    #[default]
    Small,
    /// 9–15 units, 10 HP, 30 %.
    Medium,
    /// 18–30 units, 30 HP, 18 %.
    Big,
    /// 38–55 units, 50 HP, 7 %.
    Huge,
}

impl AsteroidTier {
    /// Row index in [`crate::rules::ASTEROID_TIERS`].
    #[must_use]
    pub fn index(self) -> usize {
        match self {
            AsteroidTier::Small => 0,
            AsteroidTier::Medium => 1,
            AsteroidTier::Big => 2,
            AsteroidTier::Huge => 3,
        }
    }

    /// Tier from a row index. Out-of-range indices clamp to [`Self::Huge`],
    /// matching the JS weighted pick, which falls through to the last row when
    /// the accumulated weight comes up short (`asteroids.js:27`).
    #[must_use]
    pub fn from_index(i: usize) -> AsteroidTier {
        match i {
            0 => AsteroidTier::Small,
            1 => AsteroidTier::Medium,
            2 => AsteroidTier::Big,
            _ => AsteroidTier::Huge,
        }
    }
}

/// One asteroid.
///
/// **The world owns this in every mode.** In multiplayer the server is
/// authoritative for `hp` (`asteroid-hit` in, `asteroid-hp` /
/// `asteroid-destroyed` out); in solo nothing is. Both drive the same field
/// here, and [`Authority`] decides only whether the result is also reported. The
/// JS learned this the hard way — see [`Authority`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Asteroid {
    /// Stable id.
    ///
    /// Normalised to **0-based** for every mode. The JS disagrees with itself:
    /// the server allocates `id: i` from 0 (`server/index.js:574`) while both
    /// client generators start at 1 (`asteroids.js:184`, `main.js:239`), so id
    /// `0` exists only in multiplayer. Not a live bug — every comparison is an
    /// explicit `!== null` — but it is exactly the kind of thing a truthiness
    /// check or a `NonZeroU32` would trip over.
    pub id: u32,
    /// Centre.
    pub pos: Vec3,
    /// Mesh scale.
    pub size: f64,
    /// Collision radius, `size * `[`crate::rules::AsteroidFieldRules::collision_radius_scale`].
    /// Stored rather than recomputed because every collision query reads it.
    pub radius: f64,
    /// Remaining hit points.
    pub hp: i32,
    /// Size tier, which fixed `hp` at generation and fixes the spin scale.
    pub tier: AsteroidTier,
    /// Which deformed-icosahedron mesh to draw. Simulation state only because
    /// every client must pick the same one.
    pub variant: u8,
    /// Current Euler rotation. Cosmetic — collision uses [`Self::radius`], so a
    /// rock is a sphere no matter how it is drawn.
    pub rot: Vec3,
    /// Angular velocity, already scaled by the tier's spin scale. Cosmetic.
    /// Apply the tier scale exactly once; see
    /// [`crate::rules::AsteroidTierSpec::spin_scale`].
    pub spin: Vec3,
    /// Cosmetic damage flash, decays at 4/s (`asteroids.js:101`).
    pub hit_flash: f64,
}

/// A spherical obstacle. Only the moon today (`main.js:184`).
///
/// Distinct from an asteroid: it has no HP, it cannot be destroyed, and
/// touching it is instant death rather than 15–29 damage (`main.js:2244`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Obstacle {
    /// Centre.
    pub pos: Vec3,
    /// Radius.
    pub radius: f64,
}

/// An axis-aligned box a ship is pushed out of: a mothership or an airfield.
///
/// `main.js:163` (`motherships`). Ships bounce off these
/// ([`crate::rules::CombatRules::box_collision_restitution`]) rather than
/// taking damage.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxVolume {
    /// Centre.
    pub pos: Vec3,
    /// Half-extents.
    pub half: Vec3,
}

// ---------------------------------------------------------------------------
// Aim assist
// ---------------------------------------------------------------------------

/// Aim assist's per-ship memory. `main.js:2027` (`applyAimAssist`).
///
/// One instance, on [`World`], for the local player: assist is a client-side
/// aiming aid, and a headless server with no [`World::local_id`] never runs it.
/// See [`crate::aim_assist`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AimAssistState {
    /// Whether assist is switched on. Forced on for coarse-aim pilots
    /// (`main.js:996`); see [`crate::aim_assist::update`], which rewrites this
    /// field rather than testing the profile at every read.
    pub enabled: bool,
    /// Smoothed assist strength, so engaging and releasing is not a step.
    pub strength_smoothed: f64,
    /// The target assist is currently holding, if any. Held targets get
    /// [`crate::rules::AimAssistTuning::sticky_dot_bonus`] so the assist does
    /// not flicker between two candidates at similar angles.
    ///
    /// This is `main.js`'s `lastAssistTargetId`, and it deliberately outlives
    /// [`AimAssistState::has_target`]: releasing the assist by steering hard
    /// keeps the memory of who you were on, so reacquiring after the input
    /// settles snaps back to the same ship instead of picking afresh.
    pub target: Option<EntityId>,
    /// Whether the assist is *currently* engaged on [`AimAssistState::target`].
    /// `main.js`'s `assistHasTarget`. This, not `target`, is what the HUD lock
    /// reads — see [`HudState::assist_target`].
    pub has_target: bool,
    /// Unit direction toward the intercept point of the held target.
    pub target_dir: Vec3,
}

impl AimAssistState {
    /// The target the HUD should draw a lock on: the held target, but only
    /// while the assist is actually engaged on it.
    #[must_use]
    pub fn locked_target(&self) -> Option<EntityId> {
        if self.has_target {
            self.target
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Mode state
// ---------------------------------------------------------------------------

/// Time-trial state. `main.js:360`–`:368`.
#[derive(Debug, Clone, PartialEq)]
pub struct TrialsState {
    /// Which trial, 1–4.
    pub trial: u8,
    /// Checkpoint ring positions in lap order, from
    /// [`crate::rules::trial_checkpoints`]. Copied in so a custom track is a
    /// `World` change, not a code change.
    pub checkpoints: Vec<Vec3>,
    /// Index of the next checkpoint to pass.
    pub next_cp: usize,
    /// Elapsed time on the current lap.
    pub timer: f64,
    /// Completed laps.
    pub lap: u32,
    /// Whether the clock is running. Starts on the first pass of checkpoint 0.
    pub running: bool,
    /// Best lap this session, in seconds. Persisting it is the JS's job
    /// (`localStorage`, `main.js:1765`).
    pub best_lap: Option<f64>,
    /// Most recent completed lap.
    pub last_lap: Option<f64>,
    /// Dead time before the next checkpoint can trigger.
    pub cp_cooldown: f64,
    /// Pre-race countdown. While positive the whole simulation tick is skipped
    /// (`main.js:1198` returns early), which is a real control-flow rule, not
    /// just a HUD effect.
    pub countdown: f64,
    /// Whether the countdown is running.
    pub countdown_active: bool,
}

/// One capital ship turret. `main.js:2649`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Turret {
    /// Pivot position in capital-ship local space.
    pub local_pos: Vec3,
    /// Current yaw, solved toward the player each tick.
    pub yaw: f64,
    /// Current pitch, clamped to
    /// ±[`crate::rules::CampaignRules::turret_pitch_limit`].
    pub pitch: f64,
    /// Countdown to the next round. Reload shortens as the boss loses HP.
    pub fire_timer: f64,
}

/// Which part of a campaign mission is running. `main.js:2320`
/// (`campaignPhase`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CampaignPhase {
    /// Fighting wave `0`, `1`, or `2`.
    #[default]
    Wave,
    /// Fighting the capital ship.
    Boss,
    /// Mission complete.
    Victory,
    /// Out of lives.
    Failed,
}

/// Campaign mission state. `main.js:2320`–`:2338`.
///
/// Note what is *not* modelled: `bossFireTimer` (`main.js:2328`, written at
/// `:2730`, never read) and `fireFromBoss` (`main.js:2749`, never called). Both
/// are dead in the JS and are not ported.
#[derive(Debug, Clone, PartialEq)]
pub struct CampaignState {
    /// Mission number, 1–3.
    pub mission: u8,
    /// Current phase.
    pub phase: CampaignPhase,
    /// Wave index within the mission, 0–2.
    pub wave_index: usize,
    /// Ids of the current wave's bots. A `Vec` rather than a set: it holds
    /// single digits and set iteration order is a determinism hazard.
    pub wave_bot_ids: Vec<EntityId>,
    /// How many of them are still alive, cached so the HUD is not recomputed
    /// every tick.
    pub bots_alive: u32,
    /// Whether the between-waves pause is running.
    pub between: bool,
    /// Countdown for that pause.
    pub between_timer: f64,
    /// Lives remaining.
    pub lives: i32,
    /// Where the player respawns, moved forward as waves clear.
    /// `main.js:2869`.
    pub checkpoint_pos: Vec3,
    /// Next id to hand a campaign bot. `main.js:2330`.
    pub next_bot_id: EntityId,
    /// Warp-in effect countdown after a death. `main.js:3252`.
    pub warp_timer: f64,

    /// Boss hit points, mirrored onto the centre hitbox for the HUD
    /// (`main.js:2723`).
    pub boss_hp: i32,
    /// Whether the boss is engaged.
    pub boss_active: bool,
    /// Capital ship position, drifting on two sines. `main.js:2659`.
    pub boss_pos: Vec3,
    /// Accumulated boss animation time, which is the argument to those sines.
    ///
    /// Kept separate from [`World::time`] because it only advances while the
    /// boss is active (`main.js:2658`), so the drift starts from zero at
    /// engagement rather than from wherever the match clock happens to be.
    pub boss_time: f64,
    /// The four turrets.
    pub turrets: [Turret; 4],
}

/// A scoreboard row.
///
/// No name field: pilot names are strings that never need to enter the
/// simulation. The JS lobby already holds the id→name map and renders from it
/// (`main.js:576`, `scores`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Score {
    /// Entity this row belongs to.
    pub id: EntityId,
    /// Team, if assigned.
    pub team: Option<Team>,
    /// Kills.
    pub kills: u32,
    /// Deaths.
    pub deaths: u32,
}

/// Match clock and scoring. `main.js:2264`–`:2271`.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchState {
    /// Seconds remaining. In multiplayer this is overwritten by the server's
    /// `match-state` message (`main.js:876`) rather than counted down locally.
    pub timer: f64,
    /// Kills per team.
    pub team_kills: [u32; 2],
    /// Whether the match has ended.
    pub over: bool,
    /// Whether the clock and team kills apply at all. False in trials,
    /// campaign, and the tutorial. `main.js:2270`.
    pub active: bool,
    /// Bots the local player has killed, for the post-match credit award.
    /// `main.js:2271`.
    pub solo_bots_killed: u32,
    /// Scoreboard rows, in insertion order.
    pub scores: Vec<Score>,
}

// ---------------------------------------------------------------------------
// RNG
// ---------------------------------------------------------------------------

/// Stream selector for asteroid field generation.
pub const RNG_STREAM_FIELD: u64 = 1;
/// Stream selector for spawn positions and jitter.
pub const RNG_STREAM_SPAWN: u64 = 2;
/// Stream selector for combat rolls (collision damage, turret spread).
pub const RNG_STREAM_COMBAT: u64 = 3;
/// Stream selector for bot decisions (aim wander, evade axis, missile delay).
pub const RNG_STREAM_BOTS: u64 = 4;
/// Stream selector for cosmetic-but-shared randomness (flare directions,
/// asteroid variants).
pub const RNG_STREAM_EFFECTS: u64 = 5;

/// The world's randomness, split into independent streams.
///
/// Every `Math.random()` on a simulation path becomes a draw from one of these.
/// The JS call sites are asteroid generation (`asteroids.js:148`,
/// `main.js:253`, `server/index.js:555`), collision damage (`main.js:2215`),
/// flare directions (`missiles.js:239`), bot aim wander and evade axis
/// (`bot.js:167`, `bot.js:57`), bot missile delay (`bot.js:25`), boss turret
/// spread and reload (`main.js:2684`, `:2692`), and every spawn jitter
/// (`main.js:2708`, `:3293`, `:3331`; `server/index.js:499`).
///
/// # Why five streams and not one
///
/// With a single stream, adding one draw anywhere shifts every subsequent draw
/// everywhere — so a bug fix in bot aiming silently regenerates the asteroid
/// field, and a replay recorded before the fix no longer reproduces. Separate
/// streams confine that blast radius to one subsystem. All five derive from one
/// match seed, so the server still ships a single `u64` and both sides
/// reconstruct the identical world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldRng {
    /// The seed all streams derive from. Kept so a match can be reproduced from
    /// the world state alone.
    pub seed: u64,
    /// Asteroid field layout.
    pub field: Rng,
    /// Spawn positions and jitter.
    pub spawn: Rng,
    /// Combat rolls.
    pub combat: Rng,
    /// Bot decisions.
    pub bots: Rng,
    /// Shared cosmetic randomness.
    pub effects: Rng,
}

impl WorldRng {
    /// Derives all five streams from one match seed.
    #[must_use]
    pub fn from_seed(seed: u64) -> WorldRng {
        WorldRng {
            seed,
            field: Rng::with_stream(seed, RNG_STREAM_FIELD),
            spawn: Rng::with_stream(seed, RNG_STREAM_SPAWN),
            combat: Rng::with_stream(seed, RNG_STREAM_COMBAT),
            bots: Rng::with_stream(seed, RNG_STREAM_BOTS),
            effects: Rng::with_stream(seed, RNG_STREAM_EFFECTS),
        }
    }
}

// ---------------------------------------------------------------------------
// World
// ---------------------------------------------------------------------------

/// The complete authoritative state of one match.
///
/// # Why `Vec` and not `HashMap`
///
/// Entity iteration order is load-bearing. "First hit wins" in `bullets.js:138`
/// and `missiles.js:395` walks a JS `Map`, which iterates in insertion order, so
/// which of two overlapping ships eats a bullet depends on join order. A
/// `HashMap` would make that depend on the process's random hash seed instead —
/// a determinism bug that only shows up as rare desyncs. `Vec` preserves the
/// existing behaviour and is faster at these sizes.
#[derive(Debug, Clone, PartialEq)]
pub struct World {
    /// Every tunable. Copied in at construction; never read from anywhere else.
    pub rules: Rules,
    /// The world's randomness.
    pub rng: WorldRng,

    /// Seconds elapsed since the match began.
    ///
    /// Advanced only by the tick's `dt`. This replaces `performance.now()` in
    /// the remote-velocity estimator (`main.js:831`), which is wall clock and
    /// therefore forbidden here.
    pub time: f64,
    /// Ticks elapsed. Cheap monotonic sequence number for frame identity and
    /// replay indexing.
    pub tick: u64,

    /// What kind of match this is.
    pub mode: Mode,
    /// Which map.
    pub map: MapKind,
    /// Who owns hit points. See [`Authority`].
    pub authority: Authority,
    /// The player on this machine, if any. A headless server has none.
    pub local_id: Option<EntityId>,

    /// Every ship, in insertion order.
    pub ships: Vec<Ship>,
    /// Bullets in flight, including the boss's turret rounds.
    pub bullets: Vec<Bullet>,
    /// Missiles in flight.
    pub missiles: Vec<Missile>,
    /// Burning flares.
    pub flares: Vec<Flare>,
    /// The asteroid field, owned here in every mode.
    pub asteroids: Vec<Asteroid>,
    /// Indestructible spheres — the moon.
    pub obstacles: Vec<Obstacle>,
    /// Solid boxes — motherships or airfields.
    pub boxes: Vec<BoxVolume>,

    /// Match clock and scoring.
    pub match_state: MatchState,
    /// Aim assist memory for the local player.
    pub aim_assist: AimAssistState,
    /// Trials state, present only in [`Mode::Trials`].
    pub trials: Option<TrialsState>,
    /// Campaign state, present only in [`Mode::Campaign`].
    pub campaign: Option<CampaignState>,

    /// Next key to hand a bullet, missile, or flare.
    ///
    /// Monotonic and never reused within a match, so the renderer can cache a
    /// mesh against a key without ever seeing it point at a different
    /// projectile. Part of the world state because it must survive a snapshot.
    pub next_projectile_key: u64,
    /// Next id to hand a locally spawned asteroid.
    pub next_asteroid_id: u32,
}

impl World {
    /// An empty world with the static geometry for `map` already in place.
    ///
    /// No ships, no asteroids: those come from a spawn pass and the field
    /// generator, which are other modules' work. What this does own is the
    /// invariant that the moon exists on the space map and nowhere else, and
    /// that both platform boxes are present with the right half-extents — the
    /// facts the asteroid generator needs before it can avoid them.
    #[must_use]
    pub fn new(seed: u64, rules: Rules, mode: Mode, map: MapKind) -> World {
        let mut obstacles = Vec::new();
        let mut boxes = Vec::new();
        match map {
            MapKind::Space => {
                obstacles.push(Obstacle {
                    pos: rules.world.moon_pos,
                    radius: rules.world.moon_radius,
                });
                for z in [-rules.world.mothership_z, rules.world.mothership_z] {
                    boxes.push(BoxVolume {
                        pos: Vec3::new(0.0, 0.0, z),
                        half: rules.world.mothership_half,
                    });
                }
            }
            MapKind::Terrain => {
                for z in [-rules.world.airfield_z, rules.world.airfield_z] {
                    boxes.push(BoxVolume {
                        pos: Vec3::new(0.0, 0.0, z),
                        half: rules.world.airfield_half,
                    });
                }
            }
        }

        World {
            rules,
            rng: WorldRng::from_seed(seed),
            time: 0.0,
            tick: 0,
            mode,
            map,
            authority: if mode.is_solo() {
                Authority::Local
            } else {
                Authority::Server
            },
            local_id: None,
            ships: Vec::new(),
            bullets: Vec::new(),
            missiles: Vec::new(),
            flares: Vec::new(),
            asteroids: Vec::new(),
            obstacles,
            boxes,
            match_state: MatchState {
                timer: rules.match_duration(matches!(mode, Mode::Training)),
                team_kills: [0, 0],
                over: false,
                active: mode.has_match_clock(),
                solo_bots_killed: 0,
                scores: Vec::new(),
            },
            aim_assist: AimAssistState::default(),
            trials: None,
            campaign: None,
            next_projectile_key: 1,
            next_asteroid_id: 0,
        }
    }

    /// The ship with this id.
    #[must_use]
    pub fn ship(&self, id: EntityId) -> Option<&Ship> {
        self.ships.iter().find(|s| s.id == id)
    }

    /// The ship with this id, mutably.
    pub fn ship_mut(&mut self, id: EntityId) -> Option<&mut Ship> {
        self.ships.iter_mut().find(|s| s.id == id)
    }

    /// The asteroid with this id.
    #[must_use]
    pub fn asteroid(&self, id: u32) -> Option<&Asteroid> {
        self.asteroids.iter().find(|a| a.id == id)
    }

    /// The asteroid with this id, mutably.
    pub fn asteroid_mut(&mut self, id: u32) -> Option<&mut Asteroid> {
        self.asteroids.iter_mut().find(|a| a.id == id)
    }

    /// The local player's ship, if this world has one.
    #[must_use]
    pub fn local_ship(&self) -> Option<&Ship> {
        self.local_id.and_then(|id| self.ship(id))
    }

    /// Allocates the next projectile key.
    pub fn take_projectile_key(&mut self) -> u64 {
        let key = self.next_projectile_key;
        self.next_projectile_key += 1;
        key
    }

    /// Whether `shooter` may damage `target`.
    ///
    /// One function for the rule that is currently written five times:
    /// `bullets.js:140`, `missiles.js:397`, `main.js:1395`,
    /// `server/index.js:940` and `:932`. No self-damage, no friendly fire, and
    /// no hitting a ship that is dead or still inside its spawn window — the
    /// last clause being the one solo bots do not get today.
    #[must_use]
    pub fn can_damage(&self, shooter: EntityId, target: &Ship) -> bool {
        if shooter == target.id {
            return false;
        }
        if !target.is_damageable() {
            return false;
        }
        match (self.ship(shooter).and_then(|s| s.team), target.team) {
            (Some(a), Some(b)) => a != b,
            _ => true,
        }
    }
}

// ---------------------------------------------------------------------------
// Tick inputs
// ---------------------------------------------------------------------------

/// One frame of player intent.
///
/// **Intent only, never derived state.** Everything here is something a human
/// pressed. The deadzone, the response curve, and the arrow-key ramp are
/// simulation rules ([`crate::rules::ShipRules`]) and are applied inside the
/// tick, not by the input layer — otherwise two clients with different browsers
/// would fly differently.
///
/// Carries its own `id` so the tick takes a flat `&[Input]`: a headless server
/// ticks many players at once, and a `(id, Input)` tuple slice would not be
/// copyable as one contiguous block.
///
/// Replaces the `input.keys.has(...)` reads at `main.js:1200`–`:1254` and
/// `main.js:1354`–`:1463`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct Input {
    /// Which ship this input drives.
    pub id: EntityId,

    /// Horizontal aim, `-1..1`, raw. Deadzone and curve are applied in the
    /// tick (`main.js:1225`–`:1228`).
    pub steer_x: f64,
    /// Vertical aim, `-1..1`, raw. Negative is nose-up.
    pub steer_y: f64,
    /// Roll, `-1..1`. A/D or the gamepad roll axis (`main.js:1249`).
    pub roll: f64,
    /// Arrow-key horizontal target, `-1`, `0`, or `1`. Ramped rather than
    /// applied directly (`main.js:1230`).
    pub arrow_x: f64,
    /// Arrow-key vertical target, `-1`, `0`, or `1`.
    pub arrow_y: f64,
    /// Fine-aim modifier (Q), which slows the arrow ramp. `main.js:1234`.
    pub arrow_fine: bool,

    /// Accumulated mouse-wheel notches this frame; each is worth
    /// [`crate::rules::ShipRules::throttle_step`]. `main.js:1209`.
    pub throttle_notches: f64,
    /// Continuous throttle axis, `-1..1`, from W/S or a gamepad trigger.
    /// `main.js:1211`.
    pub throttle_axis: f64,
    /// Absolute throttle from the touch HUD, `0..1`, which overrides the other
    /// two when present. `main.js:1205`.
    pub throttle_override: Option<f64>,

    /// Gun trigger held.
    pub fire: bool,
    /// Brake/drift held (Space). `main.js:1200`.
    pub braking: bool,
    /// Boost held (Shift). `main.js:1262`.
    pub boost: bool,
    /// Hard-stop modifier: braking *and* S, which uses
    /// [`crate::rules::ShipRules::drift_brake`] instead of `drift_drag`.
    /// `main.js:1284`.
    pub hard_brake: bool,
    /// Free-look held, which suppresses steering. `main.js:1220`.
    pub free_look: bool,

    /// Launch a missile. Edge-triggered; the input layer debounces
    /// (`main.js:1390`).
    pub fire_missile: bool,
    /// Release a flare burst. Edge-triggered (`main.js:1446`).
    pub deploy_flare: bool,
    /// Switch gun. Edge-triggered (`main.js:1034`).
    pub toggle_gun: bool,
    /// Switch aim assist. Edge-triggered (`main.js:999`).
    pub toggle_aim_assist: bool,
}

/// Authoritative state arriving from the server.
///
/// Mirrors the WebSocket message handler at `main.js:827`–`:975`. Every variant
/// is `Copy` with a fixed payload so a batch is one contiguous slice — the
/// server's `players` message, which is a variable-length array in JSON, is
/// delivered as one [`NetEvent::PlayerRow`] per player instead.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NetEvent {
    /// A remote ship's pose. `main.js:827`.
    RemoteState {
        /// Whose.
        id: EntityId,
        /// Position.
        pos: Vec3,
        /// Orientation.
        quat: Quat,
        /// Whether they reported boosting.
        boost: bool,
    },
    /// Authoritative hit points. `main.js:888` (`hp`).
    Hp {
        /// Whose.
        id: EntityId,
        /// New value.
        hp: i32,
    },
    /// A ship died. `main.js:901` (`death`).
    Death {
        /// Who died.
        id: EntityId,
        /// Who killed them, if anyone. `null` for self-damage
        /// (`server/index.js:894`).
        killer: Option<EntityId>,
    },
    /// A ship respawned at a server-chosen pose. `main.js:917` (`respawn`).
    Respawn {
        /// Who.
        id: EntityId,
        /// Where.
        pos: Vec3,
        /// Facing.
        quat: Quat,
    },
    /// Someone fired. One event per shot. `main.js:926` (`fire`).
    Fired {
        /// Shooter.
        id: EntityId,
        /// Which weapon.
        weapon: WeaponKind,
        /// Muzzle position.
        origin: Vec3,
        /// Direction, or the beam's endpoint for [`WeaponKind::Beam`].
        dir: Vec3,
        /// Missile lock, if this was a missile.
        target: Option<EntityId>,
    },
    /// Someone released flares. `main.js:955` (`flare`).
    FlareBurst {
        /// Who.
        id: EntityId,
        /// Where.
        pos: Vec3,
        /// Their facing, which orients the burst cone.
        quat: Quat,
    },
    /// One scoreboard row. `main.js:858` (`players`).
    PlayerRow {
        /// Who.
        id: EntityId,
        /// Their team, if assigned.
        team: Option<Team>,
        /// Kills.
        kills: u32,
        /// Deaths.
        deaths: u32,
    },
    /// Match clock and score update. `main.js:875` (`match-state`).
    MatchState {
        /// Seconds remaining.
        timer: f64,
        /// Kills per team.
        team_kills: [u32; 2],
    },
    /// The match is over. `main.js:882` (`match-end`).
    MatchEnd {
        /// Winning team, or `None` for a draw (`server/index.js:450` sends
        /// `-1`).
        winner: Option<Team>,
        /// Final kills per team.
        team_kills: [u32; 2],
    },
    /// Authoritative asteroid hit points. `main.js:965` (`asteroid-hp`).
    AsteroidHp {
        /// Which rock.
        id: u32,
        /// New value.
        hp: i32,
    },
    /// An asteroid was destroyed. `main.js:967` (`asteroid-destroyed`).
    AsteroidDestroyed {
        /// Which rock.
        id: u32,
    },
    /// A player left. `main.js:854` (`disconnect`).
    Disconnect {
        /// Who.
        id: EntityId,
    },
}

/// Which weapon fired or landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WeaponKind {
    /// Travelling bolt.
    #[default]
    Bullet,
    /// Hitscan beam.
    Beam,
    /// Homing missile.
    Missile,
}

// ---------------------------------------------------------------------------
// Tick outputs
// ---------------------------------------------------------------------------

/// Something the tick did that the outside world cares about.
///
/// These drive particles, audio, HUD toasts, and the kill feed. They are
/// reported, never acted on from in here — this crate does not know what a
/// sound is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SimEvent {
    /// A gun fired. Spawns the muzzle flash and the shot sound.
    Fired {
        /// Shooter.
        owner: EntityId,
        /// Which weapon.
        weapon: WeaponKind,
        /// Muzzle position.
        origin: Vec3,
        /// Direction, or the beam's endpoint for [`WeaponKind::Beam`].
        dir: Vec3,
    },
    /// A flare burst was released.
    FlareBurst {
        /// Who.
        owner: EntityId,
        /// Where.
        origin: Vec3,
    },
    /// Something exploded.
    Explosion {
        /// Where.
        pos: Vec3,
        /// Rough size, in world units.
        scale: f64,
        /// What kind, so the renderer picks the right particle preset.
        kind: ExplosionKind,
    },
    /// A ship took damage.
    Damaged {
        /// Who.
        id: EntityId,
        /// How much.
        amount: i32,
        /// Hit points after.
        new_hp: i32,
        /// Who dealt it, if anyone.
        source: Option<EntityId>,
    },
    /// A ship was destroyed.
    ShipDestroyed {
        /// Who.
        id: EntityId,
        /// Who killed them.
        killer: Option<EntityId>,
        /// Where they died.
        pos: Vec3,
    },
    /// A ship respawned.
    ShipRespawned {
        /// Who.
        id: EntityId,
        /// Where.
        pos: Vec3,
    },
    /// An asteroid lost hit points.
    AsteroidDamaged {
        /// Which rock.
        id: u32,
        /// Hit points after.
        hp: i32,
    },
    /// An asteroid was destroyed.
    AsteroidDestroyed {
        /// Which rock.
        id: u32,
        /// Where.
        pos: Vec3,
        /// Its collision radius, which sizes the debris burst.
        radius: f64,
    },
    /// The local player passed a trials checkpoint.
    CheckpointPassed {
        /// Which one.
        index: usize,
        /// Lap time, if this pass completed a lap.
        lap_time: Option<f64>,
    },
    /// A trials lap completed.
    LapComplete {
        /// Lap time.
        time: f64,
        /// Whether it beat the previous best.
        is_best: bool,
    },
    /// A campaign wave was cleared.
    WaveComplete {
        /// Which wave.
        index: usize,
    },
    /// The capital ship engaged.
    BossPhaseStarted,
    /// The mission was lost.
    CampaignFailed,
    /// The mission was won.
    CampaignVictory {
        /// Lives left, which sets the award.
        lives_left: i32,
    },
    /// The match ended.
    MatchEnded {
        /// Winning team, or `None` for a draw.
        winner: Option<Team>,
    },
}

/// Which particle preset an explosion uses. Cosmetic classification only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExplosionKind {
    /// A bullet impact.
    #[default]
    Impact,
    /// A ship dying.
    ShipDeath,
    /// A rock breaking up.
    AsteroidBreak,
    /// A missile detonating.
    MissileHit,
    /// A flare igniting.
    FlareBurst,
}

/// Something the tick wants sent to the server.
///
/// The simulation never touches a socket. It says what should go out; the JS
/// transport serialises it to the existing JSON messages and sends it. Mirrors
/// the `ws.send` sites at `main.js:1495`, `:1501`, `:1520`, `:1697`, `:1712`,
/// `:1319`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NetIntent {
    /// Periodic pose update, at
    /// [`crate::rules::MatchRules::state_send_interval`].
    State {
        /// Position.
        pos: Vec3,
        /// Orientation.
        quat: Quat,
        /// Whether boosting, for remote trail effects.
        boost: bool,
    },
    /// A shot was fired, for other clients to draw.
    Fire {
        /// Which weapon.
        weapon: WeaponKind,
        /// Muzzle position.
        origin: Vec3,
        /// Direction, or the beam endpoint.
        dir: Vec3,
        /// Missile lock, if any.
        target: Option<EntityId>,
    },
    /// A flare burst was released.
    Flare {
        /// Where.
        pos: Vec3,
        /// Facing.
        quat: Quat,
    },
    /// A hit is claimed on another ship.
    ///
    /// The server applies damage from this message with **no validation at
    /// all** (`server/index.js:910`–`:936` trusts the sender's target and
    /// weapon, rate-limiting only). Hit radii are therefore the entire
    /// anti-cheat story, which is a further reason for there to be exactly one
    /// of them.
    Hit {
        /// Who was hit.
        target: EntityId,
        /// With what.
        weapon: WeaponKind,
        /// Set when a locally driven bot landed the shot rather than the
        /// player. `server/index.js:917` (`fromBotId`).
        from_bot: Option<EntityId>,
    },
    /// A hit is claimed on an asteroid.
    AsteroidHit {
        /// Which rock.
        id: u32,
    },
    /// Damage the player did to themselves — brake overcharge, or flying into
    /// something.
    SelfDamage {
        /// How much.
        amount: i32,
    },
    /// A locally driven bot's pose, relayed to other clients.
    BotState {
        /// Which bot.
        id: EntityId,
        /// Position.
        pos: Vec3,
        /// Orientation.
        quat: Quat,
    },
}

// ---------------------------------------------------------------------------
// Frame — the renderer's view
// ---------------------------------------------------------------------------

/// Per-ship render flags, packed into one word.
///
/// A bitset rather than eight `bool` fields: it keeps [`ShipView`] to a tidy
/// fixed stride, and eight separately-padded bytes would cost more than the
/// four this takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct ShipFlags(u32);

impl ShipFlags {
    /// No flags set.
    pub const NONE: ShipFlags = ShipFlags(0);
    /// The ship is alive; a dead ship's mesh is hidden.
    pub const ALIVE: ShipFlags = ShipFlags(1 << 0);
    /// The ship is boosting, for the exhaust trail.
    pub const BOOSTING: ShipFlags = ShipFlags(1 << 1);
    /// The ship is braking, for the brake trail.
    pub const BRAKING: ShipFlags = ShipFlags(1 << 2);
    /// Spawn protection is active, which strobes the hull.
    pub const INVULN: ShipFlags = ShipFlags(1 << 3);
    /// The ship is AI-driven.
    pub const BOT: ShipFlags = ShipFlags(1 << 4);
    /// The ship belongs to the player on this machine.
    pub const LOCAL: ShipFlags = ShipFlags(1 << 5);
    /// The ship is one of the boss's hitboxes, which are never drawn.
    pub const BOSS_HITBOX: ShipFlags = ShipFlags(1 << 6);

    /// The raw bits.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Wraps raw bits. Unknown bits are preserved, so a newer simulation can
    /// set flags an older renderer ignores.
    #[must_use]
    pub const fn from_bits(bits: u32) -> ShipFlags {
        ShipFlags(bits)
    }

    /// The union of `self` and `other`.
    #[must_use]
    pub const fn with(self, other: ShipFlags) -> ShipFlags {
        ShipFlags(self.0 | other.0)
    }

    /// `other` if `cond`, otherwise nothing — for building a set from booleans
    /// without a chain of `if`s.
    #[must_use]
    pub const fn with_if(self, cond: bool, other: ShipFlags) -> ShipFlags {
        if cond {
            self.with(other)
        } else {
            self
        }
    }

    /// Whether every bit in `other` is set.
    #[must_use]
    pub const fn contains(self, other: ShipFlags) -> bool {
        self.0 & other.0 == other.0
    }
}

/// One ship, as the renderer needs it.
///
/// `#[repr(C)]` and `Copy`, so a `&[ShipView]` is a contiguous byte range the
/// WASM layer can expose as a typed array without touching an element.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct ShipView {
    /// Which ship. The renderer keeps its own id→mesh map.
    pub id: i32,
    /// Team index, or `-1` if unassigned.
    pub team: i32,
    /// Hit points.
    pub hp: i32,
    /// Render flags.
    pub flags: ShipFlags,
    /// Position.
    pub pos: [f32; 3],
    /// Orientation, `(x, y, z, w)`.
    pub quat: [f32; 4],
    /// Velocity, for trail emission and the speed readout.
    pub vel: [f32; 3],
    /// Damage flash, `0..1`.
    pub hit_flash: f32,
}

/// One bullet or missile, as the renderer needs it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct ProjView {
    /// Stable key. Lets the renderer keep one mesh per projectile across
    /// frames instead of rebuilding, and detect which ones vanished.
    pub key: u64,
    /// Position.
    pub pos: [f32; 3],
    /// Unit facing, which orients the bolt or the missile body.
    pub dir: [f32; 3],
}

/// One burning flare.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct FlareView {
    /// Stable key.
    pub key: u64,
    /// Position.
    pub pos: [f32; 3],
    /// Time since release, which drives the flicker.
    pub age: f32,
    /// Remaining burn time, which drives the fade.
    pub life: f32,
}

/// One asteroid.
///
/// Sent every frame for simplicity. If 60 rocks × 44 bytes ever shows up in a
/// profile, the fix is a dirty flag on [`Asteroid`] and a shorter list — not a
/// nested or indexed structure.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct RockView {
    /// Which rock.
    pub id: u32,
    /// Hit points, for the damage tint.
    pub hp: i32,
    /// Position.
    pub pos: [f32; 3],
    /// Euler rotation.
    pub rot: [f32; 3],
    /// Mesh scale.
    pub size: f32,
    /// Damage flash, `0..1`.
    pub hit_flash: f32,
}

/// The campaign capital ship.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct BossView {
    /// Hull position.
    pub pos: [f32; 3],
    /// Turret yaws, in [`crate::rules::BOSS_TURRET_PIVOTS`] order.
    pub turret_yaw: [f32; 4],
    /// Turret pitches, same order.
    pub turret_pitch: [f32; 4],
    /// Hit points.
    pub hp: i32,
    /// Maximum hit points, so the HUD does not need the rules.
    pub max_hp: i32,
}

/// Everything the HUD and cockpit dash draw.
///
/// Replaces `camTel` (`main.js:430`), which is the existing partial
/// simulation→render snapshot, plus the values `renderMatchHud`,
/// `updateTrialsHud`, and `updateCampaignHud` currently read straight out of
/// closure variables.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct HudState {
    /// Throttle as a fraction of maximum, `0..1`.
    pub throttle01: f32,
    /// Speed in world units per second.
    pub speed: f32,
    /// Hit points.
    pub hp: i32,
    /// Hit points as a fraction of maximum, `0..1`.
    pub hp01: f32,
    /// Ammo as a fraction of capacity, `0..1`.
    pub ammo01: f32,
    /// Boost meter as a fraction of capacity, `0..1`.
    pub boost01: f32,
    /// Brake charge, `0..1`.
    pub charge01: f32,
    /// Brake overcharge, `0..1`, where 1 is taking damage.
    pub overcharge01: f32,
    /// Missiles remaining.
    pub missiles: u8,
    /// Flares remaining.
    pub flares: u8,
    /// Selected gun.
    pub gun_mode: GunMode,
    /// Whether spawn protection is active.
    pub invuln: bool,
    /// Whether an enemy missile is tracking the local player, which triggers
    /// the lock warning. `missiles.js:282` (`isTargetingLocal`).
    pub missile_lock_warning: bool,
    /// The ship aim assist is currently pulling toward, or `-1` for none.
    ///
    /// [`AimAssistState::locked_target`], flattened. Drives the reticle lock,
    /// the lead marker, and the cockpit's TGT annunciator, which is why it goes
    /// back to `-1` the moment the assist releases — including while the player
    /// is steering hard enough to break it.
    pub assist_target: i32,
    /// Seconds left on the match clock.
    pub match_timer: f32,
    /// Kills per team.
    pub team_kills: [u32; 2],
    /// Steering after deadzone and curve, for the camera lean.
    pub steer: [f32; 2],
    /// Trials readout; zeroed outside trials.
    pub trials: TrialsHud,
    /// Campaign readout; zeroed outside the campaign.
    pub campaign: CampaignHud,
}

/// Trials HUD readout.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct TrialsHud {
    /// Whether a trial is running.
    pub active: bool,
    /// Whether the lap clock is running.
    pub running: bool,
    /// Current lap number.
    pub lap: u32,
    /// Elapsed time on this lap.
    pub timer: f32,
    /// Best lap this session, or a negative value if there is none.
    pub best_lap: f32,
    /// Most recent lap, or a negative value if there is none.
    pub last_lap: f32,
    /// Index of the next checkpoint.
    pub next_cp: u32,
    /// World position of the next checkpoint, for the tracer dots.
    pub next_cp_pos: [f32; 3],
    /// Pre-race countdown; zero when not counting down.
    pub countdown: f32,
}

/// Campaign HUD readout.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct CampaignHud {
    /// Whether a campaign mission is running.
    pub active: bool,
    /// Mission number.
    pub mission: u8,
    /// Wave index, 0-based.
    pub wave: u8,
    /// Enemies left in the current wave.
    pub enemies_left: u32,
    /// Lives left.
    pub lives: i32,
    /// Whether the boss is engaged.
    pub boss_active: bool,
    /// Boss hit points as a fraction of maximum, `0..1`.
    pub boss_hp01: f32,
}

/// One tick's output: everything the renderer, the HUD, and the transport need.
///
/// Rebuilt each tick and thrown away. See the module docs for why it is `f32`
/// and why every list is flat.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Frame {
    /// [`World::tick`] after the step, so the renderer can detect a dropped or
    /// repeated frame.
    pub tick: u64,
    /// [`World::time`] after the step.
    pub time: f64,
    /// Every ship, in [`World::ships`] order.
    pub ships: Vec<ShipView>,
    /// Bullets in flight.
    pub bullets: Vec<ProjView>,
    /// Missiles in flight.
    pub missiles: Vec<ProjView>,
    /// Burning flares.
    pub flares: Vec<FlareView>,
    /// The asteroid field.
    pub asteroids: Vec<RockView>,
    /// The capital ship, in the campaign only.
    pub boss: Option<BossView>,
    /// One-shot events for particles, audio, and the HUD.
    pub events: Vec<SimEvent>,
    /// Messages the transport should send.
    pub net_out: Vec<NetIntent>,
    /// HUD and cockpit telemetry.
    pub hud: HudState,
}

impl Frame {
    /// An empty frame.
    #[must_use]
    pub fn new() -> Frame {
        Frame::default()
    }

    /// Empties every list but keeps the allocations.
    ///
    /// The signature in [`TickFn`] returns a `Frame` by value, which is the
    /// honest shape: the caller owns the output. A caller that wants zero
    /// steady-state allocation keeps one `Frame`, calls this, and refills it —
    /// after the first few seconds every buffer has reached its high-water mark
    /// and no tick allocates again.
    pub fn clear(&mut self) {
        self.tick = 0;
        self.time = 0.0;
        self.ships.clear();
        self.bullets.clear();
        self.missiles.clear();
        self.flares.clear();
        self.asteroids.clear();
        self.boss = None;
        self.events.clear();
        self.net_out.clear();
        self.hud = HudState::default();
    }

    /// Total entity records in this frame, across every list. The rough measure
    /// of how much is crossing the boundary.
    #[must_use]
    pub fn entity_count(&self) -> usize {
        self.ships.len()
            + self.bullets.len()
            + self.missiles.len()
            + self.flares.len()
            + self.asteroids.len()
    }
}

// ---------------------------------------------------------------------------
// The tick contract
// ---------------------------------------------------------------------------

/// The signature of one simulation step.
///
/// ```text
/// pub fn tick(
///     world:  &mut World,
///     inputs: &[Input],
///     events: &[NetEvent],
///     dt:     f64,
/// ) -> Frame;
/// ```
///
/// Expressed as a type alias rather than a function because this module defines
/// data, not behaviour — the physics, collision, homing, and AI that make up the
/// body are separate work. A `todo!()` stub would compile and then silently
/// return empty frames to anyone who called it; an alias pins the contract
/// without pretending the implementation exists.
///
/// # Deviations from the shape originally proposed
///
/// - **`dt: f64`, not `f32`.** Everything else in the simulation is `f64` (see
///   the module docs), and [`TICK_DT`] is already `f64`. An `f32` `dt` would be
///   widened on the first multiply, so it would buy nothing and invite a
///   caller to pass a `dt` that is not exactly representable.
/// - **`inputs: &[Input]` with the id inside [`Input`]**, rather than a slice of
///   `(EntityId, Input)` pairs. Keeps the slice one contiguous block of
///   identical records, which is what a headless server ticking many players
///   wants and what a WASM caller can hand over without marshalling.
///
/// # Contract
///
/// - `dt` is a **fixed** step, normally [`TICK_DT`], supplied by the caller.
///   Variable frame time must not reach here: the caller accumulates real time
///   and runs a whole number of ticks, keeping the remainder. Where the JS
///   clamps a variable delta ([`crate::rules::MAX_FRAME_DT`], `main.js:3485`),
///   the clamp becomes a cap on how many ticks one frame may run.
/// - `inputs` holds at most one entry per ship. A ship with no entry coasts on
///   its last state; it does not reset to neutral.
/// - `events` are applied **before** the step, in the order given. Order is
///   significant: an `Hp` following a `Death` for the same ship means something
///   different from the reverse.
/// - The returned [`Frame`] borrows nothing and may outlive the call.
/// - The function is pure in `(world, inputs, events, dt)`. Two worlds that
///   compare equal, given equal inputs, produce equal worlds and equal frames —
///   on any platform, on any run. That is the property the whole crate exists
///   to preserve.
pub type TickFn = fn(&mut World, &[Input], &[NetEvent], f64) -> Frame;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::BOSS_HITBOX_COUNT;

    fn world() -> World {
        World::new(0xC0FFEE, Rules::DEFAULT, Mode::Skirmish, MapKind::Space)
    }

    #[test]
    fn space_world_gets_the_moon_and_both_motherships() {
        let w = world();
        assert_eq!(w.obstacles.len(), 1, "the moon is the only obstacle");
        assert_eq!(w.obstacles[0].radius, 80.0);
        assert_eq!(w.obstacles[0].pos, Vec3::ZERO);
        assert_eq!(w.boxes.len(), 2);
        assert_eq!(w.boxes[0].pos.z, -600.0);
        assert_eq!(w.boxes[1].pos.z, 600.0);
        assert_eq!(w.boxes[0].half, Vec3::new(45.0, 18.0, 35.0));
    }

    #[test]
    fn terrain_world_has_airfields_and_no_moon() {
        let w = World::new(1, Rules::DEFAULT, Mode::Skirmish, MapKind::Terrain);
        assert!(
            w.obstacles.is_empty(),
            "the terrain map has no moon (main.js:182)"
        );
        assert_eq!(w.boxes.len(), 2);
        assert_eq!(w.boxes[0].half, Vec3::new(280.0, 4.0, 190.0));
        assert_eq!(w.boxes[1].pos.z, 1500.0);
    }

    #[test]
    fn authority_follows_the_mode() {
        assert_eq!(world().authority, Authority::Local);
        let mp = World::new(1, Rules::DEFAULT, Mode::Multiplayer, MapKind::Space);
        assert_eq!(mp.authority, Authority::Server);
    }

    #[test]
    fn training_matches_are_shorter_and_trials_have_no_clock() {
        let train = World::new(1, Rules::DEFAULT, Mode::Training, MapKind::Space);
        assert_eq!(train.match_state.timer, 180.0);
        assert!(train.match_state.active);

        let trials = World::new(1, Rules::DEFAULT, Mode::Trials(2), MapKind::Space);
        assert!(!trials.match_state.active, "trials have no match clock");
    }

    #[test]
    fn a_fresh_ship_spawns_whole_and_protected() {
        let rules = Rules::DEFAULT;
        let s = Ship::spawn(7, ShipKind::Local, Vec3::ZERO, Quat::IDENTITY, &rules);
        assert_eq!(s.hp, rules.ship.max_hp);
        assert!(s.alive);
        assert_eq!(s.missiles_left, rules.weapons.missile_max);
        assert_eq!(s.flares_left, rules.weapons.flare_max);
        assert_eq!(s.ammo, rules.weapons.max_ammo);
        assert_eq!(s.boost_meter, rules.ship.max_boost);
        assert_eq!(s.invuln_timer, rules.combat.spawn_invuln);
        assert!(
            !s.is_damageable(),
            "a ship inside its spawn window must not be damageable"
        );
    }

    #[test]
    fn spawn_invulnerability_is_universal_not_player_only() {
        // The JS gates only the local player on an invuln timer
        // (main.js:3232); applyHitToBot (main.js:3201) checks nothing, and bot
        // records never get the field. Every ship kind gets one here.
        let rules = Rules::DEFAULT;
        for kind in [
            ShipKind::Local,
            ShipKind::Remote,
            ShipKind::Bot,
            ShipKind::BossHitbox,
        ] {
            let s = Ship::spawn(1, kind, Vec3::ZERO, Quat::IDENTITY, &rules);
            assert_eq!(s.invuln_timer, 2.0, "{kind:?} missed spawn protection");
            assert!(!s.is_damageable(), "{kind:?} was damageable on spawn");
        }
    }

    #[test]
    fn hit_radius_is_one_number_unless_overridden() {
        let rules = Rules::DEFAULT;
        let mut s = Ship::spawn(1, ShipKind::Bot, Vec3::ZERO, Quat::IDENTITY, &rules);
        assert_eq!(s.hit_radius(&rules, false), 6.0);
        assert_eq!(s.hit_radius(&rules, true), 7.0, "coarse aim widens by 1.0");

        // A boss hitbox is the one legitimate override, and it ignores the
        // shooter's aim profile — the sphere is 28 either way.
        s.hit_radius_override = Some(rules.weapons.boss_hitbox_radius);
        assert_eq!(s.hit_radius(&rules, false), 28.0);
        assert_eq!(s.hit_radius(&rules, true), 28.0);
    }

    #[test]
    fn can_damage_rejects_self_friendlies_dead_and_invulnerable() {
        let rules = Rules::DEFAULT;
        let mut w = world();

        let mut shooter = Ship::spawn(1, ShipKind::Local, Vec3::ZERO, Quat::IDENTITY, &rules);
        shooter.team = Some(Team::Zero);
        shooter.invuln_timer = 0.0;
        let mut ally = Ship::spawn(2, ShipKind::Bot, Vec3::ZERO, Quat::IDENTITY, &rules);
        ally.team = Some(Team::Zero);
        ally.invuln_timer = 0.0;
        let mut enemy = Ship::spawn(3, ShipKind::Bot, Vec3::ZERO, Quat::IDENTITY, &rules);
        enemy.team = Some(Team::One);
        enemy.invuln_timer = 0.0;
        w.ships = vec![shooter, ally, enemy];

        let ally = w.ship(2).unwrap().clone();
        let enemy = w.ship(3).unwrap().clone();
        let me = w.ship(1).unwrap().clone();

        assert!(w.can_damage(1, &enemy), "enemies are fair game");
        assert!(!w.can_damage(1, &ally), "no friendly fire");
        assert!(!w.can_damage(1, &me), "no self-damage");

        let mut fresh = enemy.clone();
        fresh.invuln_timer = 2.0;
        assert!(!w.can_damage(1, &fresh), "spawn protection blocks the hit");

        let mut dead = enemy;
        dead.alive = false;
        assert!(!w.can_damage(1, &dead), "corpses cannot be hit");
    }

    #[test]
    fn unassigned_teams_do_not_block_damage() {
        // server/index.js:941 rejects friendly fire only when the target has a
        // team; an unassigned pair must stay shootable or a match cannot start.
        let rules = Rules::DEFAULT;
        let mut w = world();
        let mut a = Ship::spawn(1, ShipKind::Local, Vec3::ZERO, Quat::IDENTITY, &rules);
        a.invuln_timer = 0.0;
        let mut b = Ship::spawn(2, ShipKind::Bot, Vec3::ZERO, Quat::IDENTITY, &rules);
        b.invuln_timer = 0.0;
        w.ships = vec![a, b];
        let b = w.ship(2).unwrap().clone();
        assert!(w.can_damage(1, &b));
    }

    #[test]
    fn projectile_keys_are_unique_and_monotonic() {
        let mut w = world();
        let keys: Vec<u64> = (0..64).map(|_| w.take_projectile_key()).collect();
        for pair in keys.windows(2) {
            assert!(pair[0] < pair[1], "keys must increase");
        }
        assert_ne!(keys[0], 0, "0 is reserved as 'no projectile'");
    }

    #[test]
    fn lookups_find_ships_and_asteroids_by_id() {
        let rules = Rules::DEFAULT;
        let mut w = world();
        w.ships = vec![
            Ship::spawn(4, ShipKind::Bot, Vec3::ZERO, Quat::IDENTITY, &rules),
            Ship::spawn(9, ShipKind::Bot, Vec3::X, Quat::IDENTITY, &rules),
        ];
        assert_eq!(w.ship(9).map(|s| s.pos), Some(Vec3::X));
        assert!(w.ship(5).is_none());
        w.ship_mut(4).unwrap().hp = 42;
        assert_eq!(w.ship(4).unwrap().hp, 42);

        w.asteroids.push(Asteroid {
            id: 0,
            pos: Vec3::ZERO,
            size: 10.0,
            radius: 9.5,
            hp: 10,
            tier: AsteroidTier::Medium,
            variant: 0,
            rot: Vec3::ZERO,
            spin: Vec3::ZERO,
            hit_flash: 0.0,
        });
        // Id 0 is real: the server allocates 0-based ids, so a truthiness-style
        // check would drop the first rock in every multiplayer match.
        assert!(w.asteroid(0).is_some());
        w.asteroid_mut(0).unwrap().hp = 3;
        assert_eq!(w.asteroid(0).unwrap().hp, 3);
        assert!(w.asteroid(1).is_none());
    }

    #[test]
    fn local_ship_resolves_through_local_id() {
        let rules = Rules::DEFAULT;
        let mut w = world();
        assert!(w.local_ship().is_none());
        w.ships = vec![Ship::spawn(
            11,
            ShipKind::Local,
            Vec3::Y,
            Quat::IDENTITY,
            &rules,
        )];
        w.local_id = Some(11);
        assert_eq!(w.local_ship().map(|s| s.pos), Some(Vec3::Y));
    }

    #[test]
    fn rng_streams_are_reproducible_and_independent() {
        let mut a = WorldRng::from_seed(99);
        let mut b = WorldRng::from_seed(99);
        assert_eq!(a.field.next_u32(), b.field.next_u32());

        // Burning the bot stream must not disturb the asteroid field: that is
        // the entire point of splitting the streams.
        for _ in 0..1000 {
            a.bots.next_u32();
        }
        assert_eq!(a.field.next_u32(), b.field.next_u32());

        let mut fresh = WorldRng::from_seed(99);
        let field: Vec<u32> = (0..8).map(|_| fresh.field.next_u32()).collect();
        let spawn: Vec<u32> = (0..8).map(|_| fresh.spawn.next_u32()).collect();
        assert_ne!(field, spawn, "streams must not overlap");
    }

    #[test]
    fn worlds_from_the_same_seed_are_identical() {
        let a = World::new(7, Rules::DEFAULT, Mode::Campaign(2), MapKind::Space);
        let b = World::new(7, Rules::DEFAULT, Mode::Campaign(2), MapKind::Space);
        assert_eq!(a, b);
        let c = World::new(8, Rules::DEFAULT, Mode::Campaign(2), MapKind::Space);
        assert_ne!(a.rng, c.rng);
    }

    #[test]
    fn boss_hitbox_ids_are_recognised() {
        assert!(!is_boss_hitbox(BOSS_ID_BASE - 1));
        assert!(is_boss_hitbox(BOSS_ID_BASE));
        assert!(is_boss_hitbox(
            BOSS_ID_BASE + BOSS_HITBOX_COUNT as EntityId - 1
        ));
        assert!(!is_boss_hitbox(
            BOSS_ID_BASE + BOSS_HITBOX_COUNT as EntityId
        ));
        // Ordinary ids, including the negative ones the server hands bots.
        assert!(!is_boss_hitbox(1));
        assert!(!is_boss_hitbox(-3));
        assert!(!is_boss_hitbox(100));
    }

    #[test]
    fn teams_are_two_and_oppose_each_other() {
        assert_eq!(Team::Zero.index(), 0);
        assert_eq!(Team::One.index(), 1);
        assert_eq!(Team::Zero.other(), Team::One);
        assert_eq!(Team::One.other(), Team::Zero);
        assert_eq!(Team::from_index(0), Some(Team::Zero));
        assert_eq!(Team::from_index(1), Some(Team::One));
        assert_eq!(Team::from_index(2), None);
        assert_eq!(Team::default(), Team::Zero);
    }

    #[test]
    fn asteroid_tier_indices_round_trip() {
        for tier in [
            AsteroidTier::Small,
            AsteroidTier::Medium,
            AsteroidTier::Big,
            AsteroidTier::Huge,
        ] {
            assert_eq!(AsteroidTier::from_index(tier.index()), tier);
        }
        // The JS weighted pick falls through to the last row when the weights
        // come up short (asteroids.js:27), so out-of-range clamps to Huge.
        assert_eq!(AsteroidTier::from_index(99), AsteroidTier::Huge);
    }

    #[test]
    fn modes_classify_themselves_correctly() {
        assert!(!Mode::Multiplayer.is_solo());
        for mode in [
            Mode::Training,
            Mode::Skirmish,
            Mode::Trials(1),
            Mode::Campaign(1),
            Mode::Tutorial,
        ] {
            assert!(mode.is_solo(), "{mode:?} should be solo");
        }
        assert!(Mode::Skirmish.has_match_clock());
        assert!(!Mode::Trials(1).has_match_clock());
        assert!(!Mode::Campaign(1).has_match_clock());
        // The tutorial is the only mode that suppresses collision self-damage
        // (main.js:2214, :2243, :2255).
        assert!(!Mode::Tutorial.has_collision_damage());
        assert!(Mode::Skirmish.has_collision_damage());
    }

    #[test]
    fn quaternion_round_trips_and_defaults_to_identity() {
        assert_eq!(Quat::default(), Quat::IDENTITY);
        assert_eq!(Quat::IDENTITY.to_array(), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(Quat::FLIP_Y.to_array(), [0.0, 1.0, 0.0, 0.0]);
        let a = [0.1, -0.2, 0.3, 0.9];
        assert_eq!(Quat::from_array(a).to_array(), a);
        assert!(Quat::IDENTITY.is_finite());
        assert!(!Quat::new(f64::NAN, 0.0, 0.0, 1.0).is_finite());
    }

    #[test]
    fn ship_flags_pack_and_unpack() {
        let f = ShipFlags::NONE
            .with(ShipFlags::ALIVE)
            .with_if(true, ShipFlags::BOOSTING)
            .with_if(false, ShipFlags::BRAKING);
        assert!(f.contains(ShipFlags::ALIVE));
        assert!(f.contains(ShipFlags::BOOSTING));
        assert!(!f.contains(ShipFlags::BRAKING));
        assert!(f.contains(ShipFlags::ALIVE.with(ShipFlags::BOOSTING)));
        assert_eq!(ShipFlags::from_bits(f.bits()), f);
        // Every flag is a distinct bit.
        let all = [
            ShipFlags::ALIVE,
            ShipFlags::BOOSTING,
            ShipFlags::BRAKING,
            ShipFlags::INVULN,
            ShipFlags::BOT,
            ShipFlags::LOCAL,
            ShipFlags::BOSS_HITBOX,
        ];
        let union = all.iter().fold(ShipFlags::NONE, |acc, f| acc.with(*f));
        assert_eq!(union.bits().count_ones(), all.len() as u32);
    }

    #[test]
    fn frame_clear_keeps_capacity() {
        let mut frame = Frame::new();
        frame.tick = 12;
        frame.ships.push(ShipView::default());
        frame.bullets.push(ProjView::default());
        frame.asteroids.push(RockView::default());
        frame.boss = Some(BossView::default());
        frame.events.push(SimEvent::BossPhaseStarted);
        assert_eq!(frame.entity_count(), 3);

        let ship_cap = frame.ships.capacity();
        frame.clear();

        assert_eq!(frame, Frame::new());
        assert_eq!(frame.entity_count(), 0);
        assert!(frame.boss.is_none());
        assert!(
            frame.ships.capacity() >= ship_cap,
            "clear must not free the buffers it exists to reuse"
        );
    }

    #[test]
    fn frame_records_are_flat_and_copyable() {
        // The boundary contract: every per-entity record is a fixed-stride POD,
        // so a slice of them is one contiguous byte range for the WASM layer.
        fn assert_pod<T: Copy + Default>() {}
        assert_pod::<ShipView>();
        assert_pod::<ProjView>();
        assert_pod::<FlareView>();
        assert_pod::<RockView>();
        assert_pod::<BossView>();
        assert_pod::<HudState>();
        assert_pod::<Input>();

        // And small: at 60 Hz with ~100 entities these strides are the entire
        // per-frame cost. If one of these grows, that is a decision, not a
        // drive-by.
        use core::mem::size_of;
        assert_eq!(size_of::<ShipView>(), 60);
        assert_eq!(size_of::<ProjView>(), 32);
        assert_eq!(size_of::<FlareView>(), 32);
        assert_eq!(size_of::<RockView>(), 40);
    }

    #[test]
    fn events_and_intents_stay_copyable() {
        // Frame::events and Frame::net_out are flat arrays of these, so a heap
        // allocation inside a variant would defeat the whole layout.
        fn assert_copy<T: Copy>() {}
        assert_copy::<SimEvent>();
        assert_copy::<NetIntent>();
        assert_copy::<NetEvent>();
    }

    #[test]
    fn tick_signature_is_pinned() {
        // Compiles only while `TickFn` has exactly the agreed shape. If someone
        // reorders the parameters or changes `dt`'s width, this fails here
        // rather than at every call site later.
        fn sample(_w: &mut World, _i: &[Input], _e: &[NetEvent], _dt: f64) -> Frame {
            Frame::new()
        }
        let f: TickFn = sample;
        let mut w = world();
        let frame = f(&mut w, &[], &[], TICK_DT);
        assert_eq!(frame, Frame::new());
        assert!((TICK_DT - 1.0 / 60.0).abs() < f64::EPSILON);
    }
}
