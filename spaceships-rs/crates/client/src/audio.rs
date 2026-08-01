//! audio — the port of `public/src/audio.js`.
//!
//! Three things live here, in increasing order of how much thought they need:
//!
//! 1. **One-shots.** A gunshot, an impact, a rock breaking. Fire and forget,
//!    with a per-sound volume, an optional play throttle, and the distance
//!    attenuation `main.js` calls `distanceVol`.
//! 2. **Loops.** The engine (`move`), the boost, and the music. Each is one
//!    clip played end to end forever, crossfaded across the seam so the joint
//!    is inaudible, with independent volume control.
//! 3. **The voice warning system.** Fourteen cockpit callouts that *arbitrate*
//!    rather than stack: one voice at a time, a callout already speaking is cut
//!    off only by something strictly more urgent, and each has a repeat
//!    cooldown so it does not nag. This is the part with the interesting
//!    design, and it is described at [`arbitrate`].
//!
//! # Buses
//!
//! The JS builds two: a `sfxMaster` gain that everything except the music
//! passes through, and the music, which connects straight to the destination so
//! the SFX slider cannot duck it. There is no Web Audio graph here, so the same
//! routing is arithmetic — see [`MixerLevels`]. Bevy's own [`GlobalVolume`] is
//! left at 1.0 and is available as a third, outermost knob.
//!
//! [`GlobalVolume`]: bevy::audio::GlobalVolume
//!
//! # Where the sounds come from
//!
//! Everything the simulation already reports drives itself: [`SimEvent`]s for
//! weapons fire, impacts, and destruction, and [`sim::world::HudState`] for the
//! warning conditions. What the simulation does *not* report is exposed on
//! [`AudioCommands`] for the game loop to call. See [`AudioCommands`] for the
//! list of gaps.
//!
//! # Assets
//!
//! Nine effects in `public/sounds/` and fourteen voice callouts in
//! `public/sounds/warnings/`, all mp3. The paths below are the same strings the
//! JS uses, resolved against the shared `public/` asset root (`main.rs`).
//!
//! mp3 decoding is not free to enable: it needs bevy's `mp3` feature, which
//! pulls in `symphonia`. `bevy_audio` is likewise *not* implied by the `3d`
//! feature set in 0.19 — see the comments in `Cargo.toml`.
//!
//! # WASM: the browser will not make a sound until the page resumes the
//! `AudioContext`, and this crate cannot do it
//!
//! Traced through the 0.19 dependency graph, because the failure mode is
//! "everything initialises cleanly and there is silence", which is expensive to
//! debug from scratch:
//!
//! - `bevy_audio` → `rodio` 0.22 (`playback` + `wasm-bindgen`) → `cpal` 0.17's
//!   **`webaudio`** host. The `audioworklet` host is not compiled in; it needs
//!   cpal's `audioworklet` feature *and* `target-feature=atomics`.
//! - The `AudioContext` is constructed **eagerly inside
//!   `bevy_audio::AudioPlugin::build`**, via `init_resource::<AudioOutput>()`.
//!   That is always before any user gesture, so the browser's autoplay policy
//!   starts it `suspended`.
//! - cpal *does* call `ctx.resume()`, but `web_sys`'s `resume()` returns
//!   `Ok(pending_promise)` when the policy blocks it rather than erroring, the
//!   promise is dropped un-awaited, and the `Err` paths in rodio and bevy
//!   ("No audio device found.") never fire.
//! - The pump then stalls. `play()` arms exactly two one-shot `setTimeout`s and
//!   thereafter self-drives off each buffer's `ended` event, which never fires
//!   while `currentTime` is frozen. About 100 ms of audio is pulled from the
//!   mixer and everything stops.
//! - **There is no public path to that `AudioContext`.** cpal exposes it
//!   (`Stream::as_inner` → `StreamInner::WebAudio` → `Stream::audio_context`),
//!   but rodio keeps the `cpal::Stream` in a private field with no accessor,
//!   and `bevy_audio`'s `AudioOutput` is `pub(crate)`. Depending on rodio
//!   directly does not help.
//!
//! The fix is six lines of JS in `crates/client/web/index.html`, which has to
//! run **before** the wasm module is instantiated (cpal goes through the global
//! constructor, so patching it catches the one cpal makes):
//!
//! ```js
//! const Ctor = window.AudioContext || window.webkitAudioContext;
//! const live = [];
//! window.AudioContext = window.webkitAudioContext =
//!   new Proxy(Ctor, { construct: (t, a) => { const c = new t(...a); live.push(c); return c; } });
//! const resume = () => { live.forEach((c) => c.resume().catch(() => {})); };
//! addEventListener('pointerdown', resume);
//! addEventListener('keydown', resume);
//! ```
//!
//! Bevy acknowledges the requirement only in `examples/README.md` and ships no
//! workaround. [`Unlocked`] below is this module's half of the deal: it holds
//! *our* playback until the first key or click, so nothing is queued into a
//! frozen context and the loops start in phase with the resumed clock.
//!
//! `build-wasm.sh` also has to copy `public/sounds/` into `web/assets/sounds/`;
//! today it copies only `asteroid.jpg`.

use bevy::audio::{
    AudioPlayer, AudioSink, AudioSinkPlayback, AudioSource, PlaybackSettings, Volume,
};
use bevy::prelude::*;

use sim::world::{ExplosionKind, ShipFlags, ShipView, SimEvent};
use spaceships_sim as sim;

use crate::sim_bridge::{SimFrame, SimSet};

// ---------------------------------------------------------------------------
// The tables
// ---------------------------------------------------------------------------
//
// `audio.js` keeps its behaviour in five object literals — SOUNDS, VOLUMES,
// PLAY_THROTTLE, WARN_PRIORITY, WARN_COOLDOWN — and the functions below them
// only look things up. That factoring is the reason the warning system is
// legible at all, so it survives the port: the tables are data, indexed by the
// enums, and nothing below hardcodes a sound's name.

/// A one-shot effect. `SOUNDS` in `audio.js`, minus the three that are only
/// ever looped (see [`Loop`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[repr(u8)]
pub enum Sfx {
    /// Gun report. Bullets, beams, and missile launches all use this one clip.
    Shoot,
    /// A ship exploding.
    ShipDeath,
    /// A bolt hitting something that is not a ship.
    Impact,
    /// An asteroid breaking up.
    RockBreak,
    /// The "you hit them" confirmation blip.
    Hitmarker,
    /// Countermeasures away.
    FlareDeploy,
}

impl Sfx {
    /// Every variant, in table order. The tables are indexed by `as usize`, so
    /// this is what keeps the two in step — see [`load_assets`].
    const ALL: [Sfx; SFX_COUNT] = [
        Sfx::Shoot,
        Sfx::ShipDeath,
        Sfx::Impact,
        Sfx::RockBreak,
        Sfx::Hitmarker,
        Sfx::FlareDeploy,
    ];
}

/// How many [`Sfx`] variants there are.
const SFX_COUNT: usize = 6;

/// One row of `SOUNDS` + `VOLUMES` + `PLAY_THROTTLE`.
struct SfxDef {
    /// Asset path, relative to the shared `public/` root.
    path: &'static str,
    /// `VOLUMES[name]`.
    volume: f32,
    /// `PLAY_THROTTLE[name]`, in seconds. Zero means no throttle.
    throttle: f32,
}

