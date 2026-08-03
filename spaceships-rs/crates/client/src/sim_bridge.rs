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
//!   is `public/src/lobby/solo.js`. The same goes for the campaign and the
//!   trials: `sim::campaign` owns the mission script, but somebody has to call
//!   [`sim::campaign::init`] and fill [`sim::world::World::trials`], and that
//!   somebody is the lobby.
//! - **Identity.** See [`Roster`]. `sim` deliberately holds no strings.
//! - **Debouncing the edge-triggered inputs** across a variable number of fixed
//!   steps per rendered frame. See [`EdgeLatch`].
//! - **Trials checkpoint scoring.** Temporarily. See [`score_trials`] for the
//!   whole argument and for the `sim` change that should reclaim it.

use bevy::app::{RunFixedMainLoop, RunFixedMainLoopSystems};
use bevy::prelude::*;

use sim::math::Vec3 as SimVec3;
use sim::rules::Rules;
use sim::world::{
    EntityId, Frame, Input as SimInput, MapKind, Mode, Quat as SimQuat, Score, Ship, ShipKind,
    SimEvent, Team, TrialsHud, TrialsState, World as SimWorldState, TICK_DT, TICK_HZ,
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

/// What the killfeed calls the campaign's capital ship. `main.js:2778`.
const BOSS_CALLSIGN: &str = "Capital Ship";

// ---------------------------------------------------------------------------
// What match to build
// ---------------------------------------------------------------------------

/// Campaign missions the lobby offers. `lobby/solo.js`'s `MISSIONS`.
pub const CAMPAIGN_MISSIONS: u8 = 3;

/// Time-trial circuits the lobby offers. `lobby/solo.js`'s `TRIALS`.
pub const TRIAL_COUNT: u8 = 4;

/// Which solo match this process starts in.
///
/// The JS reads this from the lobby (`lobby/solo.js` calls `enterSoloGame`
/// with a mode string). There is no lobby in the Bevy client yet, so it comes
/// from the environment, in the same style as `SPACESHIPS_SCREENSHOT` and
/// `SPACESHIPS_RES` in `main.rs`.
///
/// # The surface a menu drives
///
/// Two ways in, and they do the same thing:
///
/// - **The resource.** This is inserted by [`SimPlugin`] and is the current
///   match's setup. Writing it alone changes nothing — the world is already
///   built — so it is the thing to *read* for "what am I playing".
/// - **The [`StartMatch`] message.** Write one and the next `PreUpdate`
///   rebuilds [`SimWorld`], [`Roster`] and [`SimFrame`] from it and republishes
///   this resource. That is the whole entry point; a menu needs nothing else.
///
/// There is deliberately **no separate mission or trial field**. `sim` already
/// carries the number inside [`Mode::Campaign`] and [`Mode::Trials`], and
/// [`sim::world::World::new`] reads it from there — a second copy here could
/// disagree with the world, which is exactly the bug class `rules.rs` exists to
/// stop. [`MatchSetup::campaign`] and [`MatchSetup::trial`] are the
/// constructors; [`MatchSetup::mission_number`] reads it back out.
#[derive(Resource, Debug, Clone, PartialEq)]
pub struct MatchSetup {
    /// Which mode, including the mission number for [`Mode::Campaign`] and the
    /// circuit number for [`Mode::Trials`].
    pub mode: Mode,
    /// Which map. Forced to [`MapKind::Space`] for the campaign by
    /// [`MatchSetup::normalize`], because `lobby/solo.js` passes
    /// `{ map: 'space' }` for every mission and the mission geometry is written
    /// in space coordinates.
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

/// `#[allow(dead_code)]`: the three constructors and the accessor below are the
/// surface a lobby drives, and there is no lobby yet — [`MatchSetup::from_env`]
/// is the only caller in this build. Same reasoning as [`Pilot`]'s.
#[allow(dead_code)]
impl MatchSetup {
    /// Campaign mission `n`, on the space map. Out-of-range numbers are clamped
    /// rather than rejected — see [`MatchSetup::normalize`].
    #[must_use]
    pub fn campaign(mission: u8) -> MatchSetup {
        let mut setup = MatchSetup {
            mode: Mode::Campaign(mission),
            ..MatchSetup::default()
        };
        setup.normalize();
        setup
    }

    /// Time trial `n`.
    #[must_use]
    pub fn trial(trial: u8) -> MatchSetup {
        let mut setup = MatchSetup {
            mode: Mode::Trials(trial),
            ..MatchSetup::default()
        };
        setup.normalize();
        setup
    }

    /// The mission or circuit number, for a menu that wants to show which one
    /// is selected without matching on [`Mode`].
    #[must_use]
    pub fn mission_number(&self) -> Option<u8> {
        match self.mode {
            Mode::Campaign(m) | Mode::Trials(m) => Some(m),
            _ => None,
        }
    }

    /// Clamps the mission and circuit numbers into range and forces the
    /// campaign's map.
    ///
    /// Clamping rather than refusing, for the same reason [`Self::from_env`]
    /// warns rather than exits: a menu with an off-by-one should start
    /// *something*. `sim` clamps the same way — [`sim::rules::campaign_waves`]
    /// and [`sim::rules::trial_checkpoints`] both fall back to 1 — so this only
    /// makes the world agree with the setup that built it.
    pub fn normalize(&mut self) {
        match self.mode {
            Mode::Campaign(m) => {
                self.mode = Mode::Campaign(m.clamp(1, CAMPAIGN_MISSIONS));
                // `lobby/solo.js:59`. The wave anchors, the checkpoints and the
                // capital ship's berth are all space coordinates; on the
                // terrain map the mission would run under the ground.
                self.map = MapKind::Space;
            }
            Mode::Trials(n) => self.mode = Mode::Trials(n.clamp(1, TRIAL_COUNT)),
            _ => {}
        }
    }

    /// Reads `SPACESHIPS_MODE`, `SPACESHIPS_MISSION`, `SPACESHIPS_TRIAL`,
    /// `SPACESHIPS_MAP`, `SPACESHIPS_SEED`, `SPACESHIPS_HARD` and
    /// `SPACESHIPS_CALLSIGN`, falling back to [`MatchSetup::default`] for
    /// anything absent or unparseable.
    ///
    /// `SPACESHIPS_MODE` takes the JS lobby's own mode strings, so a value that
    /// works in `enterSoloGame` works here: `train`, `skirmish`, `tutorial`,
    /// `campaign`, `trials`, `trials2`, `trials3`, `trials4`. The trailing
    /// digit is also accepted on `campaign` (`campaign3`), which the JS spells
    /// as a separate `missionId` option.
    ///
    /// `SPACESHIPS_MISSION` and `SPACESHIPS_TRIAL` set the number when the mode
    /// is already the matching one, and select the mode outright when
    /// `SPACESHIPS_MODE` was not given — so `SPACESHIPS_MISSION=3` on its own
    /// is enough to launch the boss fight.
    ///
    /// Unparseable rather than invalid on purpose: an unrecognised mode name
    /// should start the game, not refuse to. `std::env::var` on
    /// `wasm32-unknown-unknown` always returns `Err`, so the web build gets the
    /// defaults with no `cfg`.
    #[must_use]
    pub fn from_env() -> MatchSetup {
        let mut setup = MatchSetup::default();
        let mut mode_given = false;
        if let Ok(mode) = std::env::var("SPACESHIPS_MODE") {
            match parse_mode(&mode) {
                Some(m) => {
                    setup.mode = m;
                    mode_given = true;
                }
                None => warn!("SPACESHIPS_MODE={mode} is not a mode this client builds"),
            }
        }
        // A bare number refines the mode it belongs to, and picks it when no
        // mode was named at all.
        if let Some(n) = env_u8("SPACESHIPS_MISSION") {
            match setup.mode {
                Mode::Campaign(_) => setup.mode = Mode::Campaign(n),
                _ if !mode_given => setup.mode = Mode::Campaign(n),
                other => warn!("SPACESHIPS_MISSION={n} ignored: the mode is {other:?}"),
            }
        }
        if let Some(n) = env_u8("SPACESHIPS_TRIAL") {
            match setup.mode {
                Mode::Trials(_) => setup.mode = Mode::Trials(n),
                _ if !mode_given => setup.mode = Mode::Trials(n),
                other => warn!("SPACESHIPS_TRIAL={n} ignored: the mode is {other:?}"),
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
        setup.normalize();
        setup
    }
}

/// One `SPACESHIPS_MODE` value, in the JS lobby's spelling.
///
/// Split into a free function because it is pure and the round trip through
/// every spelling is the thing worth testing.
fn parse_mode(raw: &str) -> Option<Mode> {
    let s = raw.trim().to_ascii_lowercase();
    match s.as_str() {
        "train" | "training" => return Some(Mode::Training),
        "skirmish" => return Some(Mode::Skirmish),
        "tutorial" => return Some(Mode::Tutorial),
        "campaign" | "mission" => return Some(Mode::Campaign(1)),
        "trials" | "trial" => return Some(Mode::Trials(1)),
        _ => {}
    }
    // `trials2`, `campaign3`: the lobby's own spelling for the numbered modes.
    let split = s.find(|c: char| c.is_ascii_digit())?;
    let n = s[split..].parse::<u8>().ok()?;
    match &s[..split] {
        "campaign" | "mission" => Some(Mode::Campaign(n)),
        "trials" | "trial" => Some(Mode::Trials(n)),
        _ => None,
    }
}

/// A small unsigned environment variable, or `None` if absent or unparseable.
fn env_u8(key: &str) -> Option<u8> {
    std::env::var(key).ok()?.trim().parse::<u8>().ok()
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

/// Tear the current match down and build this one.
///
/// The whole interface a lobby needs. Write one with `MessageWriter<StartMatch>`
/// and [`apply_start_match`] rebuilds [`SimWorld`], [`Roster`] and [`SimFrame`]
/// in the next `PreUpdate` — before that frame's fixed steps run, so no tick
/// ever sees a half-swapped world.
///
/// # What this does *not* do
///
/// It does not tell the renderer. `scene.rs`, `weapons.rs` and `audio.rs` keep
/// id-keyed registries of spawned Bevy entities, and a rebuilt world reuses the
/// same ids for different ships. Until those modules despawn on a match change
/// this is a dev entry point — correct for "launch straight into mission 3",
/// not yet for "return to hangar and pick another". The signal they should
/// watch for is this message; it is public for that reason.
#[derive(Message, Debug, Clone, PartialEq)]
pub struct StartMatch(pub MatchSetup);

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
    ///
    /// The capital ship answers to [`BOSS_CALLSIGN`] without being a pilot. It
    /// has twenty ids and no scoreboard row (see [`Roster::sync`]), but the
    /// killfeed still has to be able to say what killed you, and `main.js:2778`
    /// solves it the same way — `scores.set(BOSS_ID_BASE, { name: 'Capital
    /// Ship' })`, a name entry that is never rendered as a scoreboard row.
    #[must_use]
    pub fn callsign(&self, id: EntityId) -> &str {
        if sim::world::is_boss_hitbox(id) {
            return BOSS_CALLSIGN;
        }
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
                // A ship the match setup did not name: a mid-match join, or a
                // campaign wave that `sim::campaign::update` put on the field
                // between ticks. Give it something stable until a `players`
                // message — or [`name_campaign_wave`] — renames it.
                let label = if s.bot.is_campaign_bot {
                    format!("Hostile {}", s.id)
                } else {
                    format!("PILOT {}", s.id)
                };
                self.name(s.id, label);
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
        log_match(&setup, &world);

        app.insert_resource(SimWorld(world))
            .insert_resource(roster)
            .insert_resource(setup)
            .init_resource::<SimFrame>()
            .init_resource::<PlayerInput>()
            .init_resource::<EdgeLatch>()
            .add_message::<StartMatch>()
            // Before `RunFixedMainLoop`, so a match built this frame is the one
            // this frame's fixed steps advance.
            .add_systems(
                PreUpdate,
                (forward_launch_requests, apply_start_match).chain(),
            )
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

/// Rebuilds the match when a [`StartMatch`] lands.
///
/// Only the last message of the frame is honoured: two requests in one frame
/// means two clicks, and building the loser first would be a wasted 280-rock
/// field generation. The input latch is cleared with the world, so a missile
/// key pressed on the menu does not fire on the new match's first tick.
/// Turns the lobby's [`crate::ui::LaunchRequest`] into a [`StartMatch`].
///
/// `ui.rs` produces the selection but deliberately does not rebuild the world
/// itself — `scene.rs` spawns its static geometry once in `Startup` and would be
/// left holding stale entities. Routing through the message that already exists
/// means the lobby reuses the rebuild path this module already tests.
///
/// `online` is ignored for now: `new_match` serves solo modes, and a networked
/// match takes its spawns from the server's `start` message rather than a seed.
/// The field is carried on the message so that path has what it needs when it
/// lands.
fn forward_launch_requests(
    mut requests: MessageReader<crate::ui::LaunchRequest>,
    mut start: MessageWriter<StartMatch>,
) {
    if let Some(req) = requests.read().last() {
        start.write(StartMatch(req.setup.clone()));
    }
}

fn apply_start_match(
    mut requests: MessageReader<StartMatch>,
    mut setup: ResMut<MatchSetup>,
    mut world: ResMut<SimWorld>,
    mut frame: ResMut<SimFrame>,
    mut roster: ResMut<Roster>,
    mut latch: ResMut<EdgeLatch>,
) {
    let Some(StartMatch(next)) = requests.read().last() else {
        return;
    };
    let mut next = next.clone();
    next.normalize();

    let (built, names) = new_match(&next);
    log_match(&next, &built);
    world.0 = built;
    *roster = names;
    frame.0 = Frame::new();
    *latch = EdgeLatch::default();
    *setup = next;
}

/// One line naming what was just built. Shared by startup and [`StartMatch`].
fn log_match(setup: &MatchSetup, world: &SimWorldState) {
    info!(
        "match: {:?} on {:?}, seed {:#x}, {} ships, {} rocks",
        setup.mode,
        setup.map,
        setup.seed,
        world.ships.len(),
        world.asteroids.len(),
    );
}

/// Runs one fixed simulation step and republishes [`SimFrame`] and [`Roster`].
#[allow(clippy::too_many_arguments)]
/// Whether an open menu stops the simulation.
///
/// Solo only. See [`fixed_tick`] for what freezing a networked match did.
fn freezes_the_world(menu_up: bool, authority: sim::world::Authority) -> bool {
    menu_up && authority == sim::world::Authority::Local
}

fn fixed_tick(
    mut world: ResMut<SimWorld>,
    setup: Res<MatchSetup>,
    input: Res<PlayerInput>,
    mut latch: ResMut<EdgeLatch>,
    mut frame: ResMut<SimFrame>,
    mut roster: ResMut<Roster>,
    mut inbox: ResMut<crate::net::NetInbox>,
    lobby: Option<Res<crate::ui::LobbyOpen>>,
    mut tape: ResMut<crate::replay::Tape>,
    theatre: Option<ResMut<crate::replay::Theatre>>,
) {
    // Nobody is flying a replay, so none of what follows applies: the stick and
    // the server's traffic both come off the recording, and `replay.rs`'s
    // transport decides whether this step happens at all. What *is* shared is
    // everything after the step, which is the point — a replayed tick produces
    // the same `Frame` a live one did, so the renderer cannot tell them apart.
    if let Some(mut theatre) = theatre {
        if theatre.running() {
            if let Some(replayed) = theatre.step(&mut world.0) {
                frame.0 = replayed;
                step_modes(&mut world.0, &mut frame.0, &mut roster, &setup, TICK_DT);
            }
        }
        roster.sync(&world.0);
        return;
    }

    // The lobby is not an overlay on a running game: while it is up, a *solo*
    // match is frozen. Without this the world keeps ticking behind the menu, so
    // bots hunt and kill a player who is reading a mission brief and cannot
    // fly. The JS has the same property for a different reason -- there, no
    // game exists until the lobby calls `startGame`.
    //
    // **Only solo.** Under `Authority::Server` there is nothing this client can
    // pause: the server runs the match clock, the other pilots keep flying, and
    // it goes on broadcasting whatever happens. Freezing here stopped the
    // world, and — because the early return sat *above* the drain below — it
    // also stopped the inbox being emptied, so every frame the server sent
    // piled up unread. A player who opened the menu online therefore stopped
    // moving, stopped seeing anyone else move, and, if they were dead at the
    // time, never respawned: the `respawn` broadcast was sitting in a queue
    // nobody drained. That was reported as being stuck on the destroyed screen
    // forever.
    //
    // Input is still latched (above, in `PreUpdate`), so a key pressed on the
    // frame the menu closes is not swallowed; the latch simply drains into the
    // first tick that actually runs.
    let menu_up = lobby.is_some_and(|l| l.0);
    if freezes_the_world(menu_up, world.0.authority) {
        return;
    }

    // Menu up in a networked match: the world still has to advance and the
    // server's frames still have to be applied, but the pilot is reading a menu
    // and is not flying. Neutral input rather than the last stick position, so
    // a ship whose owner is in the settings page does not hold a turn.
    let player = if menu_up {
        sim::world::Input {
            id: LOCAL_ID,
            ..Default::default()
        }
    } else {
        let mut p = input.0;
        latch.drain_into(&mut p);
        p
    };

    // Solo is `Authority::Local` and resolves its own damage, so this is empty
    // and the call is exactly what it always was. In multiplayer `net.rs` fills
    // it in `PreUpdate` from the server's frames, and draining it here — rather
    // than reading it — is what makes a rendered frame that runs three fixed
    // steps apply each event once.
    let events = std::mem::take(&mut inbox.0);

    // The dashcam, and the one contract it has: the slices as they go *in*.
    // Recording after the call would still see the right values, but the queue
    // above has been drained by then and a future edit that moved this line
    // below the tick would silently record an empty event log — which is
    // precisely the log a multiplayer replay cannot do without.
    tape.push(&[player], &events);

    frame.0 = tick(&mut world.0, &[player], &events, TICK_DT);

    step_modes(&mut world.0, &mut frame.0, &mut roster, &setup, TICK_DT);
    roster.sync(&world.0);
}

/// The part of a solo mode's step that `sim::tick` does not run.
///
/// Two jobs, both of them the consequence of a gap in `sim` rather than of a
/// rendering concern, and both documented at their call sites:
/// [`name_campaign_wave`] and [`score_trials`]. Public so a test — and, later, a
/// replay harness — can drive a step exactly the way [`fixed_tick`] does
/// without going through Bevy.
pub fn step_modes(
    world: &mut SimWorldState,
    frame: &mut Frame,
    roster: &mut Roster,
    setup: &MatchSetup,
    dt: f64,
) {
    if world.campaign.is_some() {
        name_campaign_wave(world, roster, setup.hard_mode);
    }
    if world.trials.is_some() {
        score_trials(world, frame, dt);
    }
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
    let mut setup = setup.clone();
    setup.normalize();

    // A solo `Mode` gives `authority == Local`, which is what lets the
    // simulation resolve its own damage — there is no server.
    debug_assert!(setup.mode.is_solo());
    let mut world = SimWorldState::new(setup.seed, rules, setup.mode, setup.map);

    // The moon and both motherships came with `World::new`; the field does not.
    // It draws from the dedicated `field` RNG stream, so the layout is stable
    // even as other subsystems are added.
    //
    // `asteroids::generate` already branches on the mode: the campaign gets its
    // three boxed slabs (280 rocks) and a trial gets its course's density
    // (120 / 150 / 180 / 210), both off `World::mode`. Nothing to pass.
    sim::asteroids::populate(&mut world);

    let (spawn, facing) = player_start(&rules, &setup);
    let mut me = Ship::spawn(LOCAL_ID, ShipKind::Local, spawn, facing, &rules);
    me.team = Some(Team::Zero);
    world.ships.push(me);
    world.local_id = Some(LOCAL_ID);

    let mut roster = Roster::default();
    roster.name(LOCAL_ID, setup.callsign.clone());

    match setup.mode {
        Mode::Training => spawn_training_bot(&mut world, &mut roster, &setup),
        Mode::Skirmish => spawn_skirmish(&mut world, &mut roster, &setup),
        Mode::Campaign(_) => start_campaign(&mut world, &mut roster, &setup),
        Mode::Trials(_) => start_trials(&mut world),
        // Tutorial has no opponents by design (`main.js:2925` falls through),
        // and a solo build never constructs `Mode::Multiplayer` — that world
        // comes from the server's `start` message, in `net.rs`.
        Mode::Tutorial | Mode::Multiplayer => {}
    }

    seed_scoreboard(&mut world);
    roster.sync(&world);
    (world, roster)
}

/// Where the local player begins.
///
/// Everything but a trial starts on its team's anchor. A trial starts at
/// [`sim::rules::SpawnRules::trials_start`], `(0, 20, -510)` — 130 units short
/// of checkpoint 0 and lined up on it, which is what makes the opening
/// countdown a grid start rather than a hunt for the first ring
/// (`main.js:224`).
fn player_start(rules: &Rules, setup: &MatchSetup) -> (SimVec3, SimQuat) {
    if let Some(at) = start_override() {
        return (at, SimQuat::IDENTITY);
    }
    match setup.mode {
        Mode::Trials(_) => (rules.spawn.trials_start, SimQuat::IDENTITY),
        _ => team_anchor(rules, setup.map, Team::Zero),
    }
}

/// `SPACESHIPS_START=x,y,z` puts the player anywhere on the map.
///
/// A screenshot hook, in the same family as `SPACESHIPS_SCREENSHOT`,
/// `SPACESHIPS_COCKPIT` and `SPACESHIPS_FX_SCENE`, and it earns its keep on
/// exactly the job those do: looking at a change. Everything the terrain map
/// draws is 1,400 units from a spawn point, which at 80 u/s is twenty seconds of
/// flying per look and a ship that has to survive the trip — so without this,
/// checking whether a ravine reads as a ravine means either playing to it or
/// guessing from a plan view.
///
/// Native only in effect: `std::env::var` always fails on
/// `wasm32-unknown-unknown`, so the web build takes the normal spawn with no
/// `cfg`.
fn start_override() -> Option<SimVec3> {
    let spec = std::env::var("SPACESHIPS_START").ok()?;
    let mut parts = spec.split(',').map(|p| p.trim().parse::<f64>());
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(Ok(x)), Some(Ok(y)), Some(Ok(z)), None) => Some(SimVec3::new(x, y, z)),
        _ => {
            warn!("SPACESHIPS_START={spec} is not an x,y,z position");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Campaign
// ---------------------------------------------------------------------------

/// Puts a mission on the field.
///
/// One call. [`sim::campaign::init`] reads the mission number out of
/// [`Mode::Campaign`], builds [`sim::world::CampaignState`] with three lives and
/// the opening checkpoint, parks the twenty boss hitboxes (ids `9000..9019`,
/// dead until the capital ship engages), and spawns wave 1 from the mission's
/// own wave table — 3/5/4 for mission 1, 4/6/5 for 2, 5/7/6 for 3.
///
/// Everything after that is `sim::campaign::update`, which `sim::tick` already
/// calls: counting the wave down, moving the checkpoint forward, the
/// between-waves pause, spending a life, engaging the boss, and ending the
/// mission. This module's remaining share is the two things `sim` cannot do —
/// naming the bots, and the missile-timer fix-up described in
/// [`name_campaign_wave`].
fn start_campaign(world: &mut SimWorldState, roster: &mut Roster, setup: &MatchSetup) {
    sim::campaign::init(world, setup.hard_mode);
    name_campaign_wave(world, roster, setup.hard_mode);
}

/// Names the current wave's bots, and arms the ones `sim` left disarmed.
///
/// Called once at setup and once per tick, because a wave is spawned *inside*
/// `sim::campaign::update` — there is no event on the way in, only
/// [`sim::world::SimEvent::WaveComplete`] on the way out, and the new wave
/// arrives [`sim::rules::CampaignRules::wave_gap`] seconds later. Cheap: the
/// roster lookup rejects every already-known bot, so the body runs `count`
/// times per wave and never again.
///
/// # The arming
///
/// `sim::campaign::spawn_wave_with` builds its bots with `Ship::spawn` and then
/// sets four `BotState` fields by hand rather than calling
/// [`sim::bot::init`] — so a campaign bot starts with
/// `missile_timer == 0.0` and fires its first missile the instant it has range
/// and line of sight, where every other bot in the game waits out
/// `bot::missile_delay`. That is a `sim` bug, not a rendering one; the fix is
/// one line in `spawn_wave_with` and is reported rather than made here. Until
/// then this re-runs `bot::init` on a bot that still has the tell-tale zero,
/// which is exactly what [`push_bot`] does for a skirmish. The guard means the
/// pass becomes a no-op the moment `sim` is fixed, rather than re-rolling the
/// delay a second time.
fn name_campaign_wave(world: &mut SimWorldState, roster: &mut Roster, hard_mode: bool) {
    let Some(camp) = world.campaign.as_ref() else {
        return;
    };
    if camp.wave_bot_ids.iter().all(|id| roster.get(*id).is_some()) {
        return;
    }
    let ids: Vec<EntityId> = camp.wave_bot_ids.clone();
    let rules = world.rules;
    let SimWorldState { ships, rng, .. } = world;

    for (i, id) in ids.iter().copied().enumerate() {
        if roster.get(id).is_some() {
            continue;
        }
        if let Some(s) = ships.iter_mut().find(|s| s.id == id) {
            if s.bot.missile_timer <= 0.0 {
                sim::bot::init(s, hard_mode, true, &rules, &mut rng.bots);
            }
        }
        // `main.js:2750` — `spawnBot(id, 1, pos, \`Enemy ${i + 1}\`)`. The
        // numbering restarts each wave, as it does there.
        roster.name(id, format!("Enemy {}", i + 1));
    }
}

// ---------------------------------------------------------------------------
// Trials
// ---------------------------------------------------------------------------

/// Puts a time-trial circuit on the field.
///
/// The course itself is [`sim::rules::trial_checkpoints`] — 12, 14, 16 or 18
/// rings — copied into the world so a custom track is world state rather than a
/// code change, which is what [`TrialsState::checkpoints`] documents.
///
/// Two starting values are not defaults and are the reason this is not
/// `TrialsState::default()`:
///
/// - `cp_cooldown` starts at [`sim::rules::TrialsRules::cp_cooldown`], not zero
///   (`main.js:372`). The grid is 130 units from ring 0 and the trigger is 55,
///   so it changes nothing today — but it is the shipped value, and a course
///   whose ring 0 sat on the grid would arm instantly without it.
/// - `countdown_active` starts **true**. That is not decoration: `sim::tick`'s
///   pre-race hold returns before the whole step while it runs, so the three
///   seconds are a real freeze on the ship, the bots and the field, exactly as
///   `main.js:1198` does.
fn start_trials(world: &mut SimWorldState) {
    let Mode::Trials(trial) = world.mode else {
        return;
    };
    let rules = world.rules;
    world.trials = Some(TrialsState {
        trial,
        checkpoints: sim::rules::trial_checkpoints(trial).to_vec(),
        next_cp: 0,
        timer: 0.0,
        lap: 0,
        running: false,
        // Session-only. Persisting a personal best is `localStorage` in the JS
        // (`main.js:1765`) and is the lobby's job on this side too.
        best_lap: None,
        last_lap: None,
        cp_cooldown: rules.trials.cp_cooldown,
        countdown: rules.trials.countdown,
        countdown_active: true,
    });
}

/// Advances a trial run: the ring test, the lap clock, and the crash reset.
///
/// # Why this is here and not in `sim`
///
/// It should be in `sim`, and the intent is that it moves there. It is
/// simulation state — [`TrialsState`] is a `World` field, the boost award
/// mutates [`sim::world::Ship::boost_meter`], and the two events it emits
/// ([`SimEvent::CheckpointPassed`], [`SimEvent::LapComplete`]) are declared in
/// `sim::world` and produced by nothing. `sim::tick` honours the pre-race hold
/// and fills [`TrialsHud`] from [`TrialsState`], but no phase ever *writes*
/// that state, so a trial launched without this runs a lap counter stuck at
/// zero past rings that never trigger.
///
/// It lives here because the alternative was leaving trials unplayable. The
/// arithmetic is a faithful port of `main.js:1772`–`:1799` and reads only
/// `World`, so lifting it into a `sim::trials` module is a move, not a rewrite:
/// a `pub fn update(world, dt, events)` called from `tick` between the campaign
/// step and `fire_weapons`, plus the crash reset in `respawn_ship`'s match on
/// where a ship comes back. The reported `sim` change is exactly that.
///
/// Running it **after** the tick rather than before is deliberate: the ring test
/// must see where the ship ended up this step, not where it started. The cost
/// is that `Frame::hud.trials` was built one phase too early, so
/// [`refresh_trials_hud`] rewrites it — along with `boost01`, which a
/// checkpoint award moves.
fn score_trials(world: &mut SimWorldState, frame: &mut Frame, dt: f64) {
    // A crash reset first, because `sim` respawned the ship in phase 2 and
    // everything below reads its position.
    reset_run_on_respawn(world, frame);

    // `main.js:1198` returns before the update block while the countdown runs,
    // and `sim::tick` already did exactly that — the whole step was skipped, so
    // there is nothing to score and no time to add.
    if world.trials.as_ref().is_some_and(|t| t.countdown_active) {
        refresh_trials_hud(world, frame);
        return;
    }

    step_checkpoints(world, frame, dt);
    refresh_trials_hud(world, frame);
}

/// The ring test and the lap clock. `main.js:1772`–`:1799`.
fn step_checkpoints(world: &mut SimWorldState, frame: &mut Frame, dt: f64) {
    let rules = world.rules;
    let Some(local) = world.local_id else { return };
    let Some((alive, pos)) = world.ship(local).map(|s| (s.alive, s.pos)) else {
        return;
    };

    let Some(trials) = world.trials.as_mut() else {
        return;
    };
    if trials.checkpoints.is_empty() {
        return;
    }

    let mut passed: Option<usize> = None;
    let mut lap: Option<(f64, bool)> = None;

    // `else if`, not two `if`s: the cooldown tick and the ring test are
    // mutually exclusive in a frame, so a ring cannot fire on the frame its
    // cooldown reaches zero. That one frame of dead time is the shipped
    // behaviour and it is what stops a slow pass through a ring re-arming it.
    if trials.cp_cooldown > 0.0 {
        trials.cp_cooldown -= dt;
    } else if alive
        && pos.distance(trials.checkpoints[trials.next_cp]) < rules.trials.cp_trigger_dist
    {
        let index = trials.next_cp;
        let at_start = index == 0;
        trials.next_cp = (index + 1) % trials.checkpoints.len();
        trials.cp_cooldown = rules.trials.cp_cooldown;

        if at_start {
            if trials.running {
                // The second and every later crossing of ring 0 closes a lap.
                // The clock the time comes off started on the *previous* one,
                // which is why a run's first reported time is the interval
                // between crossings one and two and not the time since launch.
                let time = trials.timer;
                let is_best = trials.best_lap.is_none_or(|best| time < best);
                trials.last_lap = Some(time);
                if is_best {
                    trials.best_lap = Some(time);
                }
                trials.timer = 0.0;
                trials.lap += 1;
                lap = Some((time, is_best));
            } else {
                // The first crossing is the start line: it starts the clock at
                // zero rather than reporting anything.
                trials.running = true;
                trials.timer = 0.0;
                trials.lap = 1;
            }
        }
        passed = Some(index);
    }

    // After the pass, so the lap that was just closed is not charged for this
    // step and the new one is (`main.js:1802`).
    if trials.running && alive {
        trials.timer += dt;
    }

    let Some(index) = passed else { return };

    // The award is on the ship, so it waits for the `trials` borrow to end.
    // `main.js:1778`: top the tank up by `cp_boost_award` and clear the idle
    // timer, so the refund is usable immediately rather than after the
    // recharge delay.
    if let Some(s) = world.ship_mut(local) {
        s.boost_meter = (s.boost_meter + rules.trials.cp_boost_award).min(rules.ship.max_boost);
        s.boost_idle = 0.0;
        frame.hud.boost01 = (s.boost_meter / rules.ship.max_boost) as f32;
    }

    frame.events.push(SimEvent::CheckpointPassed {
        index,
        lap_time: lap.map(|(time, _)| time),
    });
    if let Some((time, is_best)) = lap {
        frame.events.push(SimEvent::LapComplete { time, is_best });
    }
}

/// Resets the run when the player crashes. `main.js:3346`–`:3356`.
///
/// `sim`'s `respawn_ship` has an answer for the campaign and an answer for
/// everything else, and a trial is neither: it puts the ship back on the team-0
/// anchor at `(0, 0, -540)` with jitter, 30 units below and behind the grid,
/// with the run's checkpoint index untouched. So this catches the respawn `sim`
/// announced, moves the ship to the grid, and rewinds the lap.
///
/// The [`SimEvent::ShipRespawned`] payload and the frame's own [`ShipView`] are
/// corrected too, not just the world: `scene.rs` snaps its interpolator to the
/// pose in the frame on exactly that event, so leaving the old position there
/// would snap the ship to the wrong place and then streak it 400 units to the
/// grid over the following tick.
///
/// [`ShipView`]: sim::world::ShipView
fn reset_run_on_respawn(world: &mut SimWorldState, frame: &mut Frame) {
    let Some(local) = world.local_id else { return };
    let respawned = frame
        .events
        .iter()
        .any(|e| matches!(e, SimEvent::ShipRespawned { id, .. } if *id == local));
    if !respawned {
        return;
    }

    let start = world.rules.spawn.trials_start;
    // A longer dead time than an ordinary pass, so respawning on top of ring 0
    // does not instantly re-arm it (`main.js:3354`).
    let cooldown = world.rules.trials.cp_cooldown_after_reset;
    if let Some(t) = world.trials.as_mut() {
        t.running = false;
        t.timer = 0.0;
        t.next_cp = 0;
        t.cp_cooldown = cooldown;
    }
    if let Some(s) = world.ship_mut(local) {
        s.pos = start;
        s.quat = SimQuat::IDENTITY;
        s.vel = SimVec3::ZERO;
    }

    for event in &mut frame.events {
        if let SimEvent::ShipRespawned { id, pos } = event {
            if *id == local {
                *pos = start;
            }
        }
    }
    for view in &mut frame.ships {
        if view.id == local {
            view.pos = vec3f(start);
            view.quat = [0.0, 0.0, 0.0, 1.0];
        }
    }
}

/// Rewrites `Frame::hud.trials` from the state this module just advanced.
///
/// A mirror of `sim::tick`'s own `trials_hud`, which ran before the scoring
/// did. It goes away with [`score_trials`].
fn refresh_trials_hud(world: &SimWorldState, frame: &mut Frame) {
    let Some(t) = world.trials.as_ref() else {
        return;
    };
    let next = t
        .checkpoints
        .get(t.next_cp)
        .copied()
        .unwrap_or(SimVec3::ZERO);
    frame.hud.trials = TrialsHud {
        active: true,
        running: t.running,
        lap: t.lap,
        timer: t.timer as f32,
        // Negative is "no time yet"; `Option<f64>` does not survive the
        // `#[repr(C)]` narrowing `TrialsHud` is under.
        best_lap: t.best_lap.map_or(-1.0, |x| x as f32),
        last_lap: t.last_lap.map_or(-1.0, |x| x as f32),
        next_cp: t.next_cp as u32,
        next_cp_pos: vec3f(next),
        countdown: t.countdown as f32,
    };
}

/// A simulation vector as the frame carries it.
fn vec3f(v: SimVec3) -> [f32; 3] {
    [v.x as f32, v.y as f32, v.z as f32]
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
    /// Pause is a solo affordance. Online there is nothing to pause.
    ///
    /// The bug: `fixed_tick` froze the world whenever the menu was up, and its
    /// early return sat above the inbox drain — so a networked player who
    /// opened the menu stopped ticking *and* stopped consuming the server's
    /// frames, which piled up unread. Dead at the time, they never saw the
    /// `respawn` broadcast and sat on the destroyed screen forever.
    #[test]
    fn only_a_solo_match_pauses() {
        use sim::world::Authority;

        assert!(
            freezes_the_world(true, Authority::Local),
            "solo with the menu up must freeze, or bots kill a player reading it",
        );
        assert!(
            !freezes_the_world(true, Authority::Server),
            "a networked match must keep ticking and keep draining the inbox",
        );
        assert!(!freezes_the_world(false, Authority::Local));
        assert!(!freezes_the_world(false, Authority::Server));
    }

    use super::*;
    use sim::campaign::{CHECKPOINT_BOSS, CHECKPOINT_START};
    use sim::rules::{campaign_waves, trial_checkpoints, BOSS_HITBOX_COUNT, BOSS_ID_BASE};
    use sim::world::{CampaignPhase, CampaignState, ShipFlags};

    fn setup(mode: Mode) -> MatchSetup {
        MatchSetup {
            mode,
            ..MatchSetup::default()
        }
    }

    /// A built match plus the per-tick work [`fixed_tick`] does around
    /// [`tick`], so a test drives exactly what the game drives and a bug in
    /// [`step_modes`] cannot hide behind a test harness that skips it.
    struct Solo {
        world: SimWorldState,
        roster: Roster,
        setup: MatchSetup,
        frame: Frame,
    }

    impl Solo {
        fn new(setup: MatchSetup) -> Solo {
            let (world, roster) = new_match(&setup);
            Solo {
                world,
                roster,
                setup,
                frame: Frame::new(),
            }
        }

        fn step(&mut self, input: SimInput) {
            self.frame = tick(&mut self.world, &[input], &[], TICK_DT);
            step_modes(
                &mut self.world,
                &mut self.frame,
                &mut self.roster,
                &self.setup,
                TICK_DT,
            );
            self.roster.sync(&self.world);
        }

        /// `secs` of ticks, returning every event along the way.
        fn run(&mut self, input: SimInput, secs: f64) -> Vec<SimEvent> {
            let mut events = Vec::new();
            for _ in 0..(secs * TICK_HZ).round() as u32 {
                self.step(input);
                events.extend(self.frame.events.iter().copied());
            }
            events
        }

        fn campaign(&self) -> &CampaignState {
            self.world.campaign.as_ref().expect("no campaign state")
        }

        fn trials(&self) -> &TrialsState {
            self.world.trials.as_ref().expect("no trials state")
        }

        fn me(&self) -> &Ship {
            self.world.local_ship().expect("no local ship")
        }

        /// Wipes the current wave without waiting for a dogfight. The path
        /// under test is the mission script, not the ballistics.
        fn clear_wave(&mut self) {
            let ids = self.campaign().wave_bot_ids.clone();
            assert!(!ids.is_empty(), "no wave to clear");
            for id in ids {
                let s = self.world.ship_mut(id).expect("wave bot vanished");
                s.alive = false;
                s.hp = 0;
            }
        }

        /// Clears every wave and waits out both pauses, leaving the capital
        /// ship engaged.
        fn advance_to_boss(&mut self) {
            for _ in 0..sim::campaign::WAVES_PER_MISSION {
                self.clear_wave();
                // Longer than `wave_gap` (3.5) and `boss_gap` (4.8).
                self.run(idle(), 6.0);
            }
            assert!(self.campaign().boss_active, "the boss never engaged");
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

    /// A point well inside the moon (radius 80 at the origin), which kills
    /// outright on contact and needs no shooter.
    ///
    /// Off the centre on purpose, though no longer because it has to be: this
    /// used to note that a body's dead centre reported no contact at all, and
    /// since `1bbd4ef` it does — `sim::ship::sphere_penetration` exits a
    /// centred overlap along `+x`, an arbitrary but fixed direction, rather
    /// than giving up the way `main.js:2201`'s `distSq > 0.0001` guard does.
    /// Kept off-centre anyway so this test exercises the ordinary path and not
    /// that one special case.
    const MOON_INTERIOR: SimVec3 = SimVec3::new(0.0, 0.0, 40.0);

    /// Hands off the controls but keeps the local ship *flown*.
    ///
    /// `SimInput::default()` carries `id: 0`, which matches nothing, so
    /// `sim::tick`'s `integrate_players` skips the player entirely — no
    /// physics and, more to the point here, no collision. A pilot who has let
    /// go of the stick is still in the world; this is that pilot.
    fn idle() -> SimInput {
        SimInput {
            id: LOCAL_ID,
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

    /// Every spelling `lobby/solo.js` can produce, plus the numbered forms the
    /// JS carries as a separate `missionId` option.
    #[test]
    fn every_lobby_mode_string_parses() {
        assert_eq!(parse_mode("skirmish"), Some(Mode::Skirmish));
        assert_eq!(parse_mode("train"), Some(Mode::Training));
        assert_eq!(parse_mode("TUTORIAL"), Some(Mode::Tutorial));
        assert_eq!(parse_mode("trials"), Some(Mode::Trials(1)));
        assert_eq!(parse_mode("trials2"), Some(Mode::Trials(2)));
        assert_eq!(parse_mode("trials4"), Some(Mode::Trials(4)));
        assert_eq!(parse_mode("campaign"), Some(Mode::Campaign(1)));
        assert_eq!(parse_mode(" Campaign3 "), Some(Mode::Campaign(3)));
        assert_eq!(parse_mode("nonsense"), None);
        assert_eq!(parse_mode("trials9x"), None);
    }

    /// A number out of range must still start something, and the campaign must
    /// end up on the map its geometry is written in.
    #[test]
    fn a_setup_normalizes_rather_than_refuses() {
        assert_eq!(MatchSetup::campaign(9).mode, Mode::Campaign(3));
        assert_eq!(MatchSetup::campaign(0).mode, Mode::Campaign(1));
        assert_eq!(MatchSetup::trial(9).mode, Mode::Trials(4));
        assert_eq!(MatchSetup::trial(1).mission_number(), Some(1));
        assert_eq!(MatchSetup::default().mission_number(), None);

        let mut on_terrain = MatchSetup {
            mode: Mode::Campaign(2),
            map: MapKind::Terrain,
            ..MatchSetup::default()
        };
        on_terrain.normalize();
        assert_eq!(
            on_terrain.map,
            MapKind::Space,
            "the campaign is a space map"
        );
    }

    // -- the campaign ------------------------------------------------------

    /// The headline campaign requirement: each mission opens with *its own*
    /// first wave — 3, 4 and 5 enemies — on the far team, armed, and named.
    #[test]
    fn each_mission_opens_with_its_own_first_wave() {
        for mission in 1..=CAMPAIGN_MISSIONS {
            let s = Solo::new(MatchSetup::campaign(mission));
            let want = campaign_waves(mission)[0].count;
            assert_eq!(want, [3, 4, 5][mission as usize - 1], "the wave table");

            let camp = s.campaign();
            assert_eq!(camp.mission, mission);
            assert_eq!(camp.phase, CampaignPhase::Wave);
            assert_eq!(camp.wave_index, 0);
            assert_eq!(camp.bots_alive, want);
            assert_eq!(camp.wave_bot_ids.len() as u32, want);
            assert_eq!(camp.lives, s.world.rules.campaign.lives);
            assert_eq!(camp.checkpoint_pos, CHECKPOINT_START);
            assert_eq!(s.me().pos, CHECKPOINT_START, "the grid is the checkpoint");

            for (i, id) in camp.wave_bot_ids.iter().enumerate() {
                let b = s.world.ship(*id).expect("wave bot");
                assert!(b.alive);
                assert_eq!(b.team, Some(Team::One));
                assert!(b.bot.is_campaign_bot, "bot {id} would respawn forever");
                assert!(
                    b.bot.missile_timer > 0.0,
                    "campaign bot {id} would fire a missile the instant it has range"
                );
                assert_eq!(s.roster.callsign(*id), format!("Enemy {}", i + 1));
            }
        }
    }

    /// The field, the boss's twenty hitboxes, and the one thing that must be
    /// true of them on frame one: they are asleep.
    #[test]
    fn a_mission_lays_out_the_corridor_and_parks_the_capital_ship() {
        let s = Solo::new(MatchSetup::campaign(1));

        let want: u32 = s
            .world
            .rules
            .campaign
            .asteroid_zones
            .iter()
            .map(|z| z.count)
            .sum();
        assert_eq!(want, 280, "three zones, 280 rocks");
        assert_eq!(s.world.asteroids.len() as u32, want);

        let hitboxes: Vec<_> = s
            .world
            .ships
            .iter()
            .filter(|x| x.kind == ShipKind::BossHitbox)
            .collect();
        assert_eq!(hitboxes.len(), BOSS_HITBOX_COUNT);
        assert!(
            hitboxes.iter().all(|h| !h.alive),
            "the capital ship is shootable before its phase"
        );

        // Twenty ids, no scoreboard rows, one name for the killfeed.
        assert!(s.roster.get(BOSS_ID_BASE).is_none());
        assert_eq!(s.roster.callsign(BOSS_ID_BASE), BOSS_CALLSIGN);
        assert_eq!(s.roster.callsign(BOSS_ID_BASE + 7), BOSS_CALLSIGN);
        assert!(!s
            .world
            .match_state
            .scores
            .iter()
            .any(|r| r.id >= BOSS_ID_BASE));
    }

    /// `Frame::boss` is what `hud.rs`'s boss bar and the capital ship's mesh
    /// read. Nothing filled it before the campaign path existed.
    #[test]
    fn the_frame_carries_the_capital_ship() {
        let mut s = Solo::new(MatchSetup::campaign(1));
        s.step(idle());
        let boss = s.frame.boss.expect("Frame::boss is empty in the campaign");
        assert_eq!(boss.max_hp, s.world.rules.campaign.boss_max_hp);
        assert_eq!(boss.hp, boss.max_hp);
        assert_eq!(boss.pos[2], s.world.rules.campaign.boss_base_pos.z as f32);
        assert!(!s.frame.hud.campaign.boss_active, "not engaged yet");
        assert!(s.frame.hud.campaign.active);
        assert_eq!(s.frame.hud.campaign.mission, 1);
        assert_eq!(s.frame.hud.campaign.lives, 3);

        // And nowhere else.
        let mut skirmish = Solo::new(setup(Mode::Skirmish));
        skirmish.step(idle());
        assert!(skirmish.frame.boss.is_none());
    }

    /// Clearing a wave has to move the mission on: the next wave's own count,
    /// the checkpoint dragged forward, and one `WaveComplete` — not one per
    /// tick for the rest of the match.
    #[test]
    fn clearing_a_wave_spawns_the_next_one() {
        let mut s = Solo::new(MatchSetup::campaign(2));
        let waves = campaign_waves(2);
        let first: Vec<EntityId> = s.campaign().wave_bot_ids.clone();

        s.clear_wave();
        let events = s.run(idle(), 6.0);

        let completions = events
            .iter()
            .filter(|e| matches!(e, SimEvent::WaveComplete { index: 0 }))
            .count();
        assert_eq!(completions, 1, "WaveComplete fired {completions} times");

        let camp = s.campaign();
        assert_eq!(camp.wave_index, 1);
        assert_eq!(camp.bots_alive, waves[1].count);
        assert_eq!(camp.wave_bot_ids.len() as u32, waves[1].count);
        assert!(!camp.between, "the pause never ended");
        // The checkpoint follows the wave, but wave 2's raw point --
        // (0, 20, -60) -- sits inside the moon, so `campaign` pushes it back
        // out radially. Assert the properties that survive that rather than the
        // raw coordinate: it stays on the approach side, and it is somewhere
        // the player can actually respawn.
        let cp = camp.checkpoint_pos;
        let rules = sim::rules::Rules::DEFAULT;
        assert!(
            (cp - rules.world.moon_pos).length() >= rules.world.moon_radius,
            "checkpoint {cp:?} is inside the moon"
        );
        assert!(
            cp.z < 0.0,
            "checkpoint {cp:?} is past the wave it stages for"
        );
        assert_eq!(cp.x, 0.0, "checkpoint {cp:?} left the centreline");

        // Fresh ids, and the bridge named and armed every one of them.
        for (i, id) in camp.wave_bot_ids.iter().enumerate() {
            assert!(!first.contains(id), "wave 2 reused wave 1's id {id}");
            let b = s.world.ship(*id).expect("wave 2 bot");
            assert!(b.bot.missile_timer > 0.0, "wave 2 bot {id} was never armed");
            assert_eq!(s.roster.callsign(*id), format!("Enemy {}", i + 1));
        }
        // The wave-1 corpses keep their rows, so a killfeed line still resolves.
        assert_eq!(s.roster.callsign(first[0]), "Enemy 1");
    }

    /// Three lives, and a death costs one. The respawn is the campaign's own:
    /// at the checkpoint, at 55 % health, not at the team anchor at full.
    #[test]
    fn a_death_spends_a_life_and_brings_you_back_hurt_at_the_checkpoint() {
        let mut s = Solo::new(MatchSetup::campaign(1));
        let rules = s.world.rules;
        assert_eq!(s.campaign().lives, 3);

        // Flying into the moon is instant death and needs no shooter
        // (`main.js:2222`). The spawn window has to be over first, which is the
        // one thing a test has to arrange.
        {
            let me = s.world.ship_mut(LOCAL_ID).unwrap();
            me.invuln_timer = 0.0;
            me.pos = MOON_INTERIOR;
        }
        let events = s.run(idle(), 0.1);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SimEvent::ShipDestroyed { id, .. } if *id == LOCAL_ID)),
            "the moon did not kill the player"
        );
        assert_eq!(s.campaign().lives, 2, "the death cost no life");
        assert!(!s.me().alive);
        assert!(
            s.campaign().warp_timer > 0.0,
            "the warp-in effect was not armed"
        );

        // `campaign_respawn_delay` is 1.5, shorter than the rest of the game's
        // 2.0, because the warp flash covers the gap. Stopping on the respawn
        // tick rather than running past it matters: `ship::respawn` clears the
        // regeneration delay, so a tenth of a second later the hit points are
        // 56 and the number under test is gone.
        let mut back = false;
        for _ in 0..(rules.combat.campaign_respawn_delay * TICK_HZ) as u32 + 30 {
            s.step(idle());
            back = s
                .frame
                .events
                .iter()
                .any(|e| matches!(e, SimEvent::ShipRespawned { id, .. } if *id == LOCAL_ID));
            if back {
                break;
            }
        }
        assert!(back, "the player never came back");

        let me = s.me();
        assert!(me.alive);
        assert_eq!(me.hp, sim::campaign::respawn_hp(&rules));
        assert_eq!(me.hp, 55, "55 % of 100 hit points");
        assert_eq!(
            me.pos, CHECKPOINT_START,
            "the respawn ignored the campaign checkpoint"
        );
        assert_eq!(s.frame.hud.campaign.lives, 2);
    }

    /// The last life fails the mission rather than respawning into an
    /// unwinnable one.
    #[test]
    fn the_third_death_fails_the_mission() {
        let mut s = Solo::new(MatchSetup::campaign(1));
        let mut failures = 0;
        for life in 1..=3 {
            {
                let me = s.world.ship_mut(LOCAL_ID).unwrap();
                assert!(me.alive, "life {life} never came back");
                me.invuln_timer = 0.0;
                me.pos = MOON_INTERIOR;
            }
            // Long enough to die and to be back on the field for the next one.
            let events = s.run(idle(), 2.5);
            failures += events
                .iter()
                .filter(|e| matches!(e, SimEvent::CampaignFailed))
                .count();
            assert_eq!(s.campaign().lives, 3 - life);
        }
        assert_eq!(failures, 1, "CampaignFailed fired {failures} times");
        assert_eq!(s.campaign().lives, 0);
        assert_eq!(s.campaign().phase, CampaignPhase::Failed);
        assert!(
            !s.me().alive,
            "a failed mission respawned the player anyway"
        );
    }

    /// Mission 3 to the capital ship: three waves, both pauses, the fight
    /// engaged, and twenty hitboxes awake at full health.
    #[test]
    fn mission_three_reaches_the_capital_ship() {
        let mut s = Solo::new(MatchSetup::campaign(3));
        assert_eq!(
            campaign_waves(3)
                .iter()
                .map(|w| w.count)
                .collect::<Vec<_>>(),
            vec![5, 7, 6],
        );

        s.clear_wave();
        s.run(idle(), 6.0);
        assert_eq!(s.campaign().bots_alive, 7);
        s.clear_wave();
        s.run(idle(), 6.0);
        assert_eq!(s.campaign().bots_alive, 6);

        s.clear_wave();
        let events = s.run(idle(), 6.0);

        assert!(
            events
                .iter()
                .any(|e| matches!(e, SimEvent::BossPhaseStarted)),
            "no BossPhaseStarted"
        );
        let camp = s.campaign();
        assert_eq!(camp.phase, CampaignPhase::Boss);
        assert!(camp.boss_active);
        assert_eq!(camp.boss_hp, s.world.rules.campaign.boss_max_hp);
        assert_eq!(camp.checkpoint_pos, CHECKPOINT_BOSS);
        assert!(
            s.world
                .ships
                .iter()
                .filter(|x| x.kind == ShipKind::BossHitbox)
                .all(|h| h.alive),
            "the hitboxes are still asleep"
        );

        let boss = s.frame.boss.expect("Frame::boss");
        assert_eq!(boss.hp, 2500);
        assert!(s.frame.hud.campaign.boss_active);
        assert!((s.frame.hud.campaign.boss_hp01 - 1.0).abs() < 1e-6);
    }

    /// The fight, end to end: the turrets shoot back, and the player's gun
    /// takes the capital ship's hit points down.
    #[test]
    fn the_capital_ship_fights_and_can_be_shot() {
        let mut s = Solo::new(MatchSetup::campaign(3));
        s.advance_to_boss();

        // Nose-on at 120 units from the hull's leading face. The field is what
        // this test is *not* measuring: a rock on the firing line would make it
        // about asteroid placement.
        s.world.asteroids.clear();
        {
            let me = s.world.ship_mut(LOCAL_ID).unwrap();
            me.pos = SimVec3::new(0.0, 0.0, 300.0);
            me.quat = SimQuat::IDENTITY;
            me.vel = SimVec3::ZERO;
        }
        // The hitboxes get the ordinary spawn window when the boss engages.
        s.run(idle(), s.world.rules.combat.spawn_invuln + 0.2);

        let before = s.campaign().boss_hp;
        let fire = SimInput {
            id: LOCAL_ID,
            fire: true,
            ..Default::default()
        };
        let mut events = Vec::new();
        for _ in 0..90 {
            // The turrets are lethal at this range; the test is about the
            // damage going the other way.
            let me = s.world.ship_mut(LOCAL_ID).unwrap();
            me.pos = SimVec3::new(0.0, 0.0, 300.0);
            me.hp = 100;
            s.step(fire);
            events.extend(s.frame.events.iter().copied());
        }

        let after = s.campaign().boss_hp;
        assert!(
            after < before,
            "a second and a half of fire did nothing: {before} -> {after}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SimEvent::Damaged { id, .. } if *id == BOSS_ID_BASE)),
            "no Damaged event named the capital ship"
        );
        assert_eq!(
            s.frame.boss.expect("Frame::boss").hp,
            after,
            "the frame's boss bar disagrees with the world"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SimEvent::Fired { owner, .. } if *owner == BOSS_ID_BASE)),
            "the turrets never fired back"
        );
    }

    /// Killing it ends the mission, which is the only route to
    /// `CampaignVictory` and the credit award behind it.
    #[test]
    fn destroying_the_capital_ship_wins_the_mission() {
        let mut s = Solo::new(MatchSetup::campaign(1));
        s.advance_to_boss();
        s.run(idle(), s.world.rules.combat.spawn_invuln + 0.2);

        let mut events = Vec::new();
        let max_hp = s.world.rules.campaign.boss_max_hp;
        sim::campaign::apply_boss_damage(&mut s.world, max_hp, Some(LOCAL_ID), &mut events);

        assert!(events
            .iter()
            .any(|e| matches!(e, SimEvent::CampaignVictory { lives_left: 3 })));
        assert_eq!(s.campaign().phase, CampaignPhase::Victory);
        s.step(idle());
        assert_eq!(s.frame.hud.campaign.boss_hp01, 0.0);
        assert!(
            s.world
                .ships
                .iter()
                .filter(|x| x.kind == ShipKind::BossHitbox)
                .all(|h| !h.alive),
            "a destroyed capital ship is still shootable"
        );
    }

    // -- time trials -------------------------------------------------------

    /// The four circuits, their ring counts, and their fields.
    #[test]
    fn each_circuit_gets_its_own_rings_and_its_own_field() {
        for (trial, rings, rocks) in [(1u8, 12, 120), (2, 14, 150), (3, 16, 180), (4, 18, 210)] {
            let s = Solo::new(MatchSetup::trial(trial));
            assert_eq!(s.trials().trial, trial);
            assert_eq!(s.trials().checkpoints.len(), rings);
            assert_eq!(s.trials().checkpoints, trial_checkpoints(trial));
            assert_eq!(s.world.asteroids.len(), rocks, "trial {trial}'s field");

            // The grid, not the team anchor: lined up 130 units short of ring 0.
            assert_eq!(s.me().pos, s.world.rules.spawn.trials_start);
            assert_eq!(s.world.ships.len(), 1, "a trial has no opponents");
            assert!(!s.world.match_state.active, "a trial runs no match clock");
        }
    }

    /// The pre-race countdown is control flow, not decoration: the whole step
    /// is skipped while it runs, so a throttle held through it must not move
    /// the ship a millimetre.
    #[test]
    fn the_countdown_freezes_the_race() {
        let mut s = Solo::new(MatchSetup::trial(1));
        let start = s.me().pos;
        assert!(s.trials().countdown_active);

        s.run(throttle_up(), 1.0);
        assert_eq!(s.me().pos, start, "the ship moved during the countdown");
        assert!(s.frame.hud.trials.active);
        assert!(s.frame.hud.trials.countdown > 0.0);
        assert!(!s.frame.hud.trials.running);
        assert_eq!(s.trials().cp_cooldown, s.world.rules.trials.cp_cooldown);

        s.run(throttle_up(), 2.5);
        assert!(!s.trials().countdown_active, "the countdown never ended");
        assert_eq!(s.frame.hud.trials.countdown, 0.0);
        assert!(s.me().pos.z > start.z, "the race never started");
    }

    /// Parks the ship on the next ring until it triggers, and reports the
    /// events of the tick it did. Waiting rather than forcing the cooldown is
    /// the point: the dead time between rings is part of what is under test.
    fn cross_next_ring(s: &mut Solo) -> Vec<SimEvent> {
        let target = s.trials().checkpoints[s.trials().next_cp];
        for _ in 0..240 {
            {
                let me = s.world.ship_mut(LOCAL_ID).unwrap();
                me.pos = target;
                me.vel = SimVec3::ZERO;
            }
            let before = s.trials().next_cp;
            s.step(idle());
            if s.trials().next_cp != before {
                return s.frame.events.clone();
            }
        }
        panic!("ring never triggered");
    }

    /// The requirement: a lap is timed from the crossing of ring 0 that starts
    /// the run to the *next* one, and not from launch. The clock the first
    /// crossing starts is at zero, so the first reported time is the interval
    /// between crossings one and two — the countdown and the run down to the
    /// grid are not charged to it.
    #[test]
    fn a_lap_is_timed_between_the_two_crossings_of_ring_zero() {
        let mut s = Solo::new(MatchSetup::trial(1));
        // The course, not the field: a rock parked on a ring would make this
        // test about collision.
        s.world.asteroids.clear();
        s.run(idle(), s.world.rules.trials.countdown + 0.1);
        assert!(!s.trials().running, "the clock started before ring 0");

        // -- crossing one: the start line -----------------------------------
        let events = cross_next_ring(&mut s);
        let opened_at = s.world.time;
        assert!(events.iter().any(|e| matches!(
            e,
            SimEvent::CheckpointPassed {
                index: 0,
                lap_time: None
            }
        )));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SimEvent::LapComplete { .. })),
            "the start line reported a lap time"
        );
        assert!(s.trials().running);
        assert_eq!(s.trials().lap, 1);
        assert_eq!(s.trials().next_cp, 1);
        assert!(s.trials().last_lap.is_none());

        // -- the rest of the circuit ----------------------------------------
        let rings = s.trials().checkpoints.len();
        for i in 1..rings {
            let events = cross_next_ring(&mut s);
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, SimEvent::CheckpointPassed { index, .. } if *index == i)),
                "no CheckpointPassed for ring {i}"
            );
        }
        assert_eq!(s.trials().next_cp, 0, "the circuit did not wrap");

        // -- crossing two: the lap ------------------------------------------
        let events = cross_next_ring(&mut s);
        let closed_at = s.world.time;
        let lap = s.trials().last_lap.expect("no lap time");

        let measured = closed_at - opened_at;
        assert!(
            (lap - measured).abs() < TICK_DT * 1.5,
            "lap time {lap} is not the {measured} between the two crossings"
        );
        assert!(
            lap < closed_at,
            "the lap was charged for the countdown and the run to the grid"
        );
        assert_eq!(s.trials().lap, 2);
        assert_eq!(s.trials().best_lap, Some(lap), "the first lap is the best");
        assert!(events.iter().any(
            |e| matches!(e, SimEvent::CheckpointPassed { index: 0, lap_time: Some(t) } if *t == lap)
        ));
        assert!(events
            .iter()
            .any(|e| matches!(e, SimEvent::LapComplete { time, is_best: true } if *time == lap)));

        // And the HUD carries it, which is what `hud.rs` reads.
        assert_eq!(s.frame.hud.trials.lap, 2);
        assert!((f64::from(s.frame.hud.trials.last_lap) - lap).abs() < 1e-3);
        assert!(s.frame.hud.trials.running);
    }

    /// A slower second lap must not overwrite the best.
    #[test]
    fn only_a_faster_lap_becomes_the_best() {
        let mut s = Solo::new(MatchSetup::trial(1));
        s.world.trials.as_mut().unwrap().countdown_active = false;
        s.world.trials.as_mut().unwrap().cp_cooldown = 0.0;
        {
            let t = s.world.trials.as_mut().unwrap();
            t.running = true;
            t.lap = 1;
            t.timer = 12.0;
            t.best_lap = Some(9.0);
        }
        let events = cross_next_ring(&mut s);
        assert_eq!(s.trials().best_lap, Some(9.0), "a slow lap took the record");
        assert_eq!(s.trials().last_lap, Some(12.0));
        assert_eq!(s.trials().lap, 2);
        assert!(events
            .iter()
            .any(|e| matches!(e, SimEvent::LapComplete { is_best: false, .. })));
    }

    /// Crosses the next ring on the very next tick, by forgiving the dead time
    /// rather than flying it out. For the tests that are about what a pass
    /// *does*, not about when one is allowed.
    fn cross_ring_now(s: &mut Solo, boost: f64) {
        let target = s.trials().checkpoints[s.trials().next_cp];
        s.world.trials.as_mut().unwrap().cp_cooldown = 0.0;
        {
            let me = s.world.ship_mut(LOCAL_ID).unwrap();
            me.pos = target;
            me.vel = SimVec3::ZERO;
            me.boost_meter = boost;
            // Under the recharge delay, so the only thing that can move the
            // meter this tick is the ring.
            me.boost_idle = 0.0;
        }
        let before = s.trials().next_cp;
        s.step(idle());
        assert_ne!(s.trials().next_cp, before, "the ring did not trigger");
    }

    /// Every ring refunds boost, which is the whole reason a trial is flyable
    /// at speed. `main.js:1778`.
    #[test]
    fn a_ring_refunds_boost() {
        let mut s = Solo::new(MatchSetup::trial(1));
        let rules = s.world.rules;
        s.run(idle(), rules.trials.countdown + 0.1);

        cross_ring_now(&mut s, 1.0);
        let me = s.me();
        assert!(
            (me.boost_meter - (1.0 + rules.trials.cp_boost_award)).abs() < 1e-9,
            "boost went 1.0 -> {}",
            me.boost_meter
        );
        assert_eq!(
            me.boost_idle, 0.0,
            "the refund waits out the recharge delay"
        );
        assert!(
            (f64::from(s.frame.hud.trials.next_cp) - 1.0).abs() < 1e-9,
            "the HUD still points at the ring just passed"
        );
        assert!(
            (f64::from(s.frame.hud.boost01) - me.boost_meter / rules.ship.max_boost).abs() < 1e-6,
            "the boost bar is a tick behind the refund"
        );

        // And it is clamped, not stacked.
        cross_ring_now(&mut s, rules.ship.max_boost);
        assert_eq!(s.me().boost_meter, rules.ship.max_boost);
    }

    /// The trigger is a 55-unit sphere, not the ring's own 48-unit radius, and
    /// a ring 56 units away must not fire.
    #[test]
    fn the_ring_trigger_is_fifty_five_units() {
        let mut s = Solo::new(MatchSetup::trial(1));
        let rules = s.world.rules;
        assert_eq!(rules.trials.cp_trigger_dist, 55.0);
        s.run(idle(), rules.trials.countdown + 0.1);

        let ring = s.trials().checkpoints[0];
        for (offset, expect) in [(56.0, false), (54.0, true)] {
            s.world.trials.as_mut().unwrap().cp_cooldown = 0.0;
            {
                let me = s.world.ship_mut(LOCAL_ID).unwrap();
                me.pos = ring + SimVec3::new(0.0, offset, 0.0);
                me.vel = SimVec3::ZERO;
            }
            s.step(idle());
            assert_eq!(
                s.trials().running,
                expect,
                "a ring {offset} units away should{} have fired",
                if expect { "" } else { " not" }
            );
        }
    }

    /// A crash rewinds the run to the grid. `sim`'s respawn has no answer for a
    /// trial and would leave the ship on the team anchor, 30 units under the
    /// grid, still hunting whichever ring it was on.
    #[test]
    fn a_crash_puts_the_run_back_on_the_grid() {
        let mut s = Solo::new(MatchSetup::trial(1));
        let rules = s.world.rules;
        s.run(idle(), rules.trials.countdown + 0.1);
        cross_next_ring(&mut s);
        cross_next_ring(&mut s);
        assert_eq!(s.trials().next_cp, 2);
        assert!(s.trials().running);

        {
            let me = s.world.ship_mut(LOCAL_ID).unwrap();
            me.invuln_timer = 0.0;
            me.pos = MOON_INTERIOR;
        }
        s.run(idle(), 0.1);
        assert!(!s.me().alive);
        s.run(idle(), rules.combat.respawn_delay + 0.2);

        let me = s.me();
        assert!(me.alive);
        assert_eq!(me.pos, rules.spawn.trials_start, "not back on the grid");
        assert_eq!(me.quat, SimQuat::IDENTITY);
        assert_eq!(s.trials().next_cp, 0, "the run kept its old ring");
        assert!(!s.trials().running);
        assert_eq!(s.trials().timer, 0.0);
        assert!(
            s.trials().cp_cooldown > rules.trials.cp_cooldown,
            "a reset arms ring 0 as fast as an ordinary pass"
        );

        // The frame has to agree, or `scene.rs` snaps to the wrong pose and
        // then streaks the ship to the grid over the next tick.
        let view = s
            .frame
            .ships
            .iter()
            .find(|v| v.id == LOCAL_ID)
            .expect("local ship view");
        assert_eq!(view.pos, vec3f(rules.spawn.trials_start));
    }

    /// The lap clock must not run while the pilot is dead, and the HUD must not
    /// show a trial outside one.
    #[test]
    fn the_clock_and_the_readout_are_scoped_to_a_trial() {
        let mut skirmish = Solo::new(setup(Mode::Skirmish));
        skirmish.step(idle());
        assert!(!skirmish.frame.hud.trials.active);
        assert!(skirmish.world.trials.is_none());

        let mut s = Solo::new(MatchSetup::trial(2));
        s.run(idle(), s.world.rules.trials.countdown + 0.1);
        cross_next_ring(&mut s);
        s.world.ship_mut(LOCAL_ID).unwrap().alive = false;
        let held = s.trials().timer;
        s.run(idle(), 0.5);
        assert_eq!(
            s.trials().timer,
            held,
            "the clock ran while the pilot was dead"
        );
    }

    // -- rebuilding --------------------------------------------------------

    /// The message a menu writes. Building a second match must replace the
    /// world outright, not merge into it.
    #[test]
    fn a_start_match_request_rebuilds_the_world() {
        let mut app = App::new();
        app.insert_resource(SimWorld(new_match(&setup(Mode::Skirmish)).0))
            .insert_resource(Roster::default())
            .insert_resource(setup(Mode::Skirmish))
            .init_resource::<SimFrame>()
            .init_resource::<EdgeLatch>()
            .add_message::<StartMatch>()
            .add_systems(Update, apply_start_match);

        app.world_mut()
            .write_message(StartMatch(MatchSetup::campaign(3)));
        app.update();

        let world = app.world().resource::<SimWorld>();
        assert_eq!(world.0.mode, Mode::Campaign(3));
        assert_eq!(world.0.campaign.as_ref().unwrap().mission, 3);
        assert_eq!(world.0.campaign.as_ref().unwrap().bots_alive, 5);
        assert_eq!(world.0.asteroids.len(), 280);
        assert_eq!(
            app.world().resource::<MatchSetup>().mode,
            Mode::Campaign(3),
            "the resource still names the old match"
        );
        assert_eq!(app.world().resource::<Roster>().callsign(LOCAL_ID), "PILOT");
    }
}
