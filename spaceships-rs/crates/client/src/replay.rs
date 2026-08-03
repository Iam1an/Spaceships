//! Recording every match, and flying through a recorded one.
//!
//! # Two halves that barely touch
//!
//! **Recording** is a dashcam. It is on for every match, it costs one `Vec`
//! push a tick, and it writes a file when the match ends or the app does. The
//! whole of it is [`Tape`], and the only thing outside this module that knows
//! it exists is the four lines in `sim_bridge::fixed_tick` that hand it the
//! slices the tick is about to be given.
//!
//! **Playback** replaces the pilot. `SPACESHIPS_REPLAY=<file>` loads a
//! recording, puts its opening world into [`crate::sim_bridge::SimWorld`], and
//! from then on [`Theatre`] decides which tick the world is on — play, pause,
//! step, scrub. The renderer is not told: `scene.rs`, `weapons.rs`, `hud.rs`
//! and `audio.rs` read `SimFrame` exactly as they do in a live match, because
//! the frame a replayed tick produces *is* the frame the live tick produced.
//!
//! # The camera is not simulation state
//!
//! Which is the whole reason this is affordable. Nothing here touches `sim`;
//! [`FreeCam`] and [`ViewTarget`] are rendering, and the two existing cameras —
//! `camera.rs`'s chase and `cockpit.rs`'s seat — already do everything a replay
//! needs except point at a ship that is not the local player. So this module
//! adds one resource saying *which* ship, and those two modules read it.
//!
//! # Controls
//!
//! | Key | |
//! |---|---|
//! | `Space` | play / pause |
//! | `,` `.` | step one tick back / forward |
//! | `←` `→` | scrub 5 s (with `Shift`, 30 s) |
//! | `Home` | back to the start |
//! | `-` `=` | slower / faster: 0.1x to 4x |
//! | `Tab` | ride the next ship |
//! | `V` | riding: outside / inside — `cockpit.rs`'s own binding |
//! | `G` | let go of the ship and fly free |
//! | `W` `A` `S` `D` | free camera: forward, strafe |
//! | `R` `F` | free camera: up, down |
//! | `Shift` `Alt` | free camera: five times faster, five times slower |
//! | mouse | free camera: look (click first — the pointer has to be locked) |
//!
//! # Seeking a trial or a campaign is approximate, and why
//!
//! Playing forward is exact in every mode, because this module's step calls
//! `sim_bridge::step_modes` exactly where [`crate::sim_bridge::fixed_tick`]
//! does. **Seeking is not**, for trials and the campaign only: a keyframe is
//! built inside `spaceships-replay`, which calls `sim::tick::tick` and knows
//! nothing about `step_modes` — so a scrub past a checkpoint loses that ring's
//! boost award, and a scrub past a campaign wave loses the missile-timer
//! fix-up.
//!
//! It is deliberately not patched here. `step_modes` exists only because two
//! jobs that are simulation state — the trials checkpoint scoring and the
//! campaign wave arming — are still outside `sim`, and `sim_bridge` documents
//! both as bugs with the one-line fixes named. Threading a callback through the
//! keyframe builder would make the workaround load-bearing in a second crate
//! and make moving it *harder*. Solo deathmatch, skirmish, training and
//! multiplayer — everything with no `step_modes` work to do — seek exactly.
//!
//! # What phase one deliberately does not do
//!
//! No timeline widget, no camera keyframes, no export. The overlay is two lines
//! of text, because a scrub bar drawn now would be drawn again the moment the
//! nav bar is designed properly — see `BACKLOG.md` §1, which is explicit that
//! the timeline is the primary control surface and worth designing rather than
//! bolting onto the HUD.

use bevy::prelude::*;
use sim::world::{EntityId, Input as SimInput, NetEvent, World as SimWorldState, TICK_HZ};
use spaceships_replay::{Recorder, Timeline};
use spaceships_sim as sim;

use crate::sim_bridge::{MatchSetup, SimFrame, SimWorld, LOCAL_ID};

// The three items below are the two environment variables and the loader that
// reads them, and every one of them is native-only. The browser has no
// filesystem: there is nothing to record onto and no file to name, and
// `std::env::var` always fails there anyway, so a web build carries the
// transport and the cameras and never constructs a `Theatre`.

/// The `SPACESHIPS_REPLAY` value that means "play this file".
#[cfg(not(target_arch = "wasm32"))]
const REPLAY_ENV: &str = "SPACESHIPS_REPLAY";