/// Indexed by `Sfx as usize`; the order must match the enum.
const SFX: [SfxDef; SFX_COUNT] = [
    SfxDef {
        path: "sounds/shoot.mp3",
        volume: 0.28,
        // The gun fires every 0.05 s and a bolt every 0.03 s would still be a
        // machine-gun; without this the clip retriggers faster than it decays
        // and the mix turns to mud. `PLAY_THROTTLE.shoot` in the JS.
        throttle: 0.03,
    },
    SfxDef {
        path: "sounds/shipdeath.mp3",
        volume: 0.6,
        throttle: 0.0,
    },
    SfxDef {
        path: "sounds/impact.mp3",
        volume: 0.45,
        throttle: 0.0,
    },
    SfxDef {
        path: "sounds/rockbreak.mp3",
        volume: 0.55,
        throttle: 0.0,
    },
    SfxDef {
        path: "sounds/hitmarker_2.mp3",
        volume: 0.25,
        throttle: 0.0,
    },
    SfxDef {
        path: "sounds/flare_deploy.mp3",
        volume: 0.55,
        throttle: 0.0,
    },
];

/// A continuously looping bed.
///
/// These are the three entries `audio.js` drives through `setLoopVolume`
/// rather than `play`. Their `VOLUMES` entries are dead in the JS — the loop's
/// gain *is* its volume — so the maxima live with the mix in [`drive_loops`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Loop {
    /// Engine bed. `move` in the JS.
    Engine,
    /// Boost roar, ducked in over the engine.
    Boost,
    /// The soundtrack.
    Music,
}

impl Loop {
    /// Every variant, in table order.
    const ALL: [Loop; LOOP_COUNT] = [Loop::Engine, Loop::Boost, Loop::Music];
}

/// How many [`Loop`] variants there are.
const LOOP_COUNT: usize = 3;

/// One looping bed.
struct LoopDef {
    /// Asset path.
    path: &'static str,
    /// Clip length in seconds.
    ///
    /// Needed because the crossfade has to know where the seam is, and neither
    /// [`AudioSink`] nor [`AudioSource`] reports a total duration in 0.19.
    /// Measured with `afinfo public/sounds/*.mp3`. Getting it wrong is not
    /// fatal — [`drive_loops`] restarts the schedule the moment a bed runs dry
    /// — it just costs the seamlessness it is there to buy.
    secs: f32,
    /// Whether this bed is ducked by the SFX slider. The music is not; it goes
    /// to the destination directly, exactly as in the JS.
    on_sfx_bus: bool,
}

/// Indexed by `Loop as usize`.
const LOOPS: [LoopDef; LOOP_COUNT] = [
    LoopDef {
        path: "sounds/move.mp3",
        secs: 62.093_06,
        on_sfx_bus: true,
    },
    LoopDef {
        path: "sounds/boost.mp3",
        secs: 2.424,
        on_sfx_bus: true,
    },
    LoopDef {
        path: "sounds/dumb_Eflatmin.mp3",
        secs: 69.982_04,
        on_sfx_bus: false,
    },
];

/// A cockpit voice callout. `WARNINGS` in `audio.js`.
///
/// Kept apart from [`Sfx`] because these go through [`AudioCommands::warn`]
/// rather than [`AudioCommands::play`]: they arbitrate against each other
/// instead of stacking, the way a real voice warning system does.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[repr(u8)]
#[allow(
    clippy::enum_variant_names,
    reason = "`Warning::Warning` is the callout the JS calls `warning`, backed by \
              `warnings/warning.mp3`. Renaming it to please the lint would break \
              the one-to-one mapping between variant, table row, and asset that \
              the rest of this module relies on."
)]
pub enum Warning {
    /// Ground proximity, descending. The most urgent thing the box can say.
    PullUp,
    /// Ground proximity, not yet descending into it.
    Altitude,
    /// Generic advisory.
    Caution,
    /// Hull integrity.
    Warning,
    /// Airframe stress — the brake charge has gone past the warn threshold.
    MasterCaution,
    /// An enemy missile is tracking you.
    Lock,
    /// Radar warning receiver: you are being painted.
    RwrLock,
    /// Fuel state. Here, boost fuel.
    Bingo,
    /// Countermeasures away.
    Flare,
    /// Jamming.
    Jammer,
    /// Track-while-scan: searching.
    TwsSearch,
    /// Track-while-scan: locked.
    TwsLock,
    /// Track-while-scan: launch, first phrasing.
    TwsLaunch1,
    /// Track-while-scan: launch, second phrasing.
    TwsLaunch2,
}

impl Warning {
    /// Every variant, in table order.
    const ALL: [Warning; WARN_COUNT] = [
        Warning::PullUp,
        Warning::Altitude,
        Warning::Caution,
        Warning::Warning,
        Warning::MasterCaution,
        Warning::Lock,
        Warning::RwrLock,
        Warning::Bingo,
        Warning::Flare,
        Warning::Jammer,
        Warning::TwsSearch,
        Warning::TwsLock,
        Warning::TwsLaunch1,
        Warning::TwsLaunch2,
    ];
}

/// How many [`Warning`] variants there are.
const WARN_COUNT: usize = 14;

/// One row of `WARNINGS` + `WARN_PRIORITY` + `WARN_COOLDOWN`.
struct WarnDef {
    /// Asset path.
    path: &'static str,
    /// Higher wins. A callout already speaking is cut off only by something
    /// **strictly** more urgent: "pull up" interrupts "bingo" mid-word, never
    /// the reverse, and equal urgency waits its turn rather than doubling up.
    priority: u8,
    /// Minimum seconds between repeats of this callout. The classic failure is
    /// "PULL UP" firing forty times in one canyon run until players mute the
    /// game.
    cooldown: f32,
    /// How long the phrase takes to say. Not used at playback time — the
    /// cooldown has to exceed it or a second trigger clips the callout
    /// mid-sentence, and `cooldowns_outlast_their_phrases` pins that.
    secs: f32,
}

