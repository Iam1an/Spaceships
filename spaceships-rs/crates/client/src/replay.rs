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
//! # The way in
//!
//! `SPACESHIPS_REPLAY=<file>` was the *only* way in, which meant the feature did
//! not exist for anybody who opens the `.dmg`. There is now a `REPLAYS` page on
//! the lobby's rail: [`Replays`] is the shelf — what is in `<state dir>/replays`,
//! newest first — and [`WatchReplay`] is the button. `ui.rs` draws the shelf and
//! writes the message; [`open_requested`] does exactly what [`install`] does,
//! except through `Commands` rather than at plugin-build time, so the two paths
//! open a recording identically.
//!
//! **The browser has no filesystem**, so on wasm the shelf is always empty and
//! [`Replays::note`] says why. The page is still there: a web player who is told
//! "recordings are saved by the desktop version" has learnt something, and a
//! rail key that appears on one build and not the other is worse.
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
            .init_resource::<Replays>()
            .add_message::<WatchReplay>()
            .add_systems(Startup, spawn_overlay)
            // The door from the lobby, and the two things that have to happen
            // when it is used in either direction. Neither is gated on
            // `replaying`: opening one is what *makes* a replay run, and
            // launching a match has to be able to stop one.
            .add_systems(Update, (open_requested, stop_on_launch))
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
                    .run_if(replaying.and_then(not(under_the_menu))),
            )
            // Outside the gate above, because it is the *transition* into the
            // menu it watches for: a system that stops running when the lobby
            // opens can never notice that the lobby opened.
            .add_systems(Update, pause_under_the_menu.run_if(replaying))
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