/// `SPACESHIPS_RECORD=0` turns the dashcam off.
#[cfg(not(target_arch = "wasm32"))]
const RECORD_ENV: &str = "SPACESHIPS_RECORD";

/// Playback rates, in order. `1.0` is the resting one.
///
/// Slow motion is not a physics change: the simulation still advances exactly
/// [`sim::world::TICK_DT`] per tick, and the rate only changes how often a tick
/// happens in wall-clock time. `scene.rs`'s interpolation then fills in between
/// them, which is what makes 0.1x smooth rather than a slideshow.
const RATES: [f32; 7] = [0.1, 0.25, 0.5, 1.0, 2.0, 3.0, 4.0];

/// Index of `1.0` in [`RATES`], and where playback starts.
#[cfg(not(target_arch = "wasm32"))]
const NORMAL_RATE: usize = 3;

/// How far `←`/`→` scrub, in seconds.
const SCRUB_SECONDS: f64 = 5.0;

/// How far they scrub with `Shift` held.
const SCRUB_SECONDS_FAST: f64 = 30.0;

/// Free-camera speed, in world units per second, before the modifiers.
const FREE_SPEED: f32 = 120.0;

/// Free-camera look sensitivity, radians per pixel of mouse travel.
const FREE_LOOK: f32 = 0.0022;

/// How close to straight up or down the free camera may point.
///
/// A hair under a right angle, because `Transform::look_to` needs the view and
/// the up vector to be independent and exactly vertical is where they stop
/// being.
const FREE_PITCH_LIMIT: f32 = 1.552;

pub struct ReplayPlugin;