/// Indexed by `Warning as usize`.
const WARNINGS: [WarnDef; WARN_COUNT] = [
    WarnDef {
        path: "sounds/warnings/pull_up.mp3",
        priority: 100,
        cooldown: 1.6,
        secs: 1.044_898,
    },
    WarnDef {
        path: "sounds/warnings/altitude.mp3",
        priority: 55,
        cooldown: 3.0,
        secs: 1.044_898,
    },
    WarnDef {
        path: "sounds/warnings/caution.mp3",
        priority: 50,
        cooldown: 4.0,
        secs: 1.018_776,
    },
    WarnDef {
        path: "sounds/warnings/warning.mp3",
        priority: 80,
        cooldown: 6.0,
        secs: 2.037_551,
    },
    WarnDef {
        path: "sounds/warnings/master_caution.mp3",
        priority: 40,
        cooldown: 4.0,
        secs: 1.593_469,
    },
    WarnDef {
        path: "sounds/warnings/lock.mp3",
        priority: 70,
        cooldown: 3.0,
        secs: 0.940_408,
    },
    WarnDef {
        path: "sounds/warnings/rwr_lock.mp3",
        priority: 70,
        cooldown: 3.0,
        secs: 2.037_551,
    },
    WarnDef {
        path: "sounds/warnings/bingo.mp3",
        priority: 20,
        cooldown: 12.0,
        secs: 1.044_898,
    },
    WarnDef {
        path: "sounds/warnings/flare.mp3",
        priority: 15,
        // "chaff, flares" is a 3.4 s phrase, so its cooldown has to exceed its
        // own length or a second press cuts the callout off mid-sentence.
        cooldown: 4.5,
        secs: 3.395_918,
    },
    WarnDef {
        path: "sounds/warnings/jammer.mp3",
        priority: 35,
        cooldown: 5.0,
        secs: 1.044_898,
    },
    WarnDef {
        path: "sounds/warnings/tws_search.mp3",
        priority: 30,
        cooldown: 4.0,
        secs: 1.985_306,
    },
    WarnDef {
        path: "sounds/warnings/tws_lock.mp3",
        priority: 50,
        cooldown: 4.0,
        secs: 2.977_959,
    },
    WarnDef {
        path: "sounds/warnings/tws_launch_1.mp3",
        priority: 70,
        cooldown: 3.0,
        secs: 2.037_551,
    },
    WarnDef {
        path: "sounds/warnings/tws_launch_2.mp3",
        priority: 70,
        cooldown: 3.0,
        secs: 1.750_204,
    },
];

/// Playback level for every callout. `WARN_VOLUME` in the JS.
const WARN_VOLUME: f32 = 0.7;

/// A callout must not be able to interrupt itself.
///
/// The JS spells this out for `flare` — "a 3.4 s phrase, so its cooldown has to
/// exceed its own length or a second press cuts the callout off mid-sentence" —
/// and it holds for all fourteen. Checked at compile time rather than in a test
/// because it is a property of the table, and a table edit that breaks it
/// should not build.
const _: () = {
    let mut i = 0;
    while i < WARN_COUNT {
        assert!(
            WARNINGS[i].cooldown > WARNINGS[i].secs,
            "a callout's cooldown must outlast the phrase, or it clips itself"
        );
        i += 1;
    }
};

/// Which effect an explosion makes, if any.
///
/// `sim` reports a destruction twice — once specifically
/// ([`SimEvent::AsteroidDestroyed`]) and once generically
/// ([`SimEvent::Explosion`]) — because the two feed different subsystems. Audio
/// has to pick exactly one of each pair or the sound doubles, so the specific
/// event wins wherever there is one and this table returns `None`.
const EXPLOSION_SFX: [(ExplosionKind, Option<Sfx>); 5] = [
    (ExplosionKind::Impact, Some(Sfx::Impact)),
    (ExplosionKind::MissileHit, Some(Sfx::Impact)),
    // `AsteroidDestroyed` and `ShipDestroyed` carry the position *and* the
    // identity, so they drive `rockbreak` and `shipdeath` instead.
    (ExplosionKind::AsteroidBreak, None),
    (ExplosionKind::ShipDeath, None),
    // Likewise `FlareBurst`, which knows whose flare it was.
    (ExplosionKind::FlareBurst, None),
];

// ---------------------------------------------------------------------------
// Distance attenuation
// ---------------------------------------------------------------------------

/// Inside this, a sound plays at full volume. `SFX_NEAR_DIST`, `main.js:516`.
const SFX_NEAR_DIST: f32 = 80.0;
/// Beyond this, silence. `SFX_FAR_DIST`, `main.js:517`.
const SFX_FAR_DIST: f32 = 900.0;

/// The gain multiplier for a sound `pos` away from the listener.
///
/// A direct port of `distanceVol` (`main.js:518`): flat inside the near
/// radius, then a squared falloff to zero at the far radius. Squared rather
/// than linear because linear falloff sounds wrong — distant shots stay
/// audible far too long.
///
/// Note this is a *gain*, not a pan. The JS has no positional audio and
/// switching to bevy's spatial sinks would change the mix, so it is left as
/// the JS has it; `PlaybackSettings::with_spatial` is the upgrade path.
fn distance_vol(listener: Vec3, pos: Vec3) -> f32 {
    let d = listener.distance(pos);
    if d <= SFX_NEAR_DIST {
        return 1.0;
    }
    if d >= SFX_FAR_DIST {
        return 0.0;
    }
    let u = 1.0 - (d - SFX_NEAR_DIST) / (SFX_FAR_DIST - SFX_NEAR_DIST);
    u * u
}

// ---------------------------------------------------------------------------
// The engine mix
// ---------------------------------------------------------------------------
//
// `main.js:1575`-`:1593`. The JS computes these in the game loop and pushes
// them into `setLoopVolume`; here they are derived from `Frame` directly,
// because everything they need is already in a `ShipView` and a knob nobody
// turns is worse than no knob.

/// Engine bed at full chat. `MOVE_MAX_VOL`.
const MOVE_MAX_VOL: f32 = 0.25;
/// Boost roar at full chat. `BOOST_MAX_VOL`.
const BOOST_MAX_VOL: f32 = 0.4;
/// Speed at which the engine bed reaches [`MOVE_MAX_VOL`]. `SPEED_FOR_FULL_VOL`.
const SPEED_FOR_FULL_VOL: f32 = 80.0;
/// How far the engine ducks under the boost. `MOVE_DUCK_BOOST`.
const MOVE_DUCK_BOOST: f32 = 0.25;
/// How far the engine ducks under the brake. `MOVE_DUCK_BRAKE`.
const MOVE_DUCK_BRAKE: f32 = 0.4;
/// Engine convergence rate.
const MOVE_DAMP: f32 = 4.0;
/// Boost convergence rate — snappier, so the roar arrives with the shove.
const BOOST_DAMP: f32 = 5.0;

/// Frame-rate independent exponential approach. `THREE.MathUtils.damp`.
fn damp(current: f32, target: f32, lambda: f32, dt: f32) -> f32 {
    current + (target - current) * (1.0 - (-lambda * dt).exp())
}

// ---------------------------------------------------------------------------
// Loop crossfade
// ---------------------------------------------------------------------------

/// Overlap between one pass of a looping bed and the next, in seconds.
///
/// `startSeamlessLoop` uses `min(0.08, dur * 0.1)`; every bed here is longer
/// than 0.8 s so it is always the 80 ms.
const XFADE: f32 = 0.08;

/// The gain envelope for one pass of a looping bed, `t` seconds in.
///
/// Ramp up over the first [`XFADE`], hold, ramp down over the last [`XFADE`].
/// The successor starts at `dur - XFADE`, so the two ramps overlap and the
/// seam — where an mp3's encoder padding would otherwise put an audible tick on
/// every repeat — is covered. `bevy_audio`'s `PlaybackMode::Loop` is gapless in
/// *scheduling*, which is a different thing and does not help here.
fn loop_envelope(t: f32, dur: f32) -> f32 {
    if t <= 0.0 || t >= dur {
        return 0.0;
    }
    if t < XFADE {
        return t / XFADE;
    }
    let tail = dur - XFADE;
    if t > tail {
        return (dur - t) / XFADE;
    }
    1.0
}