/// Where recordings are kept: `replays/` inside this installation's own
/// directory.
///
/// One definition, because two things now need it — the writer below and the
/// shelf the lobby lists — and a menu that looked somewhere other than where the
/// files land would be the most confusing possible bug in this feature.
#[cfg(not(target_arch = "wasm32"))]
pub fn replay_dir() -> Option<std::path::PathBuf> {
    crate::api::state_dir().map(|d| d.join("replays"))
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
    let Some(dir) = replay_dir() else {
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
// The shelf
// ---------------------------------------------------------------------------

/// One recording on disk, in the words a page can print.
///
/// Everything here comes out of the **file name**, which [`write_tape`] builds
/// as `<unix seconds>-<mode>-on-<map>.spr`. Nothing is decoded to list a
/// recording, and that is the point: a five-minute match on the mouse is most of
/// half a megabyte, and reading eight of them to fill in a column nobody asked
/// for would put a visible stall on the way into the page. What a player needs
/// in a list is *which match* and *when*, and the name carries both.
///
/// The consequence, stated because it is a real limitation: the list cannot show
/// how long a recording runs. That number is the step count, which is the end of
/// the file, and there is no way to it that does not read the whole thing. The
/// transport says it the moment the recording opens.
#[derive(Debug, Clone)]
pub struct ReplayEntry {
    /// `SKIRMISH ON SPACE`. The mode and map [`label_for`] wrote.
    pub label: String,
    /// `3 HOURS AGO`. Relative rather than a date, deliberately: the timestamp
    /// in the name is UTC and this build has no timezone database, so a printed
    /// clock time would be wrong by hours for most of the planet. An age is
    /// right everywhere.
    pub age: String,
    /// Kilobytes on disk, rounded up so a very short match is not `0`.
    pub size_kb: u64,
    /// The file itself. Private: `ui.rs` names a row by index and never handles
    /// a path, so there is one place that turns a choice into a file.
    #[cfg(not(target_arch = "wasm32"))]
    path: std::path::PathBuf,
}

/// What has been recorded, as the lobby's `REPLAYS` page shows it.
///
/// Filled by [`Replays::rescan`] when the page comes up, which is cheap — a
/// directory listing and a `stat` each — and re-run rather than watched, because
/// the only thing that writes into that directory is this process.
#[derive(Resource, Default)]
pub struct Replays {
    /// Newest first.
    pub entries: Vec<ReplayEntry>,
    /// Moves whenever the list or [`Replays::note`] does, so `ui.rs` can put a
    /// single comparison in front of the whole page — the discipline that file
    /// is built on.
    pub rev: u32,
    /// What to say when there is nothing to show, or when one would not open.
    /// Empty means "the list speaks for itself".
    pub note: String,
}

impl Replays {
    /// Re-reads the recordings directory.
    pub fn rescan(&mut self) {
        let (entries, note) = scan();
        self.entries = entries;
        self.note = note;
        self.rev = self.rev.wrapping_add(1);
    }

    /// Records why a recording would not open, for the page to print.
    fn complain(&mut self, note: impl Into<String>) {
        self.note = note.into();
        self.rev = self.rev.wrapping_add(1);
    }
}

/// The browser has no filesystem, so there is nothing to list and one honest
/// thing to say about it.
#[cfg(target_arch = "wasm32")]
fn scan() -> (Vec<ReplayEntry>, String) {
    (
        Vec::new(),
        "THE WEB VERSION DOES NOT SAVE REPLAYS - PLAY ON THE DESKTOP APP".to_owned(),
    )
}

/// Everything in `<state dir>/replays` that looks like a recording, newest
/// first.
///
/// Best effort throughout. A directory that cannot be read, a file whose name
/// says nothing, a `stat` that fails — none of those is worth refusing to show
/// the rest of the list over.
#[cfg(not(target_arch = "wasm32"))]
fn scan() -> (Vec<ReplayEntry>, String) {
    let Some(dir) = replay_dir() else {
        return (
            Vec::new(),
            "NO PLACE TO KEEP REPLAYS ON THIS MACHINE".to_owned(),
        );
    };
    let Ok(read) = std::fs::read_dir(&dir) else {
        // Not an error: the directory is created by the first match that ends.
        return (
            Vec::new(),
            "NO REPLAYS YET - EVERY MATCH YOU PLAY IS RECORDED".to_owned(),
        );
    };

    let now = unix_now();
    let mut found: Vec<(u64, ReplayEntry)> = Vec::new();
    for item in read.flatten() {
        let path = item.path();
        if path.extension().and_then(|e| e.to_str()) != Some(spaceships_replay::EXTENSION) {
            continue;
        }
        if let Some(pair) = describe(&path, now) {
            found.push(pair);
        }
    }
    // Newest first, and the name as a tie-break so the order is stable between
    // two recordings made in the same second.
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.label.cmp(&b.1.label)));

    let entries: Vec<ReplayEntry> = found.into_iter().map(|(_, e)| e).collect();
    let note = if entries.is_empty() {
        "NO REPLAYS YET - EVERY MATCH YOU PLAY IS RECORDED".to_owned()
    } else {
        String::new()
    };
    (entries, note)
}

/// One file, as a row — or `None` if it is not one of ours.
#[cfg(not(target_arch = "wasm32"))]
fn describe(path: &std::path::Path, now: u64) -> Option<(u64, ReplayEntry)> {
    let stem = path.file_stem()?.to_str()?;
    // `<unix>-<slug>`, which is what `write_tape` writes. A file that was
    // renamed or copied in by hand keeps its own name as the label and dates
    // itself off the filesystem, so it still lists rather than disappearing.
    let (stamp, slug) = match stem.split_once('-') {
        Some((left, right)) => (left.parse::<u64>().ok(), right),
        None => (None, stem),
    };

    let meta = std::fs::metadata(path).ok();
    let modified = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    let at = stamp.filter(|s| *s > 0).or(modified).unwrap_or(0);
    let size = meta.as_ref().map_or(0, std::fs::Metadata::len);

    let label = slug.replace(['-', '_'], " ").trim().to_ascii_uppercase();
    Some((
        at,
        ReplayEntry {
            label: if label.is_empty() {
                "MATCH".to_owned()
            } else {
                label
            },
            age: ago(now.saturating_sub(at)),
            size_kb: size.div_ceil(1024),
            path: path.to_owned(),
        },
    ))
}

