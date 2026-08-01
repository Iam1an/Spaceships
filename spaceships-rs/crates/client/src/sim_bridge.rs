//! The seam between the simulation and Bevy.
//!
//! Everything that knows about `spaceships_sim` types lives here or in
//! [`crate::input`]; the rendering modules read only [`SimFrame`], which is the
//! flat `f32` view the simulation was already designed to hand a renderer.
//!
//! # Where the tick comes from
//!
//! It comes from `sim::tick::tick`, which is the implementation of
//! [`sim::world::TickFn`] and the composition root for every behaviour module
//! in that crate. This file used to carry a hand-rolled partial step, because
//! at the time `sim::world::tick` was only a type alias. It is not a type alias
//! any more: [`tick`] below is a one-line forward, and the phase order,
//! projectiles, homing, bot AI, the campaign script, respawn placement, and the
//! whole of [`sim::world::HudState`] come with it.
//!
//! What is still this module's own work, and cannot move into `sim`:
//!
//! - **Building the match.** [`new_match`] decides who is in it. `sim` owns
//!   *how* a bot flies and *where* a team spawns; it has no opinion about how
//!   many bots a skirmish has, because that is the lobby's answer and the lobby
//!   is `public/src/lobby/solo.js`.
//! - **Identity.** See [`Roster`]. `sim` deliberately holds no strings.
//! - **Debouncing the edge-triggered inputs** across a variable number of fixed
//!   steps per rendered frame. See [`EdgeLatch`].

use bevy::app::{RunFixedMainLoop, RunFixedMainLoopSystems};
use bevy::prelude::*;

use sim::math::Vec3 as SimVec3;
use sim::rules::Rules;
use sim::world::{
    EntityId, Frame, Input as SimInput, MapKind, Mode, Quat as SimQuat, Score, Ship, ShipKind,
    Team, World as SimWorldState, TICK_DT, TICK_HZ,
};
use spaceships_sim as sim;

/// Match seed. Fixed so every run produces the same asteroid field and the same
/// bot scatter — the whole point of `sim`'s seeded RNG, and what makes a visual
/// regression obvious.
const SEED: u64 = 0xC0FFEE;

/// The id given to the ship the player flies.
pub const LOCAL_ID: EntityId = 1;

/// Callsign used when nothing better is known, so a label never renders empty.
const UNKNOWN_CALLSIGN: &str = "UNKNOWN";

// ---------------------------------------------------------------------------
// What match to build
// ---------------------------------------------------------------------------

/// Which solo match this process starts in.
///
/// The JS reads this from the lobby (`lobby/solo.js` calls `enterSoloGame`
/// with a mode string). There is no lobby in the Bevy client yet, so it comes
/// from the environment, in the same style as `SPACESHIPS_SCREENSHOT` and
/// `SPACESHIPS_RES` in `main.rs`. When a lobby lands it should write this
/// resource and rebuild [`SimWorld`] from it; nothing else needs to change.
#[derive(Resource, Debug, Clone, PartialEq)]
pub struct MatchSetup {
    /// Which mode. Only [`Mode::Training`], [`Mode::Skirmish`] and
    /// [`Mode::Tutorial`] populate a roster here — trials and the campaign
    /// additionally need `World::trials` / `World::campaign` set up, which is
    /// its own piece of work.
    pub mode: Mode,
    /// Which map.
    pub map: MapKind,
    /// Match seed.
    pub seed: u64,
    /// The "Secret Hard Mode" setting: a three-times-faster bot gun and three
    /// missiles instead of one (`sim::bot::init`).
    pub hard_mode: bool,
    /// The local player's callsign, for [`Roster`].
    pub callsign: String,
}

impl Default for MatchSetup {
    fn default() -> Self {
        MatchSetup {
            // Skirmish rather than Training: it is the mode that exercises
            // teams, friendly fire, the scoreboard and the match clock, and a
            // 5v5 is the honest answer to "is this playable".
            mode: Mode::Skirmish,
            map: MapKind::Space,
            seed: SEED,
            hard_mode: false,
            callsign: "PILOT".to_owned(),
        }
    }
}

impl MatchSetup {
    /// Reads `SPACESHIPS_MODE`, `SPACESHIPS_MAP`, `SPACESHIPS_SEED`,
    /// `SPACESHIPS_HARD` and `SPACESHIPS_CALLSIGN`, falling back to
    /// [`MatchSetup::default`] for anything absent or unparseable.
    ///
    /// Unparseable rather than invalid on purpose: an unrecognised mode name
    /// should start the game, not refuse to. `std::env::var` on
    /// `wasm32-unknown-unknown` always returns `Err`, so the web build gets the
    /// defaults with no `cfg`.
    #[must_use]
    pub fn from_env() -> MatchSetup {
        let mut setup = MatchSetup::default();
        if let Ok(mode) = std::env::var("SPACESHIPS_MODE") {
            match mode.to_ascii_lowercase().as_str() {
                "train" | "training" => setup.mode = Mode::Training,
                "skirmish" => setup.mode = Mode::Skirmish,
                "tutorial" => setup.mode = Mode::Tutorial,
                _ => warn!("SPACESHIPS_MODE={mode} is not a mode this client builds yet"),
            }
        }
        if let Ok(map) = std::env::var("SPACESHIPS_MAP") {
            match map.to_ascii_lowercase().as_str() {
                "space" => setup.map = MapKind::Space,
                "terrain" | "sierras" => setup.map = MapKind::Terrain,
                _ => warn!("SPACESHIPS_MAP={map} is not a map"),
            }
        }
        if let Some(seed) = std::env::var("SPACESHIPS_SEED")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
        {
            setup.seed = seed;
        }
        setup.hard_mode = std::env::var_os("SPACESHIPS_HARD").is_some();
        if let Ok(callsign) = std::env::var("SPACESHIPS_CALLSIGN") {
            if !callsign.trim().is_empty() {
                setup.callsign = callsign;
            }
        }
        setup
    }
}