// ---------------------------------------------------------------------------
// Warning arbitration
// ---------------------------------------------------------------------------

/// What [`arbitrate`] decided to do with a requested callout.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verdict {
    /// Nothing is speaking. Say it.
    Play,
    /// Something less urgent is speaking. Cut it off mid-word and say this.
    Interrupt,
    /// This callout said its piece too recently.
    Cooling,
    /// Something at least as urgent is speaking. Stay quiet; do not queue.
    Outranked,
}

/// The whole of `warn()`'s decision, with none of its side effects.
///
/// Two rules, in this order:
///
/// 1. **Cooldown.** If this callout spoke less than its cooldown ago, it is
///    silent — regardless of what else is happening. This is what stops the box
///    nagging, and it is checked first so that an urgent-but-recent callout
///    does not get to interrupt a less urgent one just to say the same thing
///    again.
/// 2. **Priority.** If something is already speaking, only a **strictly** more
///    urgent callout cuts it off. Equal priority loses, deliberately: two
///    equally urgent things saying themselves over each other is how a warning
///    system becomes noise, and the second one will still be true a moment
///    later.
///
/// Note what is *not* here: a queue. An outranked callout is dropped, not
/// deferred. A warning that is still true when the voice frees up will be
/// requested again by whatever detected it; one that is no longer true should
/// not be announced late.
fn arbitrate(w: Warning, now: f32, last_warned: f32, active: Option<u8>) -> Verdict {
    let def = &WARNINGS[w as usize];
    if now - last_warned < def.cooldown {
        return Verdict::Cooling;
    }
    match active {
        Some(speaking) if def.priority <= speaking => Verdict::Outranked,
        Some(_) => Verdict::Interrupt,
        None => Verdict::Play,
    }
}

// ---------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------

/// The loaded clips, one handle per table row.
#[derive(Resource)]
struct AudioAssets {
    /// Indexed by `Sfx as usize`.
    sfx: [Handle<AudioSource>; SFX_COUNT],
    /// Indexed by `Loop as usize`.
    loops: [Handle<AudioSource>; LOOP_COUNT],
    /// Indexed by `Warning as usize`.
    warnings: [Handle<AudioSource>; WARN_COUNT],
}

/// The two master faders, matching the JS settings drawer.
///
/// Write these from wherever the settings UI ends up living. Both are clamped
/// on use, so an out-of-range value cannot blow the mix.
#[derive(Resource, Debug, Clone, Copy)]
pub struct MixerLevels {
    /// Everything except the music. `sfxMaster` in the JS.
    pub sfx: f32,
    /// The soundtrack, which the SFX fader deliberately does not touch.
    pub music: f32,
}

impl Default for MixerLevels {
    fn default() -> Self {
        // `main.js:511` reads these from localStorage and falls back to these.
        MixerLevels {
            sfx: 1.0,
            music: 0.6,
        }
    }
}

/// Whether playback has been unlocked by a user gesture.
///
/// Native audio is never suspended, so this starts unlocked there. See the
/// module docs for why the browser needs it and what else the page has to do.
#[derive(Resource)]
struct Unlocked(bool);

impl Default for Unlocked {
    fn default() -> Self {
        Unlocked(!cfg!(target_arch = "wasm32"))
    }
}

/// Last play time per [`Sfx`], for `PLAY_THROTTLE`.
#[derive(Resource)]
struct PlayThrottle {
    /// Indexed by `Sfx as usize`.
    last: [f32; SFX_COUNT],
}

impl Default for PlayThrottle {
    fn default() -> Self {
        PlayThrottle {
            last: [f32::NEG_INFINITY; SFX_COUNT],
        }
    }
}

/// The single voice channel: what is speaking, and when each callout last did.
#[derive(Resource)]
struct Voice {
    /// The callout currently speaking, if any.
    active: Option<ActiveVoice>,
    /// Last spoken time per [`Warning`], indexed by `Warning as usize`.
    last: [f32; WARN_COUNT],
}

impl Default for Voice {
    fn default() -> Self {
        Voice {
            active: None,
            last: [f32::NEG_INFINITY; WARN_COUNT],
        }
    }
}

/// The callout currently speaking.
struct ActiveVoice {
    /// Its priority, cached so arbitration does not need the enum back.
    priority: u8,
    /// The entity playing it. Despawning it stops the clip mid-word: rodio's
    /// player stops on drop unless it was detached, and bevy never detaches.
    entity: Entity,
}

/// Where the listener is, for [`distance_vol`]. The local ship, or the origin
/// before there is one.
#[derive(Resource, Default)]
struct Listener(Vec3);

/// Loop gains and the crossfade schedule.
#[derive(Resource, Default)]
struct LoopMix {
    /// Engine bed gain, damped toward its target.
    engine: f32,
    /// Boost gain, damped toward its target.
    boost: f32,
    /// When the next pass of each bed should start, indexed by `Loop as usize`.
    next_start: [f32; LOOP_COUNT],
    /// Whether each bed's schedule has begun.
    running: [bool; LOOP_COUNT],
}

/// Previous-frame HUD state, for the edge-triggered warnings.
struct WarnEdges {
    /// Previous `missile_lock_warning`.
    locked: bool,
    /// Previous `hp01`.
    hp01: f32,
    /// Previous `boost01`.
    boost01: f32,
    /// Previous `overcharge01`.
    overcharge01: f32,
}

impl Default for WarnEdges {
    fn default() -> Self {
        // Start above every threshold so a fresh spawn does not immediately
        // announce that it is damaged and out of fuel.
        WarnEdges {
            locked: false,
            hp01: 1.0,
            boost01: 1.0,
            overcharge01: 0.0,
        }
    }
}

/// Marks a one-shot effect entity, so a future EMP or scene teardown can find
/// them all.
#[derive(Component)]
struct OneShot;

/// Marks a voice callout entity.
#[derive(Component)]
struct VoiceLine;

/// Marks one pass of a looping bed.
#[derive(Component)]
struct LoopVoice {
    /// Which bed, as `Loop as usize`.
    which: usize,
    /// When this pass started, on the [`Time`] clock.
    start: f32,
}

// ---------------------------------------------------------------------------
// The public API
// ---------------------------------------------------------------------------

/// Anything the audio system should do that the simulation does not report.
///
/// A queue rather than direct playback, so that callers need only `ResMut` and
/// the arbitration stays in one system with one clock.
///
/// Most sounds need nothing from here — [`watch_sim`] already drives weapons
/// fire, impacts, destruction, flares, and the hull/fuel/lock/airframe
/// callouts off `Frame`. What is left is what `sim` has no event for:
///
/// - **Ground proximity.** [`Warning::PullUp`] and [`Warning::Altitude`] need
///   height above the terrain kill plane and vertical speed
///   (`main.js:2280`-`:2287`). `HudState` reports neither, and `Frame` carries
///   no terrain sample under the ship, so whoever ports the terrain map has to
///   call [`AudioCommands::warn`] each frame. They are *level*-triggered on
///   purpose — the danger persists, so the callout repeats at its cooldown
///   until you climb out.
/// - **The radar callouts.** [`Warning::RwrLock`], [`Warning::Jammer`], and the
///   four `Tws*` variants have no trigger in the JS either. The clips and the
///   table entries are here; the sensor model that fires them is not.
/// - **[`Warning::Caution`]**, likewise unused so far.
/// - **EMP.** [`AudioCommands::stop_warnings`] silences the cockpit mid-
///   sentence, which is the whole reason `stopWarnings()` exists in the JS.
#[derive(Resource, Default)]
pub struct AudioCommands {
    /// Drained every frame by [`run_commands`].
    queue: Vec<Command>,
}