impl Plugin for ReplayPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ViewTarget>()
            .init_resource::<FreeCam>()
            .init_resource::<Tape>()
            .add_systems(Startup, spawn_overlay)
            .add_systems(
                Update,
                (
                    // Transport before the camera: a scrub that moves the world
                    // should be reflected by the same frame's view, not the
                    // next one.
                    drive_transport,
                    choose_view,
                    update_overlay,
                )
                    .chain()
                    .run_if(replaying),
            )
            // `PostUpdate`, and after propagation, for the reason
            // `cockpit.rs::seat_camera` is: `camera.rs::follow` writes the
            // camera in `PostUpdate` and this has to be the last word. Both are
            // gated so they never both write, but the ordering keeps that a
            // property of the schedule rather than of a run condition.
            .add_systems(
                PostUpdate,
                fly_free_camera
                    .after(TransformSystems::Propagate)
                    .run_if(camera_is_free),
            )
            // Before `SimSet`, so a world swapped in this frame is on a fresh
            // tape before its first tick is recorded onto the last match's.
            // Skipped while replaying: a scrub moves the tick counter
            // backwards, which is exactly the signal `arm_tape` reads as a new
            // match, and there is nothing to record anyway.
            .add_systems(
                FixedUpdate,
                arm_tape
                    .before(crate::sim_bridge::SimSet)
                    .run_if(not(replaying)),
            )
            // Written from `Last` so the flush sees the frame that asked for
            // it, and on exit so a match nobody finished is still kept.
            .add_systems(Last, (flush_on_match_end, flush_on_exit));

        #[cfg(not(target_arch = "wasm32"))]
        if let Some(path) = replay_path() {
            match load(&path) {
                Ok(theatre) => install(app, theatre),
                Err(e) => error!("replay: cannot play {}: {e}", path.display()),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Which ship the cameras look at
// ---------------------------------------------------------------------------

/// The ship `camera.rs` chases and `cockpit.rs` seats the viewer in.
///
/// # Why this exists
///
/// Nine modules ask the simulation for [`LOCAL_ID`] rather than for
/// `World::local_id`, and `net.rs`'s `IdSwap` documents at length what that
/// assumption cost the day it stopped being true online — a camera parked on a
/// spawn point, watching somebody else fly. It is the right assumption for a
/// pilot in a seat and the wrong one for a replay, where the whole point is to
/// watch somebody else fly on purpose.
///
/// Rather than teach nine modules to read a target, this names the one thing
/// that actually varies. **Two** modules read it — `camera.rs`, for where the
/// chase camera sits, and `cockpit.rs`, for whose canopy is drawn and whose
/// hull is hidden. The rest keep [`LOCAL_ID`], which is correct for them: the
/// flat HUD is the recorded pilot's HUD, and it stands down in free flight
/// anyway.
///
/// It defaults to [`LOCAL_ID`], so a live match behaves exactly as before.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewTarget(pub EntityId);

impl Default for ViewTarget {
    fn default() -> ViewTarget {
        ViewTarget(LOCAL_ID)
    }
}

/// The detached camera: six degrees of freedom, attached to nothing.
///
/// `active` is false in a live match and this module is the only thing that
/// ever sets it, so the flight cameras keep the field to themselves unless a
/// replay is running.
#[derive(Resource, Debug, Default)]
pub struct FreeCam {
    /// Whether the camera is detached.
    pub active: bool,
    /// Where it is. Kept here rather than read back off the `Transform` so that
    /// `camera.rs` handing the camera over does not teleport it.
    at: Vec3,
    yaw: f32,
    pitch: f32,
    /// Whether `at`/`yaw`/`pitch` have been seeded, so a second frame of free
    /// flight continues from the first rather than re-framing every time.
    seeded: bool,
}

/// Where the free camera sits relative to the ship it is framed on, the first
/// time it takes over: behind, above, and looking at it.
///
/// The same offsets `camera.rs` chases with, so letting go of an aircraft
/// leaves the picture almost where it was and then stops moving. A camera
/// seeded from its own `Transform` instead would be correct in that case and
/// badly wrong in the other one: at startup nothing has driven it, so it holds
/// the pose `spawn_camera` gave it, pointing away from the map.
const FRAME_BEHIND: f32 = 14.0;
const FRAME_ABOVE: f32 = 6.0;

/// Run condition: the free camera has the wheel.
pub fn camera_is_free(free: Res<FreeCam>) -> bool {
    free.active
}

/// Run condition: the flight cameras have it.
pub fn camera_is_attached(free: Res<FreeCam>) -> bool {
    !free.active
}

// ---------------------------------------------------------------------------
// Recording
// ---------------------------------------------------------------------------

/// Which match a tape is a recording of.
///
/// Enough to notice that the world underneath has been swapped for a different
/// one — a lobby launch, a networked handover — without every module that can
/// swap it having to say so. The tick is not in here; it is compared
/// separately, because a tick that goes *backwards* is the other way a world
/// gets replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MatchId {
    seed: u64,
    mode: sim::world::Mode,
    map: sim::world::MapKind,
}

impl MatchId {
    fn of(world: &SimWorldState) -> MatchId {
        MatchId {
            seed: world.rng.seed,
            mode: world.mode,
            map: world.map,
        }
    }
}

/// The dashcam.
///
/// Always on, in every mode. That is the point: a replay you had to decide to
/// record before the thing worth watching happened is a recording of the times
/// nothing happened.
#[derive(Resource, Default)]
pub struct Tape {
    recorder: Option<Recorder>,
    /// Whether what is in `recorder` has already been written out, so a match
    /// that ends and then exits does not produce the same file twice.
    written: bool,
    /// Which match is on the tape.
    covering: Option<MatchId>,
    /// The last tick seen, so a world that has gone back to zero is recognised
    /// as a new match rather than recorded onto the end of the old one.
    last_tick: u64,
}

impl Tape {
    /// Begins recording `world`, discarding whatever came before.
    fn start(&mut self, world: &SimWorldState, label: String) {
        self.covering = Some(MatchId::of(world));
        self.last_tick = world.tick;
        if !recording_enabled() {
            self.recorder = None;
            return;
        }
        let mut recorder = Recorder::start(world, label);
        recorder.stamp(unix_now());
        self.recorder = Some(recorder);
        self.written = false;
    }

    /// Records one tick's arguments, before the tick runs.
    pub fn push(&mut self, inputs: &[SimInput], events: &[NetEvent]) {
        if let Some(rec) = self.recorder.as_mut() {
            rec.push(inputs, events);
        }
    }

    /// Whether this tape is a recording of the world in front of it.
    fn covers(&self, world: &SimWorldState) -> bool {
        self.covering == Some(MatchId::of(world)) && world.tick >= self.last_tick
    }
}

/// Starts a fresh tape whenever the world underneath is a different match.
///
/// # Why this watches rather than being told
///
/// Three places replace [`SimWorld`]: `SimPlugin` at startup, `apply_start_match`
/// when the lobby launches something, and `net.rs`'s handover when a networked
/// match begins. Notifying this module from all three means three edits, three
/// chances to forget, and a fourth caller one day that records nothing at all
/// and says nothing about it. Comparing the seed, the mode, the map and the
/// direction the tick counter is going costs four reads a tick and cannot be
/// forgotten.
fn arm_tape(world: Res<SimWorld>, setup: Res<MatchSetup>, mut tape: ResMut<Tape>) {
    if tape.covers(&world.0) {
        tape.last_tick = world.0.tick;
        return;
    }
    // Whatever was on the tape was a whole match, however it ended.
    write_tape(&mut tape);
    tape.start(&world.0, label_for(setup.as_ref()));
}

/// Whether the dashcam runs at all.
fn recording_enabled() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        // No filesystem to write to, so there is nothing to hold the tape for.
        false
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        !matches!(
            std::env::var(RECORD_ENV).as_deref(),
            Ok("0") | Ok("off") | Ok("false")
        )
    }
}