// ---------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------

/// The authoritative simulation state. Never read by the rendering systems.
#[derive(Resource)]
pub struct SimWorld(pub SimWorldState);

/// The most recent [`Frame`]. This is the *only* simulation state the renderer
/// reads; the one thing beside it is [`Roster`], which `sim` cannot carry.
///
/// # This is the last *tick*, not the last frame
///
/// It changes at [`TICK_HZ`], which is not the display's rate. Anything that
/// reads a position or an orientation straight out of here and puts it on
/// screen will hold still for two or three frames and then jump — that is
/// exactly the judder [`crate::scene`]'s interpolation exists to remove, and it
/// re-appears in whatever reads around it.
///
/// The rule of thumb: **discrete state** (hit points, ammo, flags, events,
/// scores) is correct to read here, because it has no meaningful in-between
/// value. **Continuous state** — anything a camera, a trail, a nameplate, or a
/// lock-on marker positions itself from — wants the interpolated pose that
/// `scene` writes onto the entity's `Transform`, not `ShipView::pos`.
///
/// # Allocation
///
/// `sim::tick::tick` returns a fresh `Frame` by value and builds its lists with
/// `Frame::new`, so one tick's buffers are dropped when the next one lands.
/// `Frame::clear` exists for a caller that wants to refill in place and is
/// unreachable from here — reusing the allocation would mean an
/// `&mut Frame` out-parameter on `TickFn`, which is a `sim` change.
#[derive(Resource, Default)]
pub struct SimFrame(pub Frame);

/// This frame's player intent, filled by [`crate::input`] and consumed by the
/// fixed tick.
#[derive(Resource, Default)]
pub struct PlayerInput(pub SimInput);

/// System set for the fixed-timestep simulation step, so rendering can order
/// itself after it.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct SimSet;

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// One pilot's name and score.
///
/// `#[allow(dead_code)]`: every field is written by [`Roster::sync`] and read by
/// `hud.rs`'s target labels, killfeed and scoreboard — none of which exist yet,
/// and `hud.rs` is not this module's file to edit. Without the allow the binary
/// target warns on the fields the tests are the only readers of.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pilot {
    /// Which ship. Matches [`sim::world::ShipView::id`].
    pub id: EntityId,
    /// Display name.
    pub callsign: String,
    /// Team index, or `-1` if unassigned. Matches
    /// [`sim::world::ShipView::team`].
    pub team: i32,
    /// Kills, mirrored from [`sim::world::MatchState::scores`].
    pub kills: u32,
    /// Deaths, likewise.
    pub deaths: u32,
    /// Whether this is an AI ship.
    pub is_bot: bool,
    /// Whether this is the player on this machine.
    pub is_local: bool,
}

/// The identity [`Frame`] does not carry.
///
/// # Why this is here and not in `sim`
///
/// `sim::world::Score` documents, in as many words, that it has **no name
/// field** and that pilot names are strings that never need to enter the
/// simulation — the JS lobby already owns the id→name map and renders from it.
/// That is the right call for a crate that must be bit-deterministic across
/// glibc, musl, Apple and WASM: a `String` in `World` is an allocation, a
/// collation order and a serialization decision in the middle of the hot state.
///
/// So the map lives on this side of the boundary instead. It is not a
/// compromise: **this module is the thing that assigns the ids**, so it is
/// already the authority on which id is "Ally 3". Kills and deaths are copied
/// out of [`sim::world::MatchState::scores`] each tick and joined onto the same
/// row, which gives `hud.rs` one lookup for a label, a killfeed line, and a
/// scoreboard row.
///
/// In multiplayer the same rows come from the server's `players` message —
/// which is where `net.rs` already gets names — and are fed in through
/// [`Roster::name`]; `sim` sees only the `NetEvent::PlayerRow` half.
#[derive(Resource, Debug, Clone, Default, PartialEq, Eq)]
pub struct Roster {
    rows: Vec<Pilot>,
}

#[allow(dead_code)]
impl Roster {
    /// Every pilot, in [`sim::world::World::ships`] order — the same order
    /// [`Frame::ships`] is in, so a zip is legal.
    #[must_use]
    pub fn pilots(&self) -> &[Pilot] {
        &self.rows
    }

    /// One pilot by entity id.
    #[must_use]
    pub fn get(&self, id: EntityId) -> Option<&Pilot> {
        self.rows.iter().find(|p| p.id == id)
    }

    /// One pilot's callsign, or [`UNKNOWN_CALLSIGN`] — so a label is never
    /// blank and never renders a bare id the way `hud.rs` warned it would.
    #[must_use]
    pub fn callsign(&self, id: EntityId) -> &str {
        self.get(id)
            .map_or(UNKNOWN_CALLSIGN, |p| p.callsign.as_str())
    }

    /// Names an entity, before or after it appears in the world.
    ///
    /// The path for a name that arrives from outside the simulation: the
    /// server's `players` message in multiplayer, or the lobby's callsign for
    /// the local player.
    pub fn name(&mut self, id: EntityId, callsign: impl Into<String>) {
        let callsign = callsign.into();
        match self.rows.iter_mut().find(|p| p.id == id) {
            Some(row) => row.callsign = callsign,
            None => self.rows.push(Pilot {
                id,
                callsign,
                team: -1,
                kills: 0,
                deaths: 0,
                is_bot: false,
                is_local: false,
            }),
        }
    }