/// One queued request.
enum Command {
    /// Play an effect at `gain` times its table volume.
    Play(Sfx, f32),
    /// Play an effect out in the world. The gain is resolved against the
    /// listener when the queue is drained, which is where the listener is
    /// known.
    PlayAt(Sfx, Vec3),
    /// Request a callout. May be refused; see [`arbitrate`].
    Warn(Warning),
    /// Cut off whatever is speaking.
    #[allow(
        dead_code,
        reason = "the EMP that silences the cockpit is not ported yet; \
                  `AudioCommands::stop_warnings` is the caller"
    )]
    StopWarnings,
}

#[allow(
    dead_code,
    reason = "this is the interface the game loop calls, and `audio` is a module \
              of a binary, so rustc cannot see the callers that do not exist \
              yet. The type docs list which ones and why."
)]
impl AudioCommands {
    /// Plays a one-shot at full volume.
    pub fn play(&mut self, sfx: Sfx) {
        self.queue.push(Command::Play(sfx, 1.0));
    }

    /// Plays a one-shot attenuated for its distance from the listener.
    pub fn play_at(&mut self, sfx: Sfx, pos: Vec3) {
        self.queue.push(Command::PlayAt(sfx, pos));
    }

    /// Requests a voice callout.
    ///
    /// Says nothing about whether it will be spoken — [`arbitrate`] decides,
    /// and may refuse on cooldown or priority. The JS returns a bool that no
    /// caller reads; the decision is not knowable synchronously here anyway.
    pub fn warn(&mut self, w: Warning) {
        self.queue.push(Command::Warn(w));
    }

    /// Silences the cockpit mid-sentence. For an EMP, or a scene change.
    pub fn stop_warnings(&mut self) {
        self.queue.push(Command::StopWarnings);
    }
}

// ---------------------------------------------------------------------------
// The plugin
// ---------------------------------------------------------------------------

/// Wires the sound system into the app.
pub struct AudioPlugin;

impl Plugin for AudioPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AudioCommands>()
            .init_resource::<MixerLevels>()
            .init_resource::<PlayThrottle>()
            .init_resource::<Voice>()
            .init_resource::<Listener>()
            .init_resource::<LoopMix>()
            .init_resource::<Unlocked>()
            .add_systems(Startup, load_assets)
            // Events are consumed on the fixed clock, after the tick that
            // produced them. `Frame` is refilled in place every tick, so an
            // `Update` reader would replay a tick's events on a fast display
            // and drop them on a slow one.
            .add_systems(FixedUpdate, watch_sim.after(SimSet))
            // Playback runs on the render clock: the mix damping and the
            // crossfade both want real frame time.
            .add_systems(
                Update,
                (gate_playback, reap_voice, run_commands, drive_loops).chain(),
            );
    }
}

/// Kicks off the load of all 23 clips.
///
/// Indexed through `Enum::ALL` rather than `0..COUNT` so that a variant added
/// without a table row is a compile error rather than an off-by-one that
/// silently plays the wrong sound.
fn load_assets(mut commands: Commands, server: Res<AssetServer>) {
    commands.insert_resource(AudioAssets {
        sfx: Sfx::ALL.map(|s| server.load(SFX[s as usize].path)),
        loops: Loop::ALL.map(|l| server.load(LOOPS[l as usize].path)),
        warnings: Warning::ALL.map(|w| server.load(WARNINGS[w as usize].path)),
    });
}

/// Opens the gate on the first key press or click, and swallows anything
/// requested before then.
///
/// A no-op off the web, where [`Unlocked`] starts true. Dropping rather than
/// deferring the queued commands is deliberate: a gunshot from before the
/// player touched the keyboard is not a sound that is owed, and replaying a
/// second of backlog the instant audio comes up is worse than silence.
fn gate_playback(
    mut unlocked: ResMut<Unlocked>,
    mut queue: ResMut<AudioCommands>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    touches: Res<Touches>,
) {
    if unlocked.0 {
        return;
    }
    let gesture = keys.get_just_pressed().next().is_some()
        || mouse.get_just_pressed().next().is_some()
        || touches.iter_just_pressed().next().is_some();
    if gesture {
        unlocked.0 = true;
    } else {
        queue.queue.clear();
    }
}

// ---------------------------------------------------------------------------
// Simulation → sound
// ---------------------------------------------------------------------------

/// Turns one tick's [`SimEvent`]s and HUD state into queued commands.
///
/// The event mapping, and why each one and not its neighbour:
///
/// | `SimEvent`          | sound                                            |
/// |---------------------|--------------------------------------------------|
/// | `Fired`             | [`Sfx::Shoot`], throttled                        |
/// | `FlareBurst`        | [`Sfx::FlareDeploy`] + [`Warning::Flare`] if mine |
/// | `Damaged`           | [`Sfx::Hitmarker`], only for hits I landed        |
/// | `ShipDestroyed`     | [`Sfx::ShipDeath`]                                |
/// | `AsteroidDestroyed` | [`Sfx::RockBreak`]                                |
/// | `Explosion`         | [`EXPLOSION_SFX`] — impacts only, see there       |
fn watch_sim(
    frame: Res<SimFrame>,
    mut cmds: ResMut<AudioCommands>,
    mut listener: ResMut<Listener>,
    mut edges: Local<WarnEdges>,
) {
    let frame = &frame.0;

    let me: Option<&ShipView> = frame
        .ships
        .iter()
        .find(|s| s.flags.contains(ShipFlags::LOCAL));
    if let Some(me) = me {
        listener.0 = Vec3::from_array(me.pos);
    }
    let local_id = me.map(|s| s.id);
    let mine = |id| Some(id) == local_id;

    for ev in &frame.events {
        match *ev {
            // One clip covers bullets, beams, and missile launches, as in the
            // JS. Your own gun is not distance-attenuated: it is six feet away
            // and `main.js:1550` plays it flat.
            SimEvent::Fired { owner, origin, .. } => {
                if mine(owner) {
                    cmds.play(Sfx::Shoot);
                } else {
                    cmds.play_at(Sfx::Shoot, v3(origin));
                }
            }
            SimEvent::FlareBurst { owner, origin } => {
                if mine(owner) {
                    cmds.play(Sfx::FlareDeploy);
                    cmds.warn(Warning::Flare);
                } else {
                    cmds.play_at(Sfx::FlareDeploy, v3(origin));
                }
            }
            // The hit confirmation, and only for hits I landed — it is a UI
            // cue, not a world sound, so it is flat and unattenuated.
            SimEvent::Damaged { id, source, .. } => {
                if source.is_some_and(mine) && !mine(id) {
                    cmds.play(Sfx::Hitmarker);
                }
            }
            SimEvent::ShipDestroyed { id, pos, .. } => {
                if mine(id) {
                    cmds.play(Sfx::ShipDeath);
                } else {
                    cmds.play_at(Sfx::ShipDeath, v3(pos));
                }
            }
            SimEvent::AsteroidDestroyed { pos, .. } => {
                cmds.play_at(Sfx::RockBreak, v3(pos));
            }
            SimEvent::Explosion { pos, kind, .. } => {
                if let Some(sfx) = explosion_sfx(kind) {
                    cmds.play_at(sfx, v3(pos));
                }
            }
            _ => {}
        }
    }

    // --- the warning conditions ------------------------------------------
    //
    // `main.js:1360`-`:1377`. These decide only *when* a condition is true;
    // `arbitrate` decides whether it gets said. Edge-triggered, because each is
    // a state change rather than a persistent danger — the level-triggered
    // ones are the terrain callouts, which are not reachable from here.
    let hud = &frame.hud;
    let alive = me.is_some_and(|s| s.flags.contains(ShipFlags::ALIVE));
    if alive {
        if hud.missile_lock_warning && !edges.locked {
            cmds.warn(Warning::Lock);
        }
        if hud.hp01 <= HP_WARN_FRAC && edges.hp01 > HP_WARN_FRAC {
            cmds.warn(Warning::Warning);
        }
        if hud.boost01 <= BINGO_FUEL_FRAC && edges.boost01 > BINGO_FUEL_FRAC {
            cmds.warn(Warning::Bingo);
        }
        if hud.overcharge01 > OVERCHARGE_WARN_FRAC && edges.overcharge01 <= OVERCHARGE_WARN_FRAC {
            cmds.warn(Warning::MasterCaution);
        }
        edges.hp01 = hud.hp01;
        edges.boost01 = hud.boost01;
    }
    edges.locked = hud.missile_lock_warning;
    edges.overcharge01 = hud.overcharge01;
}