/// Seconds since the Unix epoch, or `0` where there is no clock to ask.
///
/// The one wall-clock read in the whole feature, and it is here rather than in
/// `spaceships-replay` for the reason that crate's `Recorder::stamp` gives:
/// nothing downstream of `sim` may grow a clock, and a recording's timestamp is
/// a file-browser convenience that never enters a simulation.
fn unix_now() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }
}

/// A label a file browser can read: the mode and the map.
pub fn label_for(setup: &MatchSetup) -> String {
    format!("{:?} on {:?}", setup.mode, setup.map)
}

/// Writes the tape when a match ends, which is the moment there is something
/// worth keeping.
fn flush_on_match_end(frame: Res<SimFrame>, mut tape: ResMut<Tape>) {
    let ended = frame
        .0
        .events
        .iter()
        .any(|e| matches!(e, sim::world::SimEvent::MatchEnded { .. }));
    if ended {
        write_tape(&mut tape);
    }
}

/// And on the way out, so a match nobody finished is still kept.
fn flush_on_exit(mut exit: MessageReader<AppExit>, mut tape: ResMut<Tape>) {
    if exit.read().next().is_some() {
        write_tape(&mut tape);
    }
}

/// The browser has no filesystem, so the tape has nowhere to go and is never
/// started in the first place — see [`recording_enabled`].
#[cfg(target_arch = "wasm32")]
fn write_tape(_tape: &mut Tape) {}

/// Puts the tape on disk under `<state dir>/replays/`.
///
/// Best effort and loud about failing: a recording that cannot be written is
/// worth a log line and is emphatically not worth interrupting a match over.
#[cfg(not(target_arch = "wasm32"))]
fn write_tape(tape: &mut Tape) {
    if tape.written {
        return;
    }
    let Some(recorder) = tape.recorder.as_ref() else {
        return;
    };
    if recorder.is_empty() {
        return;
    }
    tape.written = true;

    let recording = recorder.recording();
    let Some(dir) = crate::api::state_dir().map(|d| d.join("replays")) else {
        warn!("replay: no state directory, so nothing was saved");
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        warn!("replay: cannot create {}: {e}", dir.display());
        return;
    }

    let name = format!(
        "{}-{}.{}",
        recording.recorded_at,
        slug(&recording.label),
        spaceships_replay::EXTENSION,
    );
    let path = dir.join(name);
    let bytes = recording.encode();
    match std::fs::write(&path, &bytes) {
        Ok(()) => info!(
            "replay: saved {} ticks ({:.1} s, {} kB) to {}",
            recording.len(),
            recording.duration(),
            bytes.len() / 1024,
            path.display(),
        ),
        Err(e) => warn!("replay: cannot write {}: {e}", path.display()),
    }
}

/// A label, as something safe to put in a file name.
#[cfg(not(target_arch = "wasm32"))]
fn slug(label: &str) -> String {
    let mut out: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').to_owned()
}

// ---------------------------------------------------------------------------
// Playback
// ---------------------------------------------------------------------------

/// A loaded recording, and where in it the viewer is.
///
/// The world it drives is [`SimWorld`] — the same resource a live match uses,
/// so nothing downstream can tell the difference. See
/// [`spaceships_replay::Timeline`] for why the timeline does not own a world of
/// its own.
#[derive(Resource)]
pub struct Theatre {
    timeline: Timeline,
    /// Whether the clock is running.
    playing: bool,
    /// Index into [`RATES`].
    rate: usize,
    /// Set by a seek, cleared by the overlay: what the last scrub cost, which
    /// is the only honest measure of whether the keyframes are doing their job.
    last_seek: Option<spaceships_replay::SeekCost>,
}