    /// Refreshes team, kills, deaths and flags from the world, adds a row for
    /// any ship that appeared without one, and drops rows for ships that left.
    ///
    /// Called once per tick. Allocates only when a ship joins.
    fn sync(&mut self, world: &SimWorldState) {
        self.rows
            .retain(|p| world.ships.iter().any(|s| s.id == p.id));

        for s in &world.ships {
            // Boss hitboxes are ships so that one damage path can serve the
            // capital ship. They are not pilots and must never reach a
            // scoreboard.
            if s.kind == ShipKind::BossHitbox {
                continue;
            }
            if self.get(s.id).is_none() {
                // A ship the match setup did not name: a mid-match join. Give
                // it something stable until a `players` message renames it.
                self.name(s.id, format!("PILOT {}", s.id));
            }
            let score = world.match_state.scores.iter().find(|r| r.id == s.id);
            let Some(row) = self.rows.iter_mut().find(|p| p.id == s.id) else {
                continue;
            };
            row.team = s.team.map_or(-1, |t| t.index() as i32);
            row.is_bot = s.kind == ShipKind::Bot;
            row.is_local = world.local_id == Some(s.id);
            if let Some(score) = score {
                row.kills = score.kills;
                row.deaths = score.deaths;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Input edges
// ---------------------------------------------------------------------------

/// Carries the four edge-triggered inputs across the gap between the render
/// rate and the tick rate.
///
/// [`crate::input`] sets `fire_missile`, `deploy_flare`, `toggle_gun` and
/// `toggle_aim_assist` from `just_pressed`, which is true for exactly one
/// *rendered* frame. `FixedUpdate` runs zero or more times per rendered frame,
/// so passing that straight through is wrong in both directions:
///
/// - At 144 Hz against a 60 Hz tick, most rendered frames run **no** fixed
///   step, so most presses of `E` would do nothing at all.
/// - Below 60 fps, one rendered frame runs **two or three** fixed steps, so one
///   press of `E` would launch two or three missiles.
///
/// Latching in [`RunFixedMainLoopSystems::BeforeFixedMainLoop`] — once per
/// rendered frame, after `PreUpdate`, before any fixed step — and draining on
/// the first fixed step that follows makes it exactly once, whatever the two
/// rates are. Held inputs (`fire`, `boost`, `braking`) are level-triggered and
/// pass through untouched, which is correct: a frame whose input never arrived
/// should coast, not stop firing.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
struct EdgeLatch {
    fire_missile: bool,
    deploy_flare: bool,
    toggle_gun: bool,
    toggle_aim_assist: bool,
}

impl EdgeLatch {
    /// Remembers any edge in `input`. Idempotent within a rendered frame.
    fn latch(&mut self, input: &SimInput) {
        self.fire_missile |= input.fire_missile;
        self.deploy_flare |= input.deploy_flare;
        self.toggle_gun |= input.toggle_gun;
        self.toggle_aim_assist |= input.toggle_aim_assist;
    }

    /// Moves the latched edges onto `input`, clearing them, so the next fixed
    /// step in the same rendered frame sees none of them.
    fn drain_into(&mut self, input: &mut SimInput) {
        input.fire_missile = std::mem::take(&mut self.fire_missile);
        input.deploy_flare = std::mem::take(&mut self.deploy_flare);
        input.toggle_gun = std::mem::take(&mut self.toggle_gun);
        input.toggle_aim_assist = std::mem::take(&mut self.toggle_aim_assist);
    }
}

// ---------------------------------------------------------------------------
// The plugin
// ---------------------------------------------------------------------------

/// Wires the simulation into the app: one world, one fixed tick at
/// [`TICK_HZ`], one frame buffer, one roster.
pub struct SimPlugin;

impl Plugin for SimPlugin {
    fn build(&self, app: &mut App) {
        let setup = MatchSetup::from_env();
        let (world, roster) = new_match(&setup);
        info!(
            "match: {:?} on {:?}, seed {:#x}, {} ships",
            setup.mode,
            setup.map,
            setup.seed,
            world.ships.len()
        );

        app.insert_resource(SimWorld(world))
            .insert_resource(roster)
            .insert_resource(setup)
            .init_resource::<SimFrame>()
            .init_resource::<PlayerInput>()
            .init_resource::<EdgeLatch>()
            // The simulation is fixed-step by contract: variable frame time
            // must never reach it (`sim::world::TickFn`). Bevy's `FixedUpdate`
            // accumulator is exactly the "accumulate real time and run a whole
            // number of ticks" the contract asks for, so the two agree without
            // any bookkeeping of our own.
            //
            // The *remainder* that accumulator carries — `Time<Fixed>`'s
            // `overstep_fraction()` — is what `scene.rs` blends this tick and
            // the last one by. That is the only thing outside this module that
            // depends on the rate, and it depends on it in the one direction
            // that is safe: reading how far through a tick the display is,
            // never feeding anything back.
            .insert_resource(Time::<Fixed>::from_hz(TICK_HZ))
            .add_systems(
                RunFixedMainLoop,
                latch_edges.in_set(RunFixedMainLoopSystems::BeforeFixedMainLoop),
            )
            .add_systems(FixedUpdate, fixed_tick.in_set(SimSet));
    }
}

/// Once per rendered frame: remember the edges before any fixed step consumes
/// them. See [`EdgeLatch`].
fn latch_edges(input: Res<PlayerInput>, mut latch: ResMut<EdgeLatch>) {
    latch.latch(&input.0);
}

/// Runs one fixed simulation step and republishes [`SimFrame`] and [`Roster`].
fn fixed_tick(
    mut world: ResMut<SimWorld>,
    input: Res<PlayerInput>,
    mut latch: ResMut<EdgeLatch>,
    mut frame: ResMut<SimFrame>,
    mut roster: ResMut<Roster>,
) {
    let mut player = input.0;
    latch.drain_into(&mut player);

    // No `NetEvent`s: solo is `Authority::Local` and resolves its own damage.
    // The multiplayer path is an inbox of `NetEvent`s from `net.rs` in place of
    // the empty slice, and nothing else about this call changes.
    frame.0 = tick(&mut world.0, &[player], &[], TICK_DT);

    roster.sync(&world.0);
}

/// One simulation step.
///
/// A forward to `sim::tick::tick`, which *is* [`sim::world::TickFn`] — the
/// phase order, and the reasoning behind every placement in it, are that
/// module's docs. Kept as a named function here because it is the single point
/// at which this crate enters the simulation, and because the tests below want
/// one entry point rather than two.
pub fn tick(
    w: &mut SimWorldState,
    inputs: &[SimInput],
    events: &[sim::world::NetEvent],
    dt: f64,
) -> Frame {
    sim::tick::tick(w, inputs, events, dt)
}

// ---------------------------------------------------------------------------
// Building the match
// ---------------------------------------------------------------------------

/// Builds the match `setup` describes, and the roster that names it.
///
/// The lobby's share of `main.js:2925` (`spawnSoloEntities`). `sim` owns how a
/// bot flies and where a team spawns; how many bots there are is a lobby
/// decision, and the counts come from [`sim::rules::SpawnRules`] rather than
/// from literals here.
#[must_use]
pub fn new_match(setup: &MatchSetup) -> (SimWorldState, Roster) {
    let rules = Rules::DEFAULT;

    // A solo `Mode` gives `authority == Local`, which is what lets the
    // simulation resolve its own damage — there is no server.
    debug_assert!(setup.mode.is_solo());
    let mut world = SimWorldState::new(setup.seed, rules, setup.mode, setup.map);

    // The moon and both motherships came with `World::new`; the field does not.
    // It draws from the dedicated `field` RNG stream, so the layout is stable
    // even as other subsystems are added.
    sim::asteroids::populate(&mut world);

    let (spawn, facing) = team_anchor(&rules, setup.map, Team::Zero);
    let mut me = Ship::spawn(LOCAL_ID, ShipKind::Local, spawn, facing, &rules);
    me.team = Some(Team::Zero);
    world.ships.push(me);
    world.local_id = Some(LOCAL_ID);

    let mut roster = Roster::default();
    roster.name(LOCAL_ID, setup.callsign.clone());

    match setup.mode {
        Mode::Training => spawn_training_bot(&mut world, &mut roster, setup),
        Mode::Skirmish => spawn_skirmish(&mut world, &mut roster, setup),
        // Tutorial has no opponents by design (`main.js:2925` falls through).
        // Trials and Campaign additionally need `World::trials` /
        // `World::campaign`, which this module does not build yet.
        _ => {}
    }

    seed_scoreboard(&mut world);
    roster.sync(&world);
    (world, roster)
}

/// A team's spawn anchor and facing, without jitter.
///
/// The same anchors `sim::tick`'s respawn placement uses (`team_spawn`), which
/// is what makes a ship come back where it started. Team 0 spawns at `-z`
/// facing `+z`, which on the space map points the nose at the moon and the
/// field beyond it; team 1 mirrors it.
fn team_anchor(rules: &Rules, map: MapKind, team: Team) -> (SimVec3, SimQuat) {
    let (z, y) = match map {
        MapKind::Space => (rules.spawn.space_z, rules.spawn.space_y),
        MapKind::Terrain => (rules.spawn.terrain_z, rules.spawn.terrain_y),
    };
    match team {
        Team::Zero => (SimVec3::new(0.0, y, -z), SimQuat::IDENTITY),
        Team::One => (SimVec3::new(0.0, y, z), SimQuat::FLIP_Y),
    }
}

/// Training: one bot, dead ahead.
///
/// `main.js:2934` — `ship.position + forward * 250`, where 250 is
/// [`sim::rules::SpawnRules::train_bot_distance`]. It is put on the far team so
/// `World::can_damage` lets the two shoot each other.
fn spawn_training_bot(world: &mut SimWorldState, roster: &mut Roster, setup: &MatchSetup) {
    let rules = world.rules;
    let Some(me) = world.local_ship() else { return };
    let ahead = me.pos + sim::math::forward(me.quat) * rules.spawn.train_bot_distance;
    push_bot(
        world,
        roster,
        LOCAL_ID + 1,
        Team::One,
        ahead,
        // Facing the player, so the first engagement is not a stern chase.
        SimQuat::FLIP_Y,
        "Bot",
        setup.hard_mode,
    );
}

/// Skirmish: allies on the player's team, enemies on the other.
///
/// `main.js:2938`. Counts and scatter are
/// [`sim::rules::SpawnRules::skirmish_ally_count`],
/// `skirmish_enemy_count` and `skirmish_jitter`; the ids run on from
/// [`LOCAL_ID`] so allies are 2..=5 and enemies 6..=10.
fn spawn_skirmish(world: &mut SimWorldState, roster: &mut Roster, setup: &MatchSetup) {
    let rules = world.rules;
    let allies = rules.spawn.skirmish_ally_count;
    let enemies = rules.spawn.skirmish_enemy_count;

    let mut next_id = LOCAL_ID + 1;
    for (team, count, label) in [(Team::Zero, allies, "Ally"), (Team::One, enemies, "Enemy")] {
        let (anchor, facing) = team_anchor(&rules, world.map, team);
        for i in 0..count {
            let pos = anchor + scatter(&mut world.rng.spawn, rules.spawn.skirmish_jitter);
            push_bot(
                world,
                roster,
                next_id,
                team,
                pos,
                facing,
                &format!("{label} {}", i + 1),
                setup.hard_mode,
            );
            next_id += 1;
        }
    }
}

/// Adds one AI ship and its roster row.
#[allow(clippy::too_many_arguments)]
fn push_bot(
    world: &mut SimWorldState,
    roster: &mut Roster,
    id: EntityId,
    team: Team,
    pos: SimVec3,
    quat: SimQuat,
    callsign: &str,
    hard_mode: bool,
) {
    let rules = world.rules;
    let mut ship = Ship::spawn(id, ShipKind::Bot, pos, quat, &rules);
    ship.team = Some(team);
    // `Ship::spawn` leaves `Ship::bot` at its default, which is a bot with no
    // missiles that would fire one immediately if it had any. `bot::init` is
    // what arms it, and without it `update_bots` flies a disarmed statue.
    sim::bot::init(&mut ship, hard_mode, false, &rules, &mut world.rng.bots);
    world.ships.push(ship);
    roster.name(id, callsign);
}

/// A scatter offset from a full-width box, drawn x, y, z in that order.
///
/// `main.js:2906` is `(Math.random() - 0.5) * range` per axis, so the named
/// width is the *full* extent and each axis reaches half of it. Note that
/// `sim::tick`'s own `team_spawn` reads `SpawnRules::space_jitter` as
/// `next_f64_signed() * jitter.x`, i.e. as a half-width, despite that field
/// being documented as a full width too — the two disagree by a factor of two.
/// This follows the documentation and the JS; the other is a `sim` fix.
fn scatter(rng: &mut sim::rng::Rng, full_width: SimVec3) -> SimVec3 {
    let x = rng.next_f64_signed() * full_width.x * 0.5;
    let y = rng.next_f64_signed() * full_width.y * 0.5;
    let z = rng.next_f64_signed() * full_width.z * 0.5;
    SimVec3::new(x, y, z)
}

/// Gives every pilot a scoreboard row.
///
/// Not cosmetic. `sim::tick`'s `credit_kill` books a kill by *finding* the
/// killer's and the victim's rows — `scores.iter_mut().find(...)` — and does
/// nothing at all when they are absent. In multiplayer the rows arrive as
/// `NetEvent::PlayerRow`; solo has no server to send them, so without this the
/// per-pilot kill and death counts stay at zero for a whole match while the
/// team totals tick up, which is exactly the client/server asymmetry
/// `sim::world::Authority` documents as the source of a whole bug class.
fn seed_scoreboard(world: &mut SimWorldState) {
    let SimWorldState {
        ships, match_state, ..
    } = world;
    for s in ships.iter() {
        if s.kind == ShipKind::BossHitbox {
            continue;
        }
        if match_state.scores.iter().any(|r| r.id == s.id) {
            continue;
        }
        match_state.scores.push(Score {
            id: s.id,
            team: s.team,
            kills: 0,
            deaths: 0,
        });
    }
}

// ---------------------------------------------------------------------------
// Frame -> Bevy conversions
// ---------------------------------------------------------------------------

/// `Frame` position to a Bevy translation.
///
/// No axis remapping: `sim` inherited Three.js's right-handed Y-up convention
/// with the ship nose along local `+z`, and Bevy uses the same handedness and
/// the same up axis. The only place the two differ is the glTF model's own
/// resting orientation, which `scene.rs` corrects exactly where `ship.js` does.
#[inline]
pub fn pos(p: [f32; 3]) -> Vec3 {
    Vec3::new(p[0], p[1], p[2])
}

/// `Frame` orientation to a Bevy rotation. Both are `(x, y, z, w)`.
#[inline]
pub fn rot(q: [f32; 4]) -> Quat {
    Quat::from_xyzw(q[0], q[1], q[2], q[3])
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim::world::{ShipFlags, SimEvent};

    fn setup(mode: Mode) -> MatchSetup {
        MatchSetup {
            mode,
            ..MatchSetup::default()
        }
    }

    fn skirmish() -> (SimWorldState, Roster) {
        new_match(&setup(Mode::Skirmish))
    }

    /// A world with no opponents, for the tests that are about the player
    /// alone. The tutorial is the mode that has none by design.
    fn solo() -> SimWorldState {
        new_match(&setup(Mode::Tutorial)).0
    }

    /// `secs` of ticks with `input` applied, returning the last frame and every
    /// event along the way.
    fn run(w: &mut SimWorldState, input: SimInput, secs: f64) -> (Frame, Vec<SimEvent>) {
        let mut events = Vec::new();
        let mut frame = Frame::new();
        for _ in 0..(secs * TICK_HZ) as u32 {
            frame = tick(w, &[input], &[], TICK_DT);
            events.extend(frame.events.iter().copied());
        }
        (frame, events)
    }

    fn throttle_up() -> SimInput {
        SimInput {
            id: LOCAL_ID,
            throttle_axis: 1.0,
            ..Default::default()
        }
    }

    fn bots(w: &SimWorldState) -> impl Iterator<Item = &Ship> {
        w.ships.iter().filter(|s| s.kind == ShipKind::Bot)
    }

    // -- the match ---------------------------------------------------------

    #[test]
    fn the_world_starts_with_a_field_and_a_moon() {
        let w = solo();
        assert_eq!(w.local_id, Some(LOCAL_ID));
        assert_eq!(
            w.asteroids.len(),
            w.rules.world.asteroid_field.count as usize
        );
        // `World::new` owns this, not the field generator.
        assert_eq!(w.obstacles.len(), 1, "the moon");
        assert_eq!(w.ships.len(), 1, "the tutorial has no opponents");
    }

    /// The headline requirement: a skirmish is a 5v5, on the right teams, with
    /// the player counted on his own side.
    #[test]
    fn a_skirmish_spawns_four_allies_and_five_enemies() {
        let (w, roster) = skirmish();
        let rules = w.rules;

        assert_eq!(
            bots(&w).count() as u32,
            rules.spawn.skirmish_ally_count + rules.spawn.skirmish_enemy_count
        );
        assert_eq!(w.ships.len(), 10, "the player plus nine bots");

        let allied = bots(&w).filter(|s| s.team == Some(Team::Zero)).count();
        let enemy = bots(&w).filter(|s| s.team == Some(Team::One)).count();
        assert_eq!(allied as u32, rules.spawn.skirmish_ally_count);
        assert_eq!(enemy as u32, rules.spawn.skirmish_enemy_count);

        // The player is on the allies' side, which is what makes it 5v5.
        assert_eq!(w.local_ship().unwrap().team, Some(Team::Zero));

        // Ids are dense and start after the player, so nothing collides with
        // `LOCAL_ID`.
        let mut ids: Vec<_> = w.ships.iter().map(|s| s.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, (LOCAL_ID..=LOCAL_ID + 9).collect::<Vec<_>>());

        assert_eq!(roster.callsign(LOCAL_ID), "PILOT");
        assert_eq!(roster.callsign(2), "Ally 1");
        assert_eq!(roster.callsign(5), "Ally 4");
        assert_eq!(roster.callsign(6), "Enemy 1");
        assert_eq!(roster.callsign(10), "Enemy 5");
    }

    /// Every bot must be armed. A `Ship::spawn` that skipped `bot::init` still
    /// flies, so this cannot be inferred from the ship count.
    #[test]
    fn every_bot_is_armed_and_placed_on_its_own_side() {
        let (w, _) = skirmish();
        let rules = w.rules;
        for b in bots(&w) {
            assert!(b.alive);
            assert_eq!(
                b.bot.missiles_left,
                rules.bot.missile_max_for(false),
                "bot {} has no missiles; bot::init was not called",
                b.id
            );
            assert!(
                b.bot.missile_timer > 0.0,
                "bot {} would fire instantly",
                b.id
            );
            let anchor = team_anchor(&rules, w.map, b.team.unwrap()).0;
            assert!(
                b.pos.distance(anchor) < rules.spawn.skirmish_jitter.length(),
                "bot {} is not near its team anchor",
                b.id
            );
        }
    }

    #[test]
    fn training_spawns_one_bot_ahead_of_the_player() {
        let (w, roster) = new_match(&setup(Mode::Training));
        assert_eq!(bots(&w).count(), 1);
        let me = w.local_ship().unwrap();
        let bot = bots(&w).next().unwrap();
        assert_eq!(bot.team, Some(Team::One));
        assert!(
            (bot.pos.distance(me.pos) - w.rules.spawn.train_bot_distance).abs() < 1e-6,
            "the training bot is placed at `train_bot_distance`"
        );
        assert_eq!(roster.callsign(bot.id), "Bot");
    }

    /// Without this, `credit_kill` has no row to increment and a whole solo
    /// match scores nothing per pilot.
    #[test]
    fn every_pilot_has_a_scoreboard_row() {
        let (w, _) = skirmish();
        assert_eq!(w.match_state.scores.len(), w.ships.len());
        for s in &w.ships {
            let row = w
                .match_state
                .scores
                .iter()
                .find(|r| r.id == s.id)
                .expect("no scoreboard row");
            assert_eq!(row.team, s.team);
        }
        assert!(w.match_state.active, "a skirmish runs the match clock");
    }

    // -- the bots fight ----------------------------------------------------

    /// The one that says the game is playable: left alone for ten seconds, the
    /// bots close, shoot, and hurt each other. Ten seconds is chosen because
    /// the two teams start 1080 units apart at a 60 u/s cruise.
    #[test]
    fn bots_engage_rather_than_sitting_still() {
        let (mut w, _) = skirmish();
        let start: Vec<_> = bots(&w).map(|s| (s.id, s.pos)).collect();

        let (_, events) = run(&mut w, SimInput::default(), 10.0);

        for (id, from) in start {
            let now = w.ship(id).unwrap().pos;
            assert!(
                now.distance(from) > 100.0,
                "bot {id} barely moved: {from:?} -> {now:?}"
            );
        }

        let bot_ids: Vec<_> = bots(&w).map(|s| s.id).collect();
        let shots = events
            .iter()
            .filter(|e| matches!(e, SimEvent::Fired { owner, .. } if bot_ids.contains(owner)))
            .count();
        assert!(shots > 0, "no bot fired a shot in ten seconds");

        let hits = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    SimEvent::Damaged {
                        source: Some(_),
                        ..
                    }
                )
            })
            .count();
        assert!(hits > 0, "{shots} shots and not one of them connected");
    }

    /// The teams must actually converge, not just drift. Separately worth
    /// pinning: a bot that flew *away* would also pass the movement check
    /// above.
    #[test]
    fn the_two_teams_close_on_each_other() {
        let (mut w, _) = skirmish();
        let gap = |w: &SimWorldState| {
            let mid = |team| {
                let mut n = 0.0;
                let mut sum = SimVec3::ZERO;
                for s in w.ships.iter().filter(|s| s.team == Some(team)) {
                    sum += s.pos;
                    n += 1.0;
                }
                sum * (1.0 / n)
            };
            mid(Team::Zero).distance(mid(Team::One))
        };

        let before = gap(&w);
        run(&mut w, SimInput::default(), 8.0);
        assert!(
            gap(&w) < before,
            "the teams did not close: {before} -> {}",
            gap(&w)
        );
    }

    /// Kills have to be booked against the pilot, not only the team, and the
    /// roster is what carries them out of the simulation.
    #[test]
    fn a_kill_reaches_the_roster() {
        let (mut w, mut roster) = skirmish();

        // Put an enemy one hit from death rather than waiting for a real
        // fifteen-second dogfight: the path under test is the scoring, not the
        // ballistics.
        let victim = bots(&w).find(|s| s.team == Some(Team::One)).unwrap().id;
        let killer = bots(&w).find(|s| s.team == Some(Team::Zero)).unwrap().id;
        {
            let s = w.ship_mut(victim).unwrap();
            s.hp = 0;
            s.alive = true;
            s.invuln_timer = 0.0;
        }
        // `sim::tick` credits from `NetEvent::Death` through the same
        // `credit_kill` a bullet uses.
        tick(
            &mut w,
            &[SimInput::default()],
            &[sim::world::NetEvent::Death {
                id: victim,
                killer: Some(killer),
            }],
            TICK_DT,
        );
        roster.sync(&w);

        assert_eq!(roster.get(killer).unwrap().kills, 1);
        assert_eq!(roster.get(victim).unwrap().deaths, 1);
        assert_eq!(w.match_state.team_kills[0], 1);
    }

    // -- what the frame now carries ---------------------------------------

    /// `scene.rs` snaps a respawning ship on `SimEvent::ShipRespawned` and
    /// streaks it across the map without one. The event only exists if this
    /// bridge runs the phase that emits it.
    #[test]
    fn a_respawn_is_announced() {
        let (mut w, _) = skirmish();
        let id = bots(&w).next().unwrap().id;
        {
            let s = w.ship_mut(id).unwrap();
            s.alive = false;
            s.respawn_timer = TICK_DT / 2.0;
        }
        let frame = tick(&mut w, &[SimInput::default()], &[], TICK_DT);
        assert!(
            frame
                .events
                .iter()
                .any(|e| matches!(e, SimEvent::ShipRespawned { id: r, .. } if *r == id)),
            "no ShipRespawned; scene.rs will interpolate the ship across the map"
        );
        assert!(w.ship(id).unwrap().alive);
    }

    /// Holding the brake past the overcharge delay must reach `hud.overcharge01`,
    /// which is what lights `#chargebar.overload` and the master caution.
    #[test]
    fn the_brake_overcharge_reaches_the_hud() {
        let mut w = solo();
        let brake = SimInput {
            id: LOCAL_ID,
            braking: true,
            ..Default::default()
        };
        let held = w.rules.ship.brake_full_time + w.rules.ship.brake_overcharge_damage_delay + 0.5;
        let (frame, _) = run(&mut w, brake, held);

        assert!(frame.hud.charge01 >= 1.0, "the brake never reached charge");
        assert!(
            frame.hud.overcharge01 >= 1.0,
            "overcharge01 stayed at {}",
            frame.hud.overcharge01
        );
    }

    /// The gun, end to end: a held trigger spends ammo and puts bolts in the
    /// frame. The old hand-rolled step ran no weapons at all, so `Frame::bullets`
    /// was always empty.
    #[test]
    fn the_trigger_spends_ammo_and_fills_the_frame() {
        let (mut w, _) = skirmish();
        let full = w.local_ship().unwrap().ammo;
        let fire = SimInput {
            id: LOCAL_ID,
            fire: true,
            ..Default::default()
        };
        let (frame, events) = run(&mut w, fire, 0.5);

        assert!(w.local_ship().unwrap().ammo < full, "no ammo was spent");
        assert!(frame.hud.ammo01 < 1.0);
        assert!(!frame.bullets.is_empty(), "no bolts in the frame");
        assert!(events
            .iter()
            .any(|e| matches!(e, SimEvent::Fired { owner, .. } if *owner == LOCAL_ID)));
    }

    /// The requirement this module exists for: a key press reaches the flight
    /// model and moves the ship, and the renderer sees the result.
    #[test]
    fn throttle_input_moves_the_ship_forward() {
        let mut w = solo();
        let start = w.local_ship().expect("local ship").pos;

        let (frame, _) = run(&mut w, throttle_up(), 2.0);

        let end = w.local_ship().unwrap().pos;
        assert!(
            end.distance(start) > 1.0,
            "two seconds of throttle should move the ship, went {start:?} -> {end:?}"
        );
        // Team 0 spawns at -z facing +z, so "forward" is +z.
        assert!(end.z > start.z, "the nose is local +z");

        let me = frame
            .ships
            .iter()
            .find(|s| s.flags.contains(ShipFlags::LOCAL))
            .expect("the local ship is flagged in the frame");
        assert_eq!(me.pos[2], end.z as f32);
        assert!(frame.hud.speed > 0.0);
    }

    #[test]
    fn no_input_leaves_the_ship_at_rest() {
        let mut w = solo();
        let start = w.local_ship().unwrap().pos;
        run(&mut w, SimInput::default(), 1.0);
        assert_eq!(w.local_ship().unwrap().pos, start);
    }

    /// The frame is rebuilt from scratch each tick and must not accumulate.
    #[test]
    fn the_frame_does_not_accumulate() {
        let mut w = solo();
        let mut frame = tick(&mut w, &[throttle_up()], &[], TICK_DT);
        let ships = frame.ships.len();
        let rocks = frame.asteroids.len();
        for _ in 0..120 {
            frame = tick(&mut w, &[throttle_up()], &[], TICK_DT);
        }
        assert_eq!(frame.ships.len(), ships);
        assert_eq!(frame.asteroids.len(), rocks);
        assert_eq!(frame.tick, 121);
    }

    /// The seed is fixed so the whole match is reproducible — the field, the
    /// bot scatter, and every decision the AI makes from them.
    #[test]
    fn the_match_is_deterministic() {
        let (mut a, _) = skirmish();
        let (mut b, _) = skirmish();
        assert_eq!(a.asteroids, b.asteroids);
        assert_eq!(
            a.ships.iter().map(|s| s.pos).collect::<Vec<_>>(),
            b.ships.iter().map(|s| s.pos).collect::<Vec<_>>()
        );

        let (fa, _) = run(&mut a, throttle_up(), 3.0);
        let (fb, _) = run(&mut b, throttle_up(), 3.0);
        assert_eq!(fa, fb, "two runs of the same seed diverged");
    }

    // -- identity ----------------------------------------------------------

    #[test]
    fn the_roster_tracks_ships_that_join_and_leave() {
        let (mut w, mut roster) = skirmish();
        assert_eq!(roster.pilots().len(), w.ships.len());
        assert!(roster.get(LOCAL_ID).unwrap().is_local);
        assert!(roster.get(2).unwrap().is_bot);
        assert!(!roster.get(LOCAL_ID).unwrap().is_bot);
        assert_eq!(roster.get(6).unwrap().team, 1);

        // A ship nobody named still gets a stable label rather than a bare id.
        let rules = w.rules;
        w.ships.push(Ship::spawn(
            99,
            ShipKind::Remote,
            SimVec3::ZERO,
            SimQuat::IDENTITY,
            &rules,
        ));
        roster.sync(&w);
        assert_eq!(roster.callsign(99), "PILOT 99");

        // And the server's name wins when it arrives.
        roster.name(99, "Maverick");
        roster.sync(&w);
        assert_eq!(roster.callsign(99), "Maverick");

        w.ships.retain(|s| s.id != 99);
        roster.sync(&w);
        assert_eq!(roster.callsign(99), UNKNOWN_CALLSIGN);
    }

    #[test]
    fn boss_hitboxes_are_never_pilots() {
        let (mut w, mut roster) = skirmish();
        let rules = w.rules;
        w.ships.push(Ship::spawn(
            9001,
            ShipKind::BossHitbox,
            SimVec3::ZERO,
            SimQuat::IDENTITY,
            &rules,
        ));
        roster.sync(&w);
        assert!(roster.get(9001).is_none());
    }

    // -- input edges -------------------------------------------------------

    /// At 144 Hz most rendered frames run no fixed step, so an edge that is not
    /// latched is simply lost.
    #[test]
    fn an_edge_survives_a_frame_with_no_fixed_step() {
        let mut latch = EdgeLatch::default();
        let pressed = SimInput {
            fire_missile: true,
            ..Default::default()
        };

        latch.latch(&pressed); // rendered frame 1: no fixed step follows
        latch.latch(&SimInput::default()); // rendered frame 2: key already up

        let mut consumed = SimInput::default();
        latch.drain_into(&mut consumed);
        assert!(consumed.fire_missile, "the press was dropped");
    }

    /// Below 60 fps one rendered frame runs several fixed steps, and an edge
    /// passed through raw would fire on every one of them.
    #[test]
    fn an_edge_is_consumed_exactly_once() {
        let mut latch = EdgeLatch::default();
        latch.latch(&SimInput {
            fire_missile: true,
            deploy_flare: true,
            toggle_gun: true,
            toggle_aim_assist: true,
            ..Default::default()
        });

        let mut first = SimInput::default();
        latch.drain_into(&mut first);
        assert!(first.fire_missile && first.deploy_flare);
        assert!(first.toggle_gun && first.toggle_aim_assist);

        let mut second = SimInput::default();
        latch.drain_into(&mut second);
        assert_eq!(second, SimInput::default(), "the edge fired twice");
    }

    /// Held inputs are level-triggered and must not go through the latch.
    #[test]
    fn held_inputs_pass_through_untouched() {
        let mut latch = EdgeLatch::default();
        let held = SimInput {
            id: LOCAL_ID,
            fire: true,
            boost: true,
            braking: true,
            throttle_axis: 1.0,
            ..Default::default()
        };
        latch.latch(&held);
        let mut out = held;
        latch.drain_into(&mut out);
        assert!(out.fire && out.boost && out.braking);
        assert_eq!(out.throttle_axis, 1.0);
        assert_eq!(out.id, LOCAL_ID);
    }

    // -- setup -------------------------------------------------------------

    #[test]
    fn the_default_match_is_a_skirmish() {
        let setup = MatchSetup::default();
        assert_eq!(setup.mode, Mode::Skirmish);
        assert!(setup.mode.is_solo());
        assert!(setup.mode.has_match_clock());
    }
}