/// Hull fraction at which the box says "warning". `main.js:1367`.
const HP_WARN_FRAC: f32 = 0.3;
/// Boost fraction at which it calls bingo fuel. `main.js:1369`.
const BINGO_FUEL_FRAC: f32 = 0.2;
/// Where `overcharge01` crosses `BRAKE_OVERCHARGE_WARN`.
///
/// `HudState::overcharge01` normalises the brake-overcharge timer against the
/// delay before it starts doing damage, so the HUD's *warn* threshold sits at
/// the ratio of the two rules. Derived rather than written as `0.5` so that
/// retuning `rules.rs` moves the callout with it.
const OVERCHARGE_WARN_FRAC: f32 = {
    let ship = sim::rules::Rules::DEFAULT.ship;
    (ship.brake_overcharge_warn / ship.brake_overcharge_damage_delay) as f32
};

/// The narrowing at the simulation boundary. `sim` is `f64`; audio is not.
fn v3(v: sim::math::Vec3) -> Vec3 {
    Vec3::new(v.x as f32, v.y as f32, v.z as f32)
}

/// [`EXPLOSION_SFX`] as a lookup.
fn explosion_sfx(kind: ExplosionKind) -> Option<Sfx> {
    EXPLOSION_SFX
        .iter()
        .find(|(k, _)| *k == kind)
        .and_then(|(_, s)| *s)
}

// ---------------------------------------------------------------------------
// Playback
// ---------------------------------------------------------------------------

/// Clears [`Voice::active`] once its entity is gone.
///
/// This is the Bevy shape of the JS `src.onended`. A callout entity carries
/// `PlaybackMode::Despawn`, so bevy removes it when the sink drains; the
/// channel is free again the moment the query stops finding it. An interrupted
/// callout is despawned by [`run_commands`] and lands here the same way.
fn reap_voice(mut voice: ResMut<Voice>, lines: Query<(), With<VoiceLine>>) {
    if let Some(active) = &voice.active {
        if lines.get(active.entity).is_err() {
            voice.active = None;
        }
    }
}

/// Drains [`AudioCommands`] and does the work.
fn run_commands(
    mut commands: Commands,
    mut queue: ResMut<AudioCommands>,
    assets: Option<Res<AudioAssets>>,
    sources: Res<Assets<AudioSource>>,
    levels: Res<MixerLevels>,
    listener: Res<Listener>,
    time: Res<Time>,
    mut throttle: ResMut<PlayThrottle>,
    mut voice: ResMut<Voice>,
    sinks: Query<&AudioSink>,
) {
    // Playback before the gesture gate opens is dropped by `gate_playback`,
    // which leaves the queue empty; nothing to check for here.
    let Some(assets) = assets else {
        queue.queue.clear();
        return;
    };

    let now = time.elapsed_secs();
    let sfx_bus = levels.sfx.clamp(0.0, 1.0);

    for cmd in queue.queue.drain(..) {
        match cmd {
            Command::Play(sfx, gain) => {
                play_one_shot(
                    &mut commands,
                    &assets,
                    &sources,
                    &mut throttle,
                    now,
                    sfx,
                    gain,
                    sfx_bus,
                );
            }
            Command::PlayAt(sfx, pos) => {
                let gain = distance_vol(listener.0, pos);
                if gain > 0.0 {
                    play_one_shot(
                        &mut commands,
                        &assets,
                        &sources,
                        &mut throttle,
                        now,
                        sfx,
                        gain,
                        sfx_bus,
                    );
                }
            }
            Command::Warn(w) => {
                let handle = &assets.warnings[w as usize];
                // An unloaded clip is not a refusal: it must not stamp the
                // cooldown, or the first callout of a session is swallowed and
                // the second is blocked. The JS bails on a missing buffer for
                // the same reason.
                if sources.get(handle).is_none() {
                    continue;
                }
                let active = voice.active.as_ref().map(|a| a.priority);
                match arbitrate(w, now, voice.last[w as usize], active) {
                    Verdict::Cooling | Verdict::Outranked => continue,
                    Verdict::Interrupt => {
                        if let Some(prev) = voice.active.take() {
                            stop(&mut commands, &sinks, prev.entity);
                        }
                    }
                    Verdict::Play => {}
                }
                let entity = commands
                    .spawn((
                        AudioPlayer::new(handle.clone()),
                        PlaybackSettings::DESPAWN
                            .with_volume(Volume::Linear(WARN_VOLUME * sfx_bus)),
                        VoiceLine,
                    ))
                    .id();
                voice.active = Some(ActiveVoice {
                    priority: WARNINGS[w as usize].priority,
                    entity,
                });
                voice.last[w as usize] = now;
            }
            Command::StopWarnings => {
                if let Some(prev) = voice.active.take() {
                    stop(&mut commands, &sinks, prev.entity);
                }
            }
        }
    }
}