impl Theatre {
    /// Whether a tick should run this fixed step.
    #[must_use]
    pub fn running(&self) -> bool {
        self.playing && !self.timeline.at_end()
    }

    /// Applies the next recorded step to `world`.
    pub fn step(&mut self, world: &mut SimWorldState) -> Option<sim::world::Frame> {
        self.timeline.step(world)
    }

    /// The playback rate, as a multiple of real time.
    #[must_use]
    pub fn rate(&self) -> f32 {
        RATES[self.rate]
    }
}

/// Run condition: a recording is loaded.
pub fn replaying(theatre: Option<Res<Theatre>>) -> bool {
    theatre.is_some()
}

/// `SPACESHIPS_REPLAY=<path>`.
#[cfg(not(target_arch = "wasm32"))]
fn replay_path() -> Option<std::path::PathBuf> {
    let raw = std::env::var_os(REPLAY_ENV)?;
    if raw.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(raw))
}

/// Reads a recording and builds its seek index.
#[cfg(not(target_arch = "wasm32"))]
fn load(path: &std::path::Path) -> Result<Theatre, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let recording = spaceships_replay::Recording::decode(&bytes).map_err(|e| e.to_string())?;

    let ticks = recording.len();
    let label = recording.label.clone();
    let at = bevy::platform::time::Instant::now();
    let timeline = Timeline::build(recording, 0);
    info!(
        "replay: {label} — {ticks} ticks ({:.1} s), {} keyframes indexed in {:.0} ms",
        timeline.duration(),
        timeline.keyframe_count(),
        at.elapsed().as_secs_f64() * 1000.0,
    );

    Ok(Theatre {
        timeline,
        playing: true,
        rate: NORMAL_RATE,
        last_seek: None,
    })
}

/// Where a replay opens: which camera, and on whose aircraft.
///
/// `SPACESHIPS_REPLAY_VIEW=free` (the default), `=chase`, `=seat`, each with an
/// optional `:<entity id>` — `seat:12` sits in ship 12's cockpit. Exactly the
/// job `SPACESHIPS_COCKPIT` does for a live match, and for exactly the reason
/// that flag documents: a visual check needs to be able to capture a view
/// without anyone being there to press the key.
#[cfg(not(target_arch = "wasm32"))]
fn opening_view() -> (bool, bool, Option<EntityId>) {
    let Ok(spec) = std::env::var("SPACESHIPS_REPLAY_VIEW") else {
        // `BACKLOG.md` §1: "fly it like a drone. This is the default view, not
        // a mode you opt into."
        return (true, false, None);
    };
    let (name, id) = match spec.split_once(':') {
        Some((name, id)) => (name, id.trim().parse::<EntityId>().ok()),
        None => (spec.as_str(), None),
    };
    match name.trim().to_ascii_lowercase().as_str() {
        "chase" | "outside" | "third" => (false, false, id),
        "seat" | "cockpit" | "inside" | "first" => (false, true, id),
        "free" | "drone" => (true, false, id),
        other => {
            warn!("SPACESHIPS_REPLAY_VIEW={other} is not free, chase or seat");
            (true, false, id)
        }
    }
}

/// A floating-point environment variable, or `None` if absent or unparseable.
#[cfg(not(target_arch = "wasm32"))]
fn env_f64(key: &str) -> Option<f64> {
    std::env::var(key).ok()?.trim().parse::<f64>().ok()
}