/// How long ago, in the coarsest unit that still says something.
#[cfg(not(target_arch = "wasm32"))]
fn ago(seconds: u64) -> String {
    // A recording stamped in the future — a clock that was wound back, a file
    // copied off another machine — arrives here as zero (the caller saturates)
    // and reads as new rather than as nonsense.
    match seconds {
        0..=59 => "JUST NOW".to_owned(),
        60..=3599 => plural(seconds / 60, "MINUTE"),
        3600..=86_399 => plural(seconds / 3600, "HOUR"),
        _ => plural(seconds / 86_400, "DAY"),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn plural(n: u64, unit: &str) -> String {
    format!("{n} {unit}{} AGO", if n == 1 { "" } else { "S" })
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

/// Why a recording would not open.
///
/// Typed rather than a `String`, because the lobby has to tell two of these
/// apart: a file this build cannot re-simulate is *old*, which is a thing to
/// say plainly, and everything else is *broken*. Matching on a formatted message
/// would work today and stop working the first time one of those sentences is
/// reworded.
#[cfg(not(target_arch = "wasm32"))]
enum LoadFailure {
    /// The file could not be read at all.
    Io(std::io::Error),
    /// The bytes were read and refused.
    Decode(spaceships_replay::Error),
}

#[cfg(not(target_arch = "wasm32"))]
impl LoadFailure {
    /// One line for the page, in a player's words.
    fn note(&self) -> &'static str {
        use spaceships_replay::Error;
        match self {
            LoadFailure::Io(_) => "THAT REPLAY COULD NOT BE READ",
            // The rules fingerprint and the format version are the same fact
            // from a player's side: the game has moved on since it was recorded.
            LoadFailure::Decode(Error::RulesChanged { .. } | Error::UnknownVersion { .. }) => {
                "THAT REPLAY IS FROM AN OLDER VERSION OF THE GAME"
            }
            LoadFailure::Decode(_) => "THAT REPLAY FILE IS DAMAGED",
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl core::fmt::Display for LoadFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            LoadFailure::Io(e) => write!(f, "{e}"),
            LoadFailure::Decode(e) => write!(f, "{e}"),
        }
    }
}

/// Reads a recording and builds its seek index.
#[cfg(not(target_arch = "wasm32"))]
fn load(path: &std::path::Path) -> Result<Theatre, LoadFailure> {
    let bytes = std::fs::read(path).map_err(LoadFailure::Io)?;
    let recording = spaceships_replay::Recording::decode(&bytes).map_err(LoadFailure::Decode)?;

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

/// Everything swapping a recording in replaces, resolved before anything is
/// written.
///
/// Two callers put these somewhere: [`install`] at plugin-build time, for
/// `SPACESHIPS_REPLAY`, and [`open_requested`] through `Commands`, for the
/// lobby. Splitting the *deciding* from the *writing* is what keeps them one
/// behaviour — the alternative is the same eight resources assembled twice and
/// drifting the first time one is added.
#[cfg(not(target_arch = "wasm32"))]
struct Opened {
    world: SimWorldState,
    frame: sim::world::Frame,
    setup: MatchSetup,
    theatre: Theatre,
    free: FreeCam,
    view: crate::cockpit::ViewMode,
    ride: Option<EntityId>,
}

/// Winds a loaded recording to its opening moment and works out what the client
/// has to look like to show it.
///
/// The map has to move as well as the world: `terrain.rs` installs or tears the
/// Sierras down when [`MatchSetup::map`] changes, so a recording made on the
/// terrain map would otherwise be flown over an empty starfield with an
/// invisible kill plane in it.
#[cfg(not(target_arch = "wasm32"))]
fn open(mut theatre: Theatre) -> Opened {
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

    Opened {
        world,
        frame,
        setup,
        theatre,
        free: FreeCam {
            active: free,
            ..default()
        },
        view: crate::cockpit::ViewMode {
            first_person: seated,
            seated: false,
        },
        ride,
    }
}

/// The startup path: `SPACESHIPS_REPLAY` named a file before the app ran.
#[cfg(not(target_arch = "wasm32"))]
fn install(app: &mut App, theatre: Theatre) {
    let opened = open(theatre);
    app.insert_resource(SimWorld(opened.world))
        .insert_resource(opened.setup)
        .insert_resource(SimFrame(opened.frame))
        // Nothing is being recorded during a replay. Recording the replay would
        // produce a byte-identical copy of the file already on disk, one match
        // later.
        .insert_resource(Tape::default())
        .insert_resource(opened.free)
        .insert_resource(opened.view)
        .insert_resource(opened.theatre);
    if let Some(id) = opened.ride {
        app.insert_resource(ViewTarget(id));
    }
}

// ---------------------------------------------------------------------------
// The door from the lobby
// ---------------------------------------------------------------------------

/// "Watch this one." Raised by `ui.rs` with a row's index into
/// [`Replays::entries`].
///
/// An index rather than a path, so the lobby never handles a file name and this
/// module keeps the one route from a choice to a recording. It is also what
/// makes the message meaningful on wasm, where there are no paths at all: the
/// shelf is empty there, so every index misses and the page says why.
#[derive(Message, Debug, Clone, Copy)]
pub struct WatchReplay {
    /// Which row of [`Replays::entries`].
    ///
    /// Never read on the web, where the shelf cannot have rows: the handler
    /// there answers every request the same way, which is the honest thing for
    /// it to do and leaves the field unused on that target alone.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub index: usize,
}

/// Opens the recording the lobby picked.
///
/// The same [`open`] the startup path uses, through `Commands` instead of an
/// `App` — so a menu-opened replay and a `SPACESHIPS_REPLAY` one are the same
/// thing, including the map swap and the opening view.
///
/// **Failure reopens the menu.** `ui.rs` closes the display optimistically when
/// the row is pressed, because in the overwhelming case the file opens; a
/// recording that will not decode — one made under different `Rules`, a
/// truncated write — would otherwise leave the pilot staring at whatever was on
/// screen before with no display and no replay. [`crate::ui::ReturnToLobby`]
/// brings it straight back, and [`Replays::note`] is on the page saying why.
#[cfg(not(target_arch = "wasm32"))]
fn open_requested(
    mut requests: MessageReader<WatchReplay>,
    mut commands: Commands,
    mut replays: ResMut<Replays>,
    mut fixed: ResMut<Time<Fixed>>,
    mut back: MessageWriter<crate::ui::ReturnToLobby>,
) {
    // `last`, on the same reasoning as `apply_start_match`: two in one frame is
    // two presses, and decoding the loser first is wasted work.
    let Some(request) = requests.read().last().copied() else {
        return;
    };
    let Some(entry) = replays.entries.get(request.index) else {
        replays.complain("THAT REPLAY IS NO LONGER THERE");
        back.write(crate::ui::ReturnToLobby);
        return;
    };
    let path = entry.path.clone();

    let theatre = match load(&path) {
        Ok(theatre) => theatre,
        Err(e) => {
            error!("replay: cannot play {}: {e}", path.display());
            replays.complain(e.note());
            back.write(crate::ui::ReturnToLobby);
            return;
        }
    };

    let opened = open(theatre);
    // A previous replay may have left the clock in slow motion. The rate is the
    // fixed timestep — see [`drive_transport`] — so it has to be put back, or
    // the new recording opens at whatever speed the last one was left at.
    fixed.set_timestep_hz(f64::from(TICK_HZ as f32 * RATES[NORMAL_RATE]));
    commands.insert_resource(SimWorld(opened.world));
    commands.insert_resource(opened.setup);
    commands.insert_resource(SimFrame(opened.frame));
    commands.insert_resource(Tape::default());
    commands.insert_resource(opened.free);
    commands.insert_resource(opened.view);
    commands.insert_resource(ViewTarget(opened.ride.unwrap_or(LOCAL_ID)));
    commands.insert_resource(opened.theatre);
}

/// The web has no files, so there is nothing to open and the page has already
/// said so. The message is still consumed: a queue nobody drains is a leak.
#[cfg(target_arch = "wasm32")]
fn open_requested(mut requests: MessageReader<WatchReplay>, mut replays: ResMut<Replays>) {
    if requests.read().next().is_some() {
        requests.clear();
        replays.complain("THE WEB VERSION DOES NOT SAVE REPLAYS");
    }
}

/// Launching a match ends the replay.
///
/// Without this, `ESC` out of a recording and pressing `START` would build a new
/// world and then hand every fixed step straight back to [`Theatre`], which
/// would overwrite it with the recording again — a match that starts and is
/// instantly replaced by the thing you were watching. Dropping the resource is
/// the whole of it: `replaying` is `Option<Res<Theatre>>`, so the transport, the
/// overlay and `fixed_tick`'s replay branch all stand down together.
fn stop_on_launch(
    mut requests: MessageReader<crate::ui::LaunchRequest>,
    theatre: Option<Res<Theatre>>,
    mut commands: Commands,
    mut fixed: ResMut<Time<Fixed>>,
    mut free: ResMut<FreeCam>,
    mut target: ResMut<ViewTarget>,
) {
    if requests.read().next().is_none() {
        return;
    }
    requests.clear();
    if theatre.is_none() {
        return;
    }
    commands.remove_resource::<Theatre>();
    fixed.set_timestep_hz(f64::from(TICK_HZ as f32));
    // The two things a replay leaves pointing somewhere else. A pilot who
    // launched a match from a drone camera parked over somebody else's wing
    // would otherwise fly it from there.
    *free = FreeCam::default();
    *target = ViewTarget::default();
}

/// Run condition: the lobby is covering the screen.
fn under_the_menu(lobby: Option<Res<crate::ui::LobbyOpen>>) -> bool {
    lobby.is_some_and(|l| l.0)
}

/// Stops the clock when the display comes up over a replay.
///
/// `sim_bridge::fixed_tick` takes its replay branch *above* the pause that
/// freezes a solo match behind the menu, so a recording left playing would run
/// on underneath the lobby — and the pilot would come back to a match several
/// minutes further on than they left it. Pausing here rather than there keeps
/// the transport's state where the transport lives: the recording is paused, the
/// overlay says `PAUSE`, and `Space` starts it again.
fn pause_under_the_menu(
    lobby: Option<Res<crate::ui::LobbyOpen>>,
    mut theatre: ResMut<Theatre>,
    mut was_up: Local<bool>,
) {
    let up = lobby.is_some_and(|l| l.0);
    if up && !*was_up && theatre.playing {
        theatre.playing = false;
    }
    *was_up = up;
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

    /// The round trip the shelf rests on: a label becomes a file name, and the
    /// file name becomes a label a player can read.
    #[test]
    fn a_file_name_reads_back_as_the_match_it_was() {
        let name = format!("1700000000-{}.spr", slug("Skirmish on Space"));
        let (at, entry) = describe(std::path::Path::new(&name), 1_700_000_000)
            .expect("a name of ours describes itself");
        assert_eq!(at, 1_700_000_000);
        assert_eq!(entry.label, "SKIRMISH ON SPACE");
        assert_eq!(entry.age, "JUST NOW");
    }

    /// A file somebody copied in by hand still lists rather than vanishing.
    #[test]
    fn a_hand_named_file_still_lists() {
        let entry = describe(std::path::Path::new("the good one.spr"), 0);
        assert_eq!(entry.expect("still described").1.label, "THE GOOD ONE");
    }

    #[test]
    fn an_age_reads_in_the_coarsest_useful_unit() {
        assert_eq!(ago(0), "JUST NOW");
        assert_eq!(ago(59), "JUST NOW");
        assert_eq!(ago(60), "1 MINUTE AGO");
        assert_eq!(ago(60 * 59), "59 MINUTES AGO");
        assert_eq!(ago(60 * 60), "1 HOUR AGO");
        assert_eq!(ago(60 * 60 * 5), "5 HOURS AGO");
        assert_eq!(ago(60 * 60 * 24), "1 DAY AGO");
        assert_eq!(ago(60 * 60 * 24 * 9), "9 DAYS AGO");
    }

    /// Empty is a state the page has to be able to describe, and an empty note
    /// would leave it describing nothing.
    #[test]
    fn an_empty_shelf_says_why() {
        let mut shelf = Replays::default();
        let was = shelf.rev;
        shelf.rescan();
        assert_ne!(shelf.rev, was, "a rescan has to be visible to the page");
        if shelf.entries.is_empty() {
            assert!(!shelf.note.is_empty(), "an empty list must explain itself");
        }
    }
}