/// Spawns one effect, if its throttle allows and its clip has loaded.
fn play_one_shot(
    commands: &mut Commands,
    assets: &AudioAssets,
    sources: &Assets<AudioSource>,
    throttle: &mut PlayThrottle,
    now: f32,
    sfx: Sfx,
    gain: f32,
    bus: f32,
) {
    let i = sfx as usize;
    let def = &SFX[i];
    if sources.get(&assets.sfx[i]).is_none() {
        return;
    }
    if def.throttle > 0.0 {
        if now - throttle.last[i] < def.throttle {
            return;
        }
        throttle.last[i] = now;
    }
    // The JS clamps the per-sound gain before the master fader, not after, so
    // a loud sound cannot borrow headroom from a quiet slider.
    let volume = (def.volume * gain).clamp(0.0, 1.0) * bus;
    commands.spawn((
        AudioPlayer::new(assets.sfx[i].clone()),
        PlaybackSettings::DESPAWN.with_volume(Volume::Linear(volume)),
        OneShot,
    ));
}

/// Stops a clip immediately and drops its entity.
///
/// Despawning alone would do it — rodio's player stops on drop — but calling
/// `stop` first means the sound cuts on this frame rather than whenever the
/// command queue is applied.
fn stop(commands: &mut Commands, sinks: &Query<&AudioSink>, entity: Entity) {
    if let Ok(sink) = sinks.get(entity) {
        sink.stop();
    }
    if let Ok(mut e) = commands.get_entity(entity) {
        e.despawn();
    }
}