/// Swaps the recorded match in for whatever `sim_bridge` built at startup.
///
/// The map has to move as well as the world: `terrain.rs` installs or tears the
/// Sierras down when [`MatchSetup::map`] changes, so a recording made on the
/// terrain map would otherwise be flown over an empty starfield with an
/// invisible kill plane in it.
#[cfg(not(target_arch = "wasm32"))]
fn install(app: &mut App, mut theatre: Theatre) {
    let mut world = theatre.timeline.start_world();

    // `SPACESHIPS_REPLAY_AT=<seconds>` opens on a moment and holds it. Same
    // family as `SPACESHIPS_SCREENSHOT` and `SPACESHIPS_COCKPIT`: a visual check
    // of a particular instant should not need somebody sitting there scrubbing
    // to it, and a *paused* replay is the only kind whose screenshot is the same
    // picture twice.
    let mut open_at = 0;
    if let Some(seconds) = env_f64("SPACESHIPS_REPLAY_AT") {
        theatre.playing = false;
        let dt = theatre.timeline.recording().dt.max(f64::MIN_POSITIVE);
        open_at = (seconds / dt) as usize;
    }

    // **One step, always.** The renderer draws `SimFrame` and nothing else, and
    // a frame only exists because a tick produced one — so a replay that opened
    // paused would come up on an empty starfield with no ships, no rocks and no
    // HUD until somebody pressed play. Seeking to just *before* the opening tick
    // and taking a single step leaves the world exactly where it was asked for
    // and leaves a frame of it behind.
    let cost = theatre
        .timeline
        .seek(&mut world, open_at.max(1).saturating_sub(1));
    let frame = theatre.timeline.step(&mut world).unwrap_or_default();
    if open_at > 0 {
        info!(
            "replay: opened paused at tick {}, {} ticks re-simulated",
            theatre.timeline.cursor(),
            cost.ticks,
        );
    }

    let setup = MatchSetup {
        mode: world.mode,
        map: world.map,
        seed: world.rng.seed,
        hard_mode: false,
        callsign: "REPLAY".to_owned(),
    };
    let (free, seated, ride) = opening_view();

    app.insert_resource(SimWorld(world))
        .insert_resource(setup)
        .insert_resource(SimFrame(frame))
        // Nothing is being recorded during a replay. Recording the replay would
        // produce a byte-identical copy of the file already on disk, one match
        // later.
        .insert_resource(Tape::default())
        .insert_resource(FreeCam {
            active: free,
            ..default()
        })
        .insert_resource(crate::cockpit::ViewMode {
            first_person: seated,
            seated: false,
        })
        .insert_resource(theatre);
    if let Some(id) = ride {
        app.insert_resource(ViewTarget(id));
    }
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// Play, pause, step, scrub, and the rate.
fn drive_transport(
    keys: Res<ButtonInput<KeyCode>>,
    mut theatre: ResMut<Theatre>,
    mut world: ResMut<SimWorld>,
    mut fixed: ResMut<Time<Fixed>>,
) {
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let dt = theatre.timeline.recording().dt.max(f64::MIN_POSITIVE);
    let scrub = ((if shift {
        SCRUB_SECONDS_FAST
    } else {
        SCRUB_SECONDS
    }) / dt) as usize;

    if keys.just_pressed(KeyCode::Space) {
        theatre.playing = !theatre.playing;
    }

    // A frame-step implies a pause: nobody steps a tick at a time while the
    // thing is running.
    let mut target = None;
    if keys.just_pressed(KeyCode::Comma) {
        theatre.playing = false;
        target = Some(theatre.timeline.cursor().saturating_sub(1));
    }
    if keys.just_pressed(KeyCode::Period) {
        theatre.playing = false;
        target = Some(theatre.timeline.cursor() + 1);
    }
    if keys.just_pressed(KeyCode::ArrowLeft) {
        target = Some(theatre.timeline.cursor().saturating_sub(scrub));
    }
    if keys.just_pressed(KeyCode::ArrowRight) {
        target = Some(theatre.timeline.cursor() + scrub);
    }
    if keys.just_pressed(KeyCode::Home) {
        target = Some(0);
    }

    if let Some(target) = target {
        let cost = theatre.timeline.seek(&mut world.0, target);
        theatre.last_seek = Some(cost);
    }

    // The rate is the fixed timestep, not a multiplier inside the tick. The
    // simulation always advances one `TICK_DT` per step; running the steps
    // further apart in wall-clock time is what slow motion *is*, and
    // `scene.rs`'s interpolation smooths between them for nothing.
    let mut rate = theatre.rate;
    if keys.just_pressed(KeyCode::Minus) {
        rate = rate.saturating_sub(1);
    }
    if keys.just_pressed(KeyCode::Equal) {
        rate = (rate + 1).min(RATES.len() - 1);
    }
    if rate != theatre.rate {
        theatre.rate = rate;
        fixed.set_timestep_hz(f64::from(TICK_HZ as f32 * RATES[rate]));
    }
}

/// `Tab` rides the next ship, `G` lets go.
fn choose_view(
    keys: Res<ButtonInput<KeyCode>>,
    frame: Res<SimFrame>,
    mut target: ResMut<ViewTarget>,
    mut free: ResMut<FreeCam>,
    mut cockpit: ResMut<crate::cockpit::ViewMode>,
) {
    if keys.just_pressed(KeyCode::KeyG) {
        free.active = true;
        // A canopy drawn around a camera that has left the ship is a box in the
        // middle of the map.
        cockpit.first_person = false;
        return;
    }
    if !keys.just_pressed(KeyCode::Tab) {
        return;
    }

    // Ships the viewer can be inside: everything with a hull, which is
    // everything except the boss's twenty invisible hitboxes.
    let flyable: Vec<EntityId> = frame
        .0
        .ships
        .iter()
        .filter(|s| !s.flags.contains(sim::world::ShipFlags::BOSS_HITBOX))
        .map(|s| s.id)
        .collect();
    let Some(&first) = flyable.first() else {
        return;
    };

    // Coming out of free flight lands on whoever is already selected rather
    // than advancing past them, so `Tab` from the drone view is "get in this
    // one" and `Tab` again is "the next one".
    if free.active {
        free.active = false;
        if !flyable.contains(&target.0) {
            target.0 = first;
        }
        return;
    }
    let next = flyable
        .iter()
        .position(|id| *id == target.0)
        .map_or(0, |i| (i + 1) % flyable.len());
    target.0 = flyable[next];
}

// ---------------------------------------------------------------------------
// The free camera
// ---------------------------------------------------------------------------

/// The camera's own pose, both halves, the same shape `cockpit.rs` uses and for
/// the same reason — transform propagation has already run by the time either of
/// these systems does, so the derived transform has to be written by hand.
type CameraPose<'w, 's> = Query<
    'w,
    's,
    (&'static mut Transform, &'static mut GlobalTransform),
    (
        With<crate::camera::FlightCamera>,
        Without<crate::scene::ShipRoot>,
    ),
>;

/// Six degrees of freedom, attached to nothing.
///
/// Writes `GlobalTransform` as well as `Transform`, for the reason
/// `cockpit.rs::seat_camera` does: this runs after propagation, so the derived
/// transform would otherwise be a frame stale — and a stale camera matrix is
/// what culls the geometry in front of you.
fn fly_free_camera(
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<bevy::input::mouse::AccumulatedMouseMotion>,
    time: Res<Time>,
    target: Res<ViewTarget>,
    ships: Query<(&crate::scene::ShipRoot, &GlobalTransform)>,
    mut free: ResMut<FreeCam>,
    mut cam: CameraPose,
) {
    let Ok((mut tf, mut global)) = cam.single_mut() else {
        return;
    };

    // The first frame of free flight frames the ship the viewer was on — see
    // [`FRAME_BEHIND`].
    //
    // **And waits for one.** `scene.rs` spawns the ship entities from the first
    // `Frame`, which is a fixed step away, so on the frame a replay opens there
    // is nothing in this query at all. Seeding from the camera's own transform
    // then would take the pose `spawn_camera` gave it — parked on team zero's
    // anchor, pointing away from the map and straight into a mothership — and
    // `seeded` would latch it there for the rest of the session. That is
    // exactly what the first capture of this showed.
    if !free.seeded {
        let framed = ships
            .iter()
            .find(|(root, _)| root.0 == target.0)
            .or_else(|| ships.iter().next());
        let Some((_, ship)) = framed else {
            return;
        };
        let (_, rotation, at_ship) = ship.to_scale_rotation_translation();
        let at = at_ship + rotation * Vec3::NEG_Z * FRAME_BEHIND + rotation * Vec3::Y * FRAME_ABOVE;
        let (yaw, pitch, _) = Transform::from_translation(at)
            .looking_at(at_ship, Vec3::Y)
            .rotation
            .to_euler(EulerRot::YXZ);
        free.at = at;
        free.yaw = yaw;
        free.pitch = pitch;
        free.seeded = true;
    }

    // Looking is unconditional on the desktop, where the pointer is locked and
    // there is nothing else for it to do. Right-drag also works, for anyone who
    // has released the lock to reach a menu.
    let dragging = buttons.pressed(MouseButton::Right);
    let delta = if dragging || motion.delta != Vec2::ZERO {
        motion.delta
    } else {
        Vec2::ZERO
    };
    free.yaw -= delta.x * FREE_LOOK;
    free.pitch = (free.pitch - delta.y * FREE_LOOK).clamp(-FREE_PITCH_LIMIT, FREE_PITCH_LIMIT);

    let rotation = Quat::from_euler(EulerRot::YXZ, free.yaw, free.pitch, 0.0);

    let axis = |neg: KeyCode, pos: KeyCode| -> f32 {
        f32::from(keys.pressed(pos)) - f32::from(keys.pressed(neg))
    };
    // A Bevy camera looks down its local -Z, so "forward" is -Z.
    let wish = rotation * Vec3::new(axis(KeyCode::KeyA, KeyCode::KeyD), 0.0, 0.0)
        + rotation * Vec3::new(0.0, 0.0, -axis(KeyCode::KeyS, KeyCode::KeyW))
        + Vec3::Y * axis(KeyCode::KeyF, KeyCode::KeyR);

    let mut speed = FREE_SPEED;
    if keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight) {
        speed *= 5.0;
    }
    if keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight) {
        speed *= 0.2;
    }
    free.at += wish.normalize_or_zero() * speed * time.delta_secs();

    tf.translation = free.at;
    tf.rotation = rotation;
    *global = GlobalTransform::from(*tf);
}