/// Keeps the three looping beds running, mixed, and crossfaded.
fn drive_loops(
    mut commands: Commands,
    assets: Option<Res<AudioAssets>>,
    sources: Res<Assets<AudioSource>>,
    frame: Res<SimFrame>,
    levels: Res<MixerLevels>,
    unlocked: Res<Unlocked>,
    time: Res<Time>,
    mut mix: ResMut<LoopMix>,
    mut voices: Query<(&LoopVoice, Option<&mut AudioSink>)>,
) {
    let now = time.elapsed_secs();
    let dt = time.delta_secs();

    // --- the engine mix ---------------------------------------------------
    let me = frame
        .0
        .ships
        .iter()
        .find(|s| s.flags.contains(ShipFlags::LOCAL));
    let alive = me.is_some_and(|s| s.flags.contains(ShipFlags::ALIVE));
    let (mut engine_target, mut boost_target) = (0.0, 0.0);
    if let Some(me) = me.filter(|_| alive) {
        let speed = Vec3::from_array(me.vel).length();
        engine_target = MOVE_MAX_VOL * (speed / SPEED_FOR_FULL_VOL).clamp(0.0, 1.0);
        if me.flags.contains(ShipFlags::BOOSTING) {
            // Duck the bed under the roar rather than letting them sum, or the
            // two together clip.
            engine_target *= MOVE_DUCK_BOOST;
            boost_target = BOOST_MAX_VOL;
        } else if me.flags.contains(ShipFlags::BRAKING) {
            engine_target *= MOVE_DUCK_BRAKE;
        }
    }
    mix.engine = damp(mix.engine, engine_target, MOVE_DAMP, dt);
    mix.boost = damp(mix.boost, boost_target, BOOST_DAMP, dt);

    let sfx_bus = levels.sfx.clamp(0.0, 1.0);
    // In `Loop` order, so it can be indexed by `LoopVoice::which` below. The
    // music is *not* multiplied by the SFX bus — that is the whole point of the
    // JS routing the music straight to the destination.
    let gains = [
        mix.engine.clamp(0.0, 1.0),
        mix.boost.clamp(0.0, 1.0),
        levels.music.clamp(0.0, 1.0),
    ];

    // --- envelopes --------------------------------------------------------
    let mut live = [false; LOOP_COUNT];
    for (voice, sink) in &mut voices {
        live[voice.which] = true;
        let def = &LOOPS[voice.which];
        let bus = if def.on_sfx_bus { sfx_bus } else { 1.0 };
        let level = loop_envelope(now - voice.start, def.secs) * gains[voice.which] * bus;
        if let Some(mut sink) = sink {
            sink.set_volume(Volume::Linear(level));
        }
    }

    // --- the schedule -----------------------------------------------------
    let Some(assets) = assets else {
        return;
    };
    if !unlocked.0 {
        return;
    }
    for bed in Loop::ALL {
        let which = bed as usize;
        if sources.get(&assets.loops[which]).is_none() {
            continue;
        }
        if !mix.running[which] {
            mix.running[which] = true;
            mix.next_start[which] = now;
        }
        // Self-heal. If the clip is shorter than the table says, the bed runs
        // dry before its successor is due; start the next pass now rather than
        // leaving a hole. Costs one seam and nothing else.
        if !live[which] {
            mix.next_start[which] = mix.next_start[which].min(now);
        }
        if now < mix.next_start[which] {
            continue;
        }
        commands.spawn((
            AudioPlayer::new(assets.loops[which].clone()),
            // Silent at t = 0; the envelope above fades it in from the next
            // frame, which is where the crossfade lives.
            PlaybackSettings::DESPAWN.with_volume(Volume::SILENT),
            LoopVoice { which, start: now },
        ));
        // `+=` rather than `= now + cycle`: the schedule is anchored, so a
        // frame that runs late does not push every later pass late with it.
        mix.next_start[which] += LOOPS[which].secs - XFADE;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- tables ------------------------------------------------------------

    /// Every table is indexed by `enum as usize`, so a variant added in the
    /// middle without a matching row would silently shift every sound by one.
    #[test]
    fn the_tables_line_up_with_their_enums() {
        assert_eq!(SFX.len(), SFX_COUNT);
        assert_eq!(LOOPS.len(), LOOP_COUNT);
        assert_eq!(WARNINGS.len(), WARN_COUNT);

        // A spot check at each end of each table.
        assert_eq!(SFX[Sfx::Shoot as usize].path, "sounds/shoot.mp3");
        assert_eq!(
            SFX[Sfx::FlareDeploy as usize].path,
            "sounds/flare_deploy.mp3"
        );
        assert_eq!(LOOPS[Loop::Engine as usize].path, "sounds/move.mp3");
        assert_eq!(LOOPS[Loop::Music as usize].path, "sounds/dumb_Eflatmin.mp3");
        assert_eq!(
            WARNINGS[Warning::PullUp as usize].path,
            "sounds/warnings/pull_up.mp3"
        );
        assert_eq!(
            WARNINGS[Warning::TwsLaunch2 as usize].path,
            "sounds/warnings/tws_launch_2.mp3"
        );
    }

    /// Every explosion kind is classified. A new one defaulting to silence is
    /// fine; a new one nobody thought about is not.
    #[test]
    fn every_explosion_kind_is_classified() {
        for kind in [
            ExplosionKind::Impact,
            ExplosionKind::ShipDeath,
            ExplosionKind::AsteroidBreak,
            ExplosionKind::MissileHit,
            ExplosionKind::FlareBurst,
        ] {
            assert!(
                EXPLOSION_SFX.iter().any(|(k, _)| *k == kind),
                "{kind:?} has no row"
            );
        }
        // The pairs that would otherwise double against a specific event.
        assert_eq!(explosion_sfx(ExplosionKind::AsteroidBreak), None);
        assert_eq!(explosion_sfx(ExplosionKind::ShipDeath), None);
        assert_eq!(explosion_sfx(ExplosionKind::Impact), Some(Sfx::Impact));
    }

    /// The ordering the module docs promise.
    #[test]
    fn ground_proximity_outranks_everything() {
        let pull_up = WARNINGS[Warning::PullUp as usize].priority;
        for (i, def) in WARNINGS.iter().enumerate() {
            if i == Warning::PullUp as usize {
                continue;
            }
            assert!(pull_up > def.priority, "{} outranks pull up", def.path);
        }
    }

    // -- arbitration -------------------------------------------------------

    /// Nothing speaking, never spoken: say it.
    #[test]
    fn a_quiet_cockpit_says_the_callout() {
        assert_eq!(
            arbitrate(Warning::Bingo, 0.0, f32::NEG_INFINITY, None),
            Verdict::Play
        );
    }

    /// "pull up" interrupts "bingo" mid-word.
    #[test]
    fn more_urgent_interrupts() {
        let bingo = WARNINGS[Warning::Bingo as usize].priority;
        assert_eq!(
            arbitrate(Warning::PullUp, 100.0, f32::NEG_INFINITY, Some(bingo)),
            Verdict::Interrupt
        );
    }

    /// ...and never the reverse.
    #[test]
    fn less_urgent_waits_its_turn() {
        let pull_up = WARNINGS[Warning::PullUp as usize].priority;
        assert_eq!(
            arbitrate(Warning::Bingo, 100.0, f32::NEG_INFINITY, Some(pull_up)),
            Verdict::Outranked
        );
    }

    /// Equal urgency loses, deliberately: two callouts of the same weight
    /// talking over each other is exactly the noise this system exists to
    /// prevent. `lock` and `rwr_lock` are both 70.
    #[test]
    fn equal_urgency_does_not_double_up() {
        let lock = WARNINGS[Warning::Lock as usize].priority;
        assert_eq!(WARNINGS[Warning::RwrLock as usize].priority, lock);
        assert_eq!(
            arbitrate(Warning::RwrLock, 100.0, f32::NEG_INFINITY, Some(lock)),
            Verdict::Outranked
        );
    }

    /// The cooldown is checked before priority, so the most urgent callout in
    /// the box still cannot nag.
    #[test]
    fn the_cooldown_beats_the_priority() {
        let cd = WARNINGS[Warning::PullUp as usize].cooldown;
        // Just inside the cooldown, with nothing else speaking.
        assert_eq!(
            arbitrate(Warning::PullUp, 10.0, 10.0 - cd * 0.5, None),
            Verdict::Cooling
        );
        // ...and just outside it.
        assert_eq!(
            arbitrate(Warning::PullUp, 10.0, 10.0 - cd - 0.01, None),
            Verdict::Play
        );
    }

    /// A canyon run: forty requests over four seconds yield a handful of
    /// callouts, not forty. This is the failure the cooldown table exists to
    /// prevent, so it is worth testing end to end rather than one call at a
    /// time.
    #[test]
    fn a_held_condition_does_not_nag() {
        let mut last = f32::NEG_INFINITY;
        let mut spoken = 0;
        for step in 0..240 {
            let now = step as f32 / 60.0;
            if arbitrate(Warning::PullUp, now, last, None) == Verdict::Play {
                spoken += 1;
                last = now;
            }
        }
        // 4 s at a 1.6 s cooldown.
        assert_eq!(spoken, 3);
    }

    // -- distance ----------------------------------------------------------

    #[test]
    fn distance_attenuation_matches_the_js_curve() {
        let ear = Vec3::ZERO;
        assert_eq!(distance_vol(ear, Vec3::ZERO), 1.0);
        assert_eq!(distance_vol(ear, Vec3::X * SFX_NEAR_DIST), 1.0);
        assert_eq!(distance_vol(ear, Vec3::X * SFX_FAR_DIST), 0.0);
        assert_eq!(distance_vol(ear, Vec3::X * 5000.0), 0.0);

        // Halfway between the two radii the JS returns u * u with u = 0.5.
        let mid = (SFX_NEAR_DIST + SFX_FAR_DIST) / 2.0;
        assert!((distance_vol(ear, Vec3::X * mid) - 0.25).abs() < 1e-5);

        // Monotone, which the squared curve makes easy to get backwards.
        let mut prev = 1.0;
        for i in 0..100 {
            let d = i as f32 * 10.0;
            let v = distance_vol(ear, Vec3::X * d);
            assert!(v <= prev, "gain rose with distance at {d}");
            prev = v;
        }
    }

    // -- loops -------------------------------------------------------------

    #[test]
    fn the_loop_envelope_fades_both_ends() {
        let dur = 10.0;
        assert_eq!(loop_envelope(0.0, dur), 0.0);
        assert_eq!(loop_envelope(dur, dur), 0.0);
        assert_eq!(loop_envelope(dur / 2.0, dur), 1.0);
        assert!((loop_envelope(XFADE, dur) - 1.0).abs() < 1e-6);
        // Symmetric: the ramps are the same shape at both ends.
        for i in 1..8 {
            let t = i as f32 * XFADE / 8.0;
            let (up, down) = (loop_envelope(t, dur), loop_envelope(dur - t, dur));
            assert!((up - down).abs() < 1e-5, "asymmetric at {t}");
        }
    }

    /// The point of the crossfade: across the seam the two passes sum to unity,
    /// so there is no dip and no doubling.
    #[test]
    fn the_crossfade_sums_to_one_across_the_seam() {
        let dur = 10.0;
        let cycle = dur - XFADE;
        for i in 0..=16 {
            let t = i as f32 * XFADE / 16.0;
            // The outgoing pass at `cycle + t`, the incoming one `t` in.
            let sum = loop_envelope(cycle + t, dur) + loop_envelope(t, dur);
            assert!((sum - 1.0).abs() < 1e-5, "sum {sum} at seam offset {t}");
        }
    }

    /// Every bed is long enough for the fixed 80 ms crossfade to be the
    /// `min(0.08, dur * 0.1)` the JS computes.
    #[test]
    fn every_bed_is_long_enough_to_crossfade() {
        for def in &LOOPS {
            assert!(
                def.secs * 0.1 >= XFADE,
                "{} is {} s; the JS would shorten the crossfade",
                def.path,
                def.secs
            );
        }
    }

    // -- mix ---------------------------------------------------------------

    #[test]
    fn damping_converges_without_overshooting() {
        let mut v = 0.0;
        for _ in 0..600 {
            v = damp(v, 1.0, MOVE_DAMP, 1.0 / 60.0);
            assert!((0.0..=1.0).contains(&v), "overshot to {v}");
        }
        assert!(v > 0.999, "did not converge, reached {v}");
    }

    /// A big frame must not overshoot either — the exponential form is chosen
    /// precisely so it cannot, unlike a naive `lerp(a, b, k * dt)`.
    #[test]
    fn damping_survives_a_stalled_frame() {
        let v = damp(0.0, 1.0, BOOST_DAMP, 5.0);
        assert!((0.0..=1.0).contains(&v), "overshot to {v} on a 5 s frame");
    }

    #[test]
    fn the_overcharge_threshold_tracks_the_rules() {
        let ship = sim::rules::Rules::DEFAULT.ship;
        assert!(ship.brake_overcharge_warn < ship.brake_overcharge_damage_delay);
        assert!((0.0..1.0).contains(&OVERCHARGE_WARN_FRAC));
    }
}