// ---------------------------------------------------------------------------
// The overlay
// ---------------------------------------------------------------------------

/// Marks the two lines of text at the bottom of the screen.
#[derive(Component)]
struct ReplayOverlay;

/// Two lines, bottom left: where we are, and what the keys do.
///
/// Deliberately not a scrub bar. `BACKLOG.md` §1 makes the timeline the primary
/// control surface of the clip editor and says it is worth designing properly;
/// a slider drawn here would be thrown away the week that starts, and until
/// then a tick counter answers the only question this phase has, which is
/// whether the transport works.
fn spawn_overlay(mut commands: Commands) {
    commands.spawn((
        ReplayOverlay,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(18.0),
            bottom: Val::Px(16.0),
            ..default()
        },
        Text::new(String::new()),
        TextFont {
            font_size: bevy::text::FontSize::Px(15.0),
            ..default()
        },
        TextColor(crate::ui::palette::PHOSPHOR),
        // Nothing to say until a recording is loaded, and `spawn_overlay` runs
        // in every build.
        Visibility::Hidden,
    ));
}

fn update_overlay(
    theatre: Res<Theatre>,
    free: Res<FreeCam>,
    target: Res<ViewTarget>,
    cockpit: Res<crate::cockpit::ViewMode>,
    mut overlay: Query<(&mut Text, &mut Visibility), With<ReplayOverlay>>,
) {
    let Ok((mut text, mut visible)) = overlay.single_mut() else {
        return;
    };
    *visible = Visibility::Inherited;

    let view = if free.active {
        "FREE".to_owned()
    } else if cockpit.seated {
        format!("SEAT {}", target.0)
    } else {
        format!("CHASE {}", target.0)
    };
    let seek = theatre
        .last_seek
        .map_or(String::new(), |c| format!("   seek {} ticks", c.ticks));

    let line = format!(
        "{}  {:>6.1} / {:.1} s   tick {} / {}   {:.2}x   {view}{seek}\n\
         SPACE play  , . step  <- -> scrub  HOME start  - = rate  TAB ride  V inside  G free",
        if theatre.playing { "PLAY " } else { "PAUSE" },
        theatre.timeline.elapsed(),
        theatre.timeline.duration(),
        theatre.timeline.cursor(),
        theatre.timeline.len(),
        theatre.rate(),
    );
    if text.0 != line {
        text.0 = line;
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn a_label_becomes_a_usable_file_name() {
        assert_eq!(slug("Skirmish on Space"), "skirmish-on-space");
        assert_eq!(slug("Campaign(3) on Space"), "campaign-3-on-space");
        assert_eq!(slug("---"), "");
    }

    /// The rate table has to contain the resting rate at the index the code
    /// starts from, or the first `-` press would change speed twice.
    #[test]
    fn the_normal_rate_is_where_the_code_thinks_it_is() {
        assert_eq!(RATES[NORMAL_RATE], 1.0);
        assert!(RATES.windows(2).all(|w| w[0] < w[1]), "rates must ascend");
    }

    /// A live match must look exactly as it did before this module existed.
    #[test]
    fn the_view_defaults_to_the_pilot() {
        assert_eq!(ViewTarget::default().0, LOCAL_ID);
        assert!(!FreeCam::default().active);
    }
}
