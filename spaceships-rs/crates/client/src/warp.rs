//! Warp-in on spawn — the star tunnel and the FOV punch, on every spawn.
//!
//! A port of `public/src/warp.js`, which in the JS client fires only on a
//! campaign respawn. Two things change and nothing else does: it fires on
//! **every** spawn, and remote ships get one of their own.
//!
//! # What it is
//!
//! Streaking stars past the camera, and a field of view that starts at 175° and
//! decelerates into the camera's resting angle over 1.5 s. The punch does most
//! of the work — `1 - (1 - progress)^6` spends three quarters of it in the
//! first quarter of the time, which is what reads as falling out of warp rather
//! than as a zoom.
//!
//! Every number here is `warp.js`'s: 3 000 stars, a 10–210 unit annulus, a
//! 1 000 unit tunnel, velocities of 2 000–5 000 scaled by a `2.0 → 0.05`
//! multiplier, streak length `speed × 0.5 + 20`, and the opacity envelope that
//! fades in over the first 20 % and out over the last 0.8 s. Where something
//! deviates it says so at the constant.
//!
//! # What fires it
//!
//! Two sources, both read off [`SimFrame`] in `FixedUpdate` — see
//! [`Warps::observe`]:
//!
//! - `SimEvent::ShipRespawned`, which the simulation already emits and
//!   `scene.rs` already consumes to snap interpolation.
//! - A ship id appearing in `Frame::ships` that was not there last tick. The
//!   event covers respawns only; this covers the match's first tick and a
//!   mid-match join, which have no event because nothing *re*-spawned.
//!
//! # It covers the invulnerability window
//!
//! [`DURATION`] is `warp.js`'s 1.5 s against `combat.spawn_invuln` of 2.0 s,
//! and a compile-time assertion holds it there. Spawn protection is a rule the
//! player is given no feedback about at all today, and an arrival that is
//! visibly still resolving is that feedback — but only if it ends first. It
//! must never promise a safety that has already expired.
//!
//! # Remote ships arrive too
//!
//! `warp.js` is a first-person effect: the tunnel is parented to the camera and
//! nobody else can see it. Watching an enemy fall out of warp at its own
//! position is worth more than having it appear, so a remote arrival gets the
//! same tunnel — same geometry, same curves — anchored to *that ship* and
//! scaled down to its own length scale, streaming along its nose. Nothing
//! screen-space: the FOV punch is the local player's alone, because a remote
//! ship arriving must not move the camera it is being watched through.
//!
//! # Draw cost
//!
//! One entity, one mesh, one material, rebuilt each frame — the shape
//! `weapons.rs` documents at length and for the same reason. `warp.js` uses an
//! `InstancedMesh` to get the same single draw call; a rebuilt vertex buffer
//! gets there too and is the form that also works on WebGL2 without a second
//! code path. Nothing here is stateful: a star's position is a closed-form
//! function of its index and the arrival's age, so 3 000 of them cost three
//! thousand hashes and no memory at all.
//!
//! # Triggering it on demand
//!
//! `SPACESHIPS_WARP=<secs>` fires a local arrival at that elapsed time, and
//! `SPACESHIPS_WARP_REMOTE=<secs>` fires one for every *other* ship in the
//! match. Both accept a comma-separated list. They exist because the real
//! trigger is the match's first tick, which is over long before
//! `SPACESHIPS_SCREENSHOT`'s three-second settle — so without them the effect
//! cannot be captured at all. They are split local/remote rather than
//! local/everyone because the local tunnel fills frame and would hide the
//! remote one entirely.
//!
//! # Not done here
//!
//! `BACKLOG.md` also asks for a rising whoosh that cuts hard at arrival. Audio
//! lives in `audio.rs`, which is not this module's file; the trigger it would
//! need is [`Warps::live`], which is already public to this crate.

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::image::{ImageSampler, ImageSamplerDescriptor};
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use sim::world::{EntityId, Frame, ShipFlags, SimEvent};
use spaceships_sim as sim;

use crate::camera::BASE_FOV;
use crate::sim_bridge::{pos as to_vec3, rot, SimFrame, SimSet, LOCAL_ID};

/// The rules, so the invulnerability window below is *derived* rather than
/// guessed at — same reasoning as `scene.rs` and `weapons.rs`.
const RULES: sim::rules::Rules = sim::rules::Rules::DEFAULT;

// ---------------------------------------------------------------------------
// warp.js, as constants
// ---------------------------------------------------------------------------

/// `WARP_DURATION`. How long an arrival lasts, in seconds.
pub const DURATION: f32 = 1.5;

/// The effect must not outlast the protection it is describing.
///
/// A compile-time assertion rather than only a test, because the failure mode —
/// a player who trusts the effect and dies to a shot that was always legal — is
/// not something anyone would connect back to a number in this file.
const _: () = assert!((DURATION as f64) <= RULES.combat.spawn_invuln);

/// `starCount`.
const STAR_COUNT: u32 = 3000;

/// `radius = 10 + Math.random() * 200`: the annulus the tunnel's stars sit in.
const RADIUS_MIN: f32 = 10.0;
/// The width of that annulus.
const RADIUS_SPAN: f32 = 200.0;

/// `z = (Math.random() - 0.5) * 1000`: how deep the tunnel is.
const TUNNEL_DEPTH: f32 = 1000.0;

/// `if (posZ[i] > 100) posZ[i] -= 1000`: how far past the viewer a star runs
/// before it wraps back to the far end.
const WRAP_AHEAD: f32 = 100.0;

/// `velocities[i] = 2000 + Math.random() * 3000`.
const VEL_MIN: f32 = 2000.0;
/// The spread on that.
const VEL_SPAN: f32 = 3000.0;

/// `speedMult = lerp(2.0, 0.05, progress)` — the tunnel decelerating to a halt,
/// in step with the FOV.
const SPEED_MULT_START: f32 = 2.0;
/// The other end of it.
const SPEED_MULT_END: f32 = 0.05;

/// `scale.z = max(1, speed * 0.5 + 20)`: the constant term.
const LENGTH_BASE: f32 = 20.0;
/// And the term that tracks how far the star moves per frame.
const LENGTH_PER_SPEED: f32 = 0.5;

/// The frame time `warp.js`'s streak length is implicitly written against.
///
/// `speed` there is `velocity * dt * speedMult` — a *per-frame* distance — and
/// it is then used to size the box. So the JS draws longer streaks on a slower
/// machine, which is a bug rather than an intent. Pinning the reference frame
/// time reproduces the 60 fps look on every display instead of the 60 fps look
/// only at 60 fps.
const REF_DT: f32 = 1.0 / 60.0;

/// `progress < 0.2 ? progress / 0.2 : 1.0`: the fade in, as a fraction of the
/// whole arrival.
const FADE_IN: f32 = 0.2;

/// `warpTimer < 0.8 ? warpTimer / 0.8`: the fade out, in seconds of remaining
/// time.
const FADE_OUT: f32 = 0.8;

/// `maxFov`, in radians. Decelerating out of 175° is the "poosh", and it is the
/// part of the effect that does most of the work.
const MAX_FOV: f32 = 175.0 * std::f32::consts::PI / 180.0;

/// `BoxGeometry(0.4, 0.4, 1)`: half the width of a streak, in world units.
const STAR_HALF_WIDTH: f32 = 0.2;

/// The narrowest a streak may get on screen, as a half-width in pixels.
///
/// Not in `warp.js`, and the single change that makes this port read like the
/// original rather than like a handful of stray lines. 0.2 world units at the
/// far end of a 1 000-unit tunnel is about a *third* of a pixel: the rasteriser
/// covers a fraction of each pixel it crosses, so the streak comes out dashed
/// and at a third of the brightness it was authored at, and most of the tunnel
/// simply disappears. Three.js has exactly the same problem, and it is why the
/// JS effect looks sparse behind its 3 000 boxes.
///
/// Widening in *world* units instead would fix the far end and turn the near
/// end into slabs. Widening to a floor in *screen* units is the standard trick
/// for lines and trails, costs one length and one divide per star, and keeps
/// the authored 0.4 wherever that is already more than this.
const STAR_MIN_HALF_PX: f32 = 0.85;

// -- the parts that are not in warp.js --------------------------------------

/// How long the arrival flash lasts, in seconds.
///
/// Not in `warp.js`, which is a *transition* — it cuts from one scene to
/// another and has no instant to punctuate. A spawn does, and the tunnel's
/// opacity envelope deliberately starts at zero, so without this the first
/// 0.3 s of an arrival is nothing at all.
const FLASH_TIME: f32 = 0.20;

/// The flash's radius at its widest, in world units. About a ship's length.
const FLASH_RADIUS: f32 = 9.0;

/// How much of a remote ship's tunnel fits inside the local one.
///
/// The local tunnel is 1 000 units deep and 420 across because it wraps the
/// camera. Anchored on a ship instead it has to read as *that ship's* arrival
/// from across the map, so radius, depth, velocity and streak length all take
/// this factor together — the shape is identical, the scale is not.
const REMOTE_SCALE: f32 = 0.16;

/// Stars in a remote ship's tunnel. Fewer than [`STAR_COUNT`] in proportion to
/// the volume they fill, so the density matches.
const REMOTE_STARS: u32 = 260;

// ---------------------------------------------------------------------------
// Curves
// ---------------------------------------------------------------------------

/// Every timeline curve, as plain functions of age in seconds.
///
/// Free functions rather than methods so the tests can pin the shape without
/// building a `World` — which matters more than usual here, because "does the
/// FOV land back exactly where it started" is the difference between an effect
/// and a permanent change to how the game looks.
pub mod curves {
    use super::{
        DURATION, FADE_IN, FADE_OUT, FLASH_TIME, LENGTH_BASE, LENGTH_PER_SPEED, MAX_FOV, REF_DT,
        SPEED_MULT_END, SPEED_MULT_START,
    };

    /// How far through the arrival, clamped to `0..=1`.
    #[must_use]
    pub fn progress(age: f32) -> f32 {
        if DURATION <= 0.0 {
            1.0
        } else {
            (age / DURATION).clamp(0.0, 1.0)
        }
    }

    /// `warp.js`'s opacity envelope: in over the first [`FADE_IN`] of the
    /// arrival, out over the last [`FADE_OUT`] seconds, full in between.
    ///
    /// It starts at **zero**, which is the JS's shape and is kept: the tunnel
    /// arriving already at full brightness is the version that looks like a
    /// cut.
    #[must_use]
    pub fn opacity01(age: f32) -> f32 {
        let timer = (DURATION - age).max(0.0);
        let progress = progress(age);
        if timer < FADE_OUT {
            (timer / FADE_OUT).clamp(0.0, 1.0)
        } else if progress < FADE_IN {
            progress / FADE_IN
        } else {
            1.0
        }
    }

    /// `speedMult = lerp(2.0, 0.05, progress)`.
    #[must_use]
    pub fn speed_mult(age: f32) -> f32 {
        let t = progress(age);
        SPEED_MULT_START * (1.0 - t) + SPEED_MULT_END * t
    }

    /// How far a star of unit velocity has travelled by `age`, in world units.
    ///
    /// `warp.js` integrates `velocity * dt * speedMult` frame by frame. This is
    /// the same integral in closed form, `∫₀^age speedMult(t) dt`, which is
    /// what lets a star's position be recomputed from its index instead of
    /// stored — no per-star state anywhere in this module.
    #[must_use]
    pub fn travel(age: f32) -> f32 {
        let t = age.clamp(0.0, DURATION);
        SPEED_MULT_START * t + (SPEED_MULT_END - SPEED_MULT_START) * t * t / (2.0 * DURATION)
    }

    /// `scale.z = max(1, speed * 0.5 + 20)`, for a star of this velocity.
    ///
    /// `scale` multiplies the whole tunnel, so a remote ship's shorter streaks
    /// stay in proportion to its smaller annulus.
    #[must_use]
    pub fn streak_len(velocity: f32, age: f32, scale: f32) -> f32 {
        let per_frame = velocity * speed_mult(age) * REF_DT;
        (per_frame * LENGTH_PER_SPEED + LENGTH_BASE * scale).max(scale)
    }

    /// The arrival flash: 1 at t=0, 0 at [`FLASH_TIME`], cubic decay.
    ///
    /// Cubic rather than quadratic so it is gone rather than lingering as a
    /// haze: its whole job is to punctuate one instant, and anything still
    /// visible a fifth of a second later is drawing attention to itself.
    #[must_use]
    pub fn flash01(age: f32) -> f32 {
        if FLASH_TIME <= 0.0 {
            return 0.0;
        }
        let q = 1.0 - (age / FLASH_TIME).clamp(0.0, 1.0);
        q * q * q
    }

    /// The FOV punch, in radians: [`MAX_FOV`] at t=0, `base` at [`DURATION`].
    ///
    /// `warp.js:59` — `fovProgress = 1 - (1 - progress)^6`, i.e. nearly all of
    /// the punch is spent in the first third and the last of it crawls in. That
    /// asymmetry is the whole trick: a linear ramp reads as a zoom, and this
    /// reads as deceleration.
    #[must_use]
    pub fn fov(age: f32, base: f32) -> f32 {
        let q = 1.0 - progress(age);
        let punch = 1.0 - q * q * q * q * q * q;
        // The precise form of a lerp, not `a + (b - a) * t`: this one is
        // *exactly* `MAX_FOV` at 0 and *exactly* `base` at 1, and landing back
        // on the resting angle bit for bit is what stops a warp from leaving
        // the camera a rounding error away from where it found it.
        MAX_FOV * (1.0 - punch) + base * punch
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// One ship's arrival, mid-flight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Arrival {
    /// Which ship. Matches `sim::world::ShipView::id`.
    pub id: EntityId,
    /// Where it came in. World space, and **fixed**: the ship flies on but the
    /// tunnel it fell out of stays where it was.
    pub pos: Vec3,
    /// Which way its nose pointed when it arrived, which is the axis a remote
    /// tunnel streams along. The local tunnel uses the camera's instead, as
    /// `warp.js` does.
    pub dir: Vec3,
    /// Seconds since the arrival instant.
    pub age: f32,
    /// Whether this is the player on this machine, and therefore whether the
    /// FOV punch runs.
    pub local: bool,
    /// Per-arrival scatter seed, so two ships arriving on the same tick do not
    /// produce the same star field.
    seed: u32,
}

impl Arrival {
    /// Still running.
    #[must_use]
    pub fn alive(&self) -> bool {
        self.age < DURATION
    }
}

/// Every arrival currently on screen, and the id set that detects new ones.
#[derive(Resource, Default)]
pub struct Warps {
    /// In arrival order. Short — at most one per ship in the match.
    pub live: Vec<Arrival>,
    /// Ship ids present on the previous tick. A `Vec` rather than a set: this
    /// is ten entries, and a linear scan over ten `i32`s beats hashing them.
    seen: Vec<EntityId>,
    /// The FOV to put back when the local arrival finishes, captured when it
    /// starts. See [`drive_fov`].
    base_fov: Option<f32>,
    /// Counter folded into each [`Arrival::seed`].
    next_seed: u32,
}

impl Warps {
    /// The local player's arrival, if one is running.
    #[must_use]
    pub fn local(&self) -> Option<&Arrival> {
        self.live.iter().find(|a| a.local)
    }

    /// Reads one tick and starts an arrival for anything that just spawned.
    ///
    /// Both routes in one place because they must not double-fire: a respawning
    /// ship never leaves `Frame::ships`, so it can only ever match the event
    /// path, and a ship joining in progress has no event so it can only ever
    /// match the new-id path.
    pub fn observe(&mut self, frame: &Frame) {
        for event in &frame.events {
            if let SimEvent::ShipRespawned { id, pos } = *event {
                let at = Vec3::new(pos.x as f32, pos.y as f32, pos.z as f32);
                let dir = facing(frame, id);
                self.begin(id, at, dir);
            }
        }

        for ship in &frame.ships {
            // Boss hitboxes are ships so that one damage path can serve the
            // capital ship. They are never drawn, so they never arrive.
            if ship.flags.contains(ShipFlags::BOSS_HITBOX) {
                continue;
            }
            if !self.seen.contains(&ship.id) {
                self.begin(ship.id, to_vec3(ship.pos), rot(ship.quat) * Vec3::Z);
            }
        }

        self.seen.clear();
        self.seen.extend(frame.ships.iter().map(|s| s.id));
    }

    /// Starts an arrival, replacing any this ship already had.
    ///
    /// Replacing rather than stacking: a ship that somehow spawns twice in
    /// quick succession should restart its warp, not run two of them at
    /// different ages and double every additive quad it draws.
    pub fn begin(&mut self, id: EntityId, pos: Vec3, dir: Vec3) {
        self.next_seed = self.next_seed.wrapping_add(0x9E37_79B9);
        let arrival = Arrival {
            id,
            pos,
            dir: dir.normalize_or(Vec3::Z),
            age: 0.0,
            local: id == LOCAL_ID,
            seed: self.next_seed ^ (id as u32),
        };
        match self.live.iter_mut().find(|a| a.id == id) {
            Some(slot) => *slot = arrival,
            None => self.live.push(arrival),
        }
    }

    /// Ages every arrival and drops the finished ones.
    pub fn advance(&mut self, dt: f32) {
        for a in &mut self.live {
            a.age += dt;
        }
        self.live.retain(Arrival::alive);
    }
}

/// One ship's nose direction in the current tick, or `+z` if it is not there.
///
/// `SimEvent::ShipRespawned` carries a position and no orientation, and the
/// respawn phase has already written the new pose into `Frame::ships` by the
/// time the event is read — so this finds it rather than guessing.
fn facing(frame: &Frame, id: EntityId) -> Vec3 {
    frame
        .ships
        .iter()
        .find(|s| s.id == id)
        .map_or(Vec3::Z, |s| rot(s.quat) * Vec3::Z)
}

// ---------------------------------------------------------------------------
// The plugin
// ---------------------------------------------------------------------------

/// Wires the arrival effect: one resource, one mesh, one projection driver.
pub struct WarpPlugin;

impl Plugin for WarpPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Warps>()
            .add_systems(Startup, setup)
            // Spawns are *tick* state: an event happens on a tick and an id
            // appears on a tick. Watching from `Update` would miss one of two
            // spawns on a frame that ran two ticks, and `Frame::events` is
            // overwritten wholesale by the next one.
            .add_systems(FixedUpdate, watch_spawns.after(SimSet))
            // Ageing is per *frame*, so the curves are smooth on a display that
            // is not the tick rate.
            .add_systems(Update, (fire_on_cue, advance).chain())
            // After transform propagation, so the tunnel is anchored to where
            // the camera actually ended up this frame rather than to where it
            // was last frame. `camera::follow` writes the chase pose in
            // `PostUpdate` before `Propagate`.
            .add_systems(
                PostUpdate,
                (drive_fov, build_surface).after(TransformSystems::Propagate),
            );
    }
}

/// Copies this tick's spawns into [`Warps`].
fn watch_spawns(frame: Res<SimFrame>, mut warps: ResMut<Warps>) {
    warps.observe(&frame.0);
}

/// Ages every arrival.
fn advance(time: Res<Time>, mut warps: ResMut<Warps>) {
    warps.advance(time.delta_secs());
}

/// `SPACESHIPS_WARP` / `SPACESHIPS_WARP_ALL`: fire an arrival on cue.
///
/// The match's real first tick is at `t = 0`, which is three seconds before
/// `main.rs`'s automatic screenshot and long gone. Without a way to re-fire on
/// demand this effect is untestable by eye, which for a purely visual feature
/// means untestable.
fn fire_on_cue(
    time: Res<Time>,
    frame: Res<SimFrame>,
    mut warps: ResMut<Warps>,
    mut cues: Local<Option<Cues>>,
) {
    let cues = cues.get_or_insert_with(Cues::from_env);
    if cues.is_empty() {
        return;
    }
    let now = time.elapsed_secs();

    while cues.local.first().is_some_and(|at| now >= *at) {
        cues.local.remove(0);
        let pos = ship_pos(&frame.0, LOCAL_ID).unwrap_or(Vec3::ZERO);
        let dir = facing(&frame.0, LOCAL_ID);
        warps.begin(LOCAL_ID, pos, dir);
    }

    while cues.remote.first().is_some_and(|at| now >= *at) {
        cues.remote.remove(0);
        for ship in &frame.0.ships {
            // Everyone *but* the local player, so the remote effect can be
            // watched on its own rather than through the local tunnel, which
            // fills frame and hides it completely.
            if ship.id == LOCAL_ID || ship.flags.contains(ShipFlags::BOSS_HITBOX) {
                continue;
            }
            warps.begin(ship.id, to_vec3(ship.pos), rot(ship.quat) * Vec3::Z);
        }
    }
}

/// Parsed `SPACESHIPS_WARP*`, in ascending order.
#[derive(Default)]
struct Cues {
    local: Vec<f32>,
    remote: Vec<f32>,
}

impl Cues {
    fn from_env() -> Cues {
        let mut cues = Cues {
            local: times("SPACESHIPS_WARP"),
            remote: times("SPACESHIPS_WARP_REMOTE"),
        };
        cues.local.sort_by(f32::total_cmp);
        cues.remote.sort_by(f32::total_cmp);
        if !cues.is_empty() {
            info!(
                "warp cues: local {:?}, remote {:?}",
                cues.local, cues.remote
            );
        }
        cues
    }

    fn is_empty(&self) -> bool {
        self.local.is_empty() && self.remote.is_empty()
    }
}

/// A comma-separated list of seconds from an environment variable.
///
/// `std::env::var` returns `Err` on `wasm32-unknown-unknown` rather than
/// failing to compile, so this needs no `cfg` — same as `MatchSetup::from_env`.
fn times(key: &str) -> Vec<f32> {
    std::env::var(key)
        .unwrap_or_default()
        .split(',')
        .filter_map(|s| s.trim().parse::<f32>().ok())
        .collect()
}

/// One ship's position in the current tick.
fn ship_pos(frame: &Frame, id: EntityId) -> Option<Vec3> {
    frame
        .ships
        .iter()
        .find(|s| s.id == id)
        .map(|s| to_vec3(s.pos))
}

// ---------------------------------------------------------------------------
// The punch
// ---------------------------------------------------------------------------

/// Drives the FOV punch, for the local player's arrival only.
///
/// # The FOV, and `cockpit.rs`
///
/// The resting FOV is captured when the arrival starts and written back when it
/// finishes, rather than read from [`BASE_FOV`], because `cockpit.rs` swaps the
/// projection for the seated profile on `V` and this must land back on whatever
/// it found. `cockpit.rs`'s own comment notes that "`warp.js` restores a
/// captured `baseFov` behind its back, and this client has no such thing" — it
/// does now, and this is it. The remaining seam is a view toggle *during* an
/// arrival, which would snapshot a punched FOV as the third-person one; a 1.5 s
/// window that also happens to be spawn protection is not where anyone changes
/// seats.
fn drive_fov(mut warps: ResMut<Warps>, mut cam: Query<&mut Projection, With<Camera3d>>) {
    let Ok(mut projection) = cam.single_mut() else {
        return;
    };

    let Some(age) = warps.local().map(|a| a.age) else {
        // Nothing arriving. Put back what the last arrival moved, exactly once
        // — writing every frame would fight `cockpit.rs` for the projection
        // forever.
        if let Some(base) = warps.base_fov.take() {
            set_fov(&mut projection, base);
        }
        return;
    };

    let base = *warps
        .base_fov
        .get_or_insert_with(|| current_fov(&projection).unwrap_or(BASE_FOV));
    set_fov(&mut projection, curves::fov(age, base));
}

/// The camera's current vertical FOV, if it is a perspective camera.
fn current_fov(projection: &Projection) -> Option<f32> {
    match projection {
        Projection::Perspective(p) => Some(p.fov),
        _ => None,
    }
}

/// Writes the vertical FOV, ignoring a camera that has no such thing.
fn set_fov(projection: &mut Projection, fov: f32) {
    if let Projection::Perspective(p) = &mut *projection {
        // glam's perspective matrix requires `0 < fov < PI` and panics
        // otherwise. `MAX_FOV` is 175°, close enough to the limit that a future
        // edit to it deserves a guard rather than a crash.
        p.fov = fov.clamp(0.01, std::f32::consts::PI - 0.01);
    }
}

// ---------------------------------------------------------------------------
// The tunnel
// ---------------------------------------------------------------------------

/// How far past 1.0 a star is authored.
///
/// `graphics.js` multiplies every additive material by a `glowBoost` of 1.7 so
/// the bloom prefilter (threshold 0.9, `camera.rs`) has something to find, and
/// `weapons.rs` follows it. 1.25 here instead: at 1.7 all three channels
/// saturate and three thousand streaks come out as a sheet of white, which
/// loses both `0xccffff`'s cyan and the sense of individual lines. This clears
/// the threshold and keeps the colour.
const STAR_GLOW: f32 = 1.25;

/// `warp.js`'s star colour, `0xccffff`.
const STAR_TINT: LinearRgba = LinearRgba::rgb(0.80 * STAR_GLOW, STAR_GLOW, STAR_GLOW);

/// The flash, hot enough to read as white.
const FLASH_TINT: LinearRgba = LinearRgba::rgb(2.4, 2.9, 3.2);

/// The one mesh every arrival draws into.
#[derive(Resource)]
struct WarpAssets {
    mesh: Handle<Mesh>,
}

/// Marks the single rendered entity, so a future system can find it.
#[derive(Component)]
struct WarpSurface;

/// A horizontal cell of the brush atlas. One material means one texture, so the
/// two shapes this module needs share an image and are picked by UV — the same
/// arrangement `weapons.rs` uses, and for the same reason.
#[derive(Clone, Copy)]
struct Brush {
    u0: f32,
    u1: f32,
}

impl Brush {
    /// A soft radial falloff: the arrival flash.
    const GLOW: Brush = Brush { u0: 0.0, u1: 0.5 };
    /// A near-solid bar with soft ends: a star streak, which in `warp.js` is a
    /// box with a hard edge.
    const CORE: Brush = Brush { u0: 0.5, u1: 1.0 };
}

/// Atlas cell size. Both brushes are smooth ramps magnified far past 1:1.
const ATLAS_CELL: u32 = 64;

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    // `RenderAssetUsages::default()` is both worlds: the main-world copy has to
    // survive because `build_surface` rewrites it every frame.
    let mesh = meshes.add(Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    ));

    let material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: Some(images.add(build_brush_atlas())),
        // `MeshBasicMaterial` in the JS. Emitters, not lit surfaces; `unlit`
        // also short-circuits the PBR block, so a vertex colour past 1.0
        // reaches the framebuffer intact and clears the bloom threshold.
        unlit: true,
        // `THREE.AdditiveBlending` with `depthWrite: false`. Bevy's
        // `AlphaMode::Add` is a premultiplied-alpha blend with the destination
        // alpha zeroed, which is additive and therefore order-independent — the
        // one blend mode a single unsorted mesh can use correctly.
        alpha_mode: AlphaMode::Add,
        cull_mode: None,
        double_sided: true,
        ..default()
    });

    commands.spawn((
        Mesh3d(mesh.clone()),
        MeshMaterial3d(material),
        // Bevy computes an `Aabb` when a mesh is *added*, not when its contents
        // change under it. Without this the effect would be culled against the
        // empty buffer it was created with and never draw.
        NoFrustumCulling,
        NotShadowCaster,
        WarpSurface,
    ));

    commands.insert_resource(WarpAssets { mesh });
}

/// Bakes the two brushes into one 128×64 RGBA image.
///
/// The shape rides entirely in **alpha** with RGB left white: `AlphaMode::Add`
/// premultiplies in the shader, so a fragment contributes
/// `vertex_colour.rgb × vertex_colour.a × texture.a` — colour and brightness
/// from the vertex, silhouette from the texture.
fn build_brush_atlas() -> Image {
    let w = ATLAS_CELL * 2;
    let h = ATLAS_CELL;
    let mut data = vec![0u8; (w * h * 4) as usize];

    for y in 0..h {
        for x in 0..w {
            let cell = x / ATLAS_CELL;
            let cx = (x % ATLAS_CELL) as f32 / (ATLAS_CELL - 1) as f32 * 2.0 - 1.0;
            let cy = y as f32 / (h - 1) as f32 * 2.0 - 1.0;
            let r = (cx * cx + cy * cy).sqrt();
            let a = if cell == 0 {
                // A soft falloff, zero at the cell edge so linear filtering
                // cannot bleed one brush into the other.
                (1.0 - r).clamp(0.0, 1.0).powf(2.2)
            } else {
                // A solid disc with a soft rim: as close to the JS's hard-edged
                // box as a textured quad gets without aliasing.
                (1.0 - ((r - 0.6) / 0.4)).clamp(0.0, 1.0)
            };
            let i = ((y * w + x) * 4) as usize;
            data[i] = 255;
            data[i + 1] = 255;
            data[i + 2] = 255;
            data[i + 3] = (a * 255.0) as u8;
        }
    }

    let mut image = Image::new(
        Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        // Alpha is never sRGB, and the RGB is a constant 1.0, so the linear
        // format is both correct and one fewer conversion.
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor::linear());
    image
}

/// The vertex buffers, reused between frames.
#[derive(Default)]
struct MeshBuild {
    pos: Vec<[f32; 3]>,
    uv: Vec<[f32; 2]>,
    color: Vec<[f32; 4]>,
    idx: Vec<u32>,
}

impl MeshBuild {
    fn clear(&mut self) {
        self.pos.clear();
        self.uv.clear();
        self.color.clear();
        self.idx.clear();
    }

    /// One quad, from a centre and two half-extent vectors.
    fn quad(&mut self, c: Vec3, a: Vec3, b: Vec3, brush: Brush, tint: LinearRgba, alpha: f32) {
        if alpha <= 0.0 {
            return;
        }
        let base = self.pos.len() as u32;
        for (sa, sb) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            let p: Vec3 = c + a * sa + b * sb;
            self.pos.push([p.x, p.y, p.z]);
            self.color
                .push([tint.red, tint.green, tint.blue, alpha.min(1.0)]);
        }
        let (u0, u1) = (brush.u0, brush.u1);
        self.uv.push([u0, 1.0]);
        self.uv.push([u1, 1.0]);
        self.uv.push([u1, 0.0]);
        self.uv.push([u0, 0.0]);
        self.idx
            .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    /// A billboarded bar along `dir`, `half_len` long and `half_w` wide.
    ///
    /// `warp.js` draws a real box; this is the same silhouette for a sixth of
    /// the vertices, and a 0.4-unit-thick box seen from a distance is a flat
    /// streak either way.
    ///
    /// The width axis is taken against the direction to `eye` — the line of
    /// sight to *this* bar — and not against the camera's forward. The two are
    /// usually near enough the same, and here they are catastrophically not:
    /// every star in the local tunnel runs exactly *antiparallel* to the
    /// camera's forward, so `dir × forward` is identically zero and the entire
    /// tunnel collapses to nothing. Against the line of sight, only a bar dead
    /// on the view axis degenerates, and that one is a dot.
    #[allow(clippy::too_many_arguments)]
    fn bar(
        &mut self,
        c: Vec3,
        dir: Vec3,
        half_len: f32,
        half_w: f32,
        eye: Vec3,
        brush: Brush,
        tint: LinearRgba,
        alpha: f32,
    ) {
        let along = dir.normalize_or_zero();
        if along == Vec3::ZERO {
            return;
        }
        let to_eye = (eye - c).normalize_or_zero();
        let side = along.cross(to_eye).normalize_or_zero();
        if side == Vec3::ZERO {
            return;
        }
        self.quad(c, along * half_len, side * half_w, brush, tint, alpha);
    }
}

/// One star tunnel's frame: where it is, which way it streams, and how big.
struct Tunnel {
    /// The point the tunnel is centred on: the camera's eye, or a ship.
    origin: Vec3,
    /// The unit vector the stars travel **along** — away from the viewer's
    /// facing, so they stream from ahead of it to behind.
    axis: Vec3,
    /// The two axes of the annulus, perpendicular to `axis`.
    right: Vec3,
    up: Vec3,
    /// Multiplies radius, depth, velocity and streak length together.
    scale: f32,
    /// How many stars.
    count: u32,
}

/// Rewrites the whole warp mesh from the live arrivals.
fn build_surface(
    mut meshes: ResMut<Assets<Mesh>>,
    assets: Res<WarpAssets>,
    warps: Res<Warps>,
    cameras: Query<(&GlobalTransform, &Camera, &Projection), With<Camera3d>>,
    mut build: Local<MeshBuild>,
) {
    let Some((cam, camera, projection)) = cameras.iter().next() else {
        return;
    };
    let Some(mut mesh) = meshes.get_mut(&assets.mesh) else {
        return;
    };

    build.clear();

    let cam_fwd = cam.forward().as_vec3();
    let view = View {
        eye: cam.translation(),
        min_half_angle: min_half_angle(camera, projection),
    };

    for arrival in &warps.live {
        // `warp.js`: `instancedMesh.position.copy(camera.position)` and
        // `.quaternion.copy(camera.quaternion)` — the local tunnel is the
        // camera's own frame, and the stars run toward its back.
        let tunnel = if arrival.local {
            Tunnel {
                origin: cam.translation(),
                axis: -cam_fwd,
                right: cam.right().as_vec3(),
                up: cam.up().as_vec3(),
                scale: 1.0,
                count: STAR_COUNT,
            }
        } else {
            // A remote ship's tunnel is the same thing in the ship's frame: it
            // streams down its own nose, so the ship reads as having flown out
            // of it rather than as having been dropped into a light show.
            let (right, up) = arrival.dir.any_orthonormal_pair();
            Tunnel {
                origin: arrival.pos,
                axis: -arrival.dir,
                right,
                up,
                scale: REMOTE_SCALE,
                count: REMOTE_STARS,
            }
        };

        draw_tunnel(&mut build, arrival, &tunnel, &view);
        draw_flash(&mut build, arrival, cam);
    }

    // An empty attribute list is a valid zero-triangle draw, so an idle frame
    // costs one empty submission rather than a branch here.
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, std::mem::take(&mut build.pos));
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, std::mem::take(&mut build.uv));
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, std::mem::take(&mut build.color));
    mesh.insert_indices(Indices::U32(std::mem::take(&mut build.idx)));
}

/// Where the camera is and how coarsely it samples: what a star needs to
/// billboard itself and to hold the screen-space width floor.
struct View {
    eye: Vec3,
    /// Half of one [`STAR_MIN_HALF_PX`]-wide pixel, in radians.
    min_half_angle: f32,
}

/// How many radians [`STAR_MIN_HALF_PX`] pixels subtend on this camera.
///
/// Read from the live projection rather than from a constant because the FOV
/// punch moves it by a factor of four over the arrival: at 175° a pixel is four
/// times the angle it is at 45°, and a floor derived from the resting FOV would
/// be four times too thin at the moment there is most to see.
fn min_half_angle(camera: &Camera, projection: &Projection) -> f32 {
    let Projection::Perspective(p) = projection else {
        return 0.0;
    };
    let height = camera
        .physical_viewport_size()
        .map_or(0.0, |s| s.y as f32)
        .max(1.0);
    p.fov / height * STAR_MIN_HALF_PX
}

/// The star tunnel: `warp.js`'s update loop, in closed form.
fn draw_tunnel(build: &mut MeshBuild, a: &Arrival, t: &Tunnel, view: &View) {
    let opacity = curves::opacity01(a.age);
    if opacity <= 0.0 {
        return;
    }
    let travelled = curves::travel(a.age);
    let depth = TUNNEL_DEPTH * t.scale;
    let ahead = WRAP_AHEAD * t.scale;
    let authored_half_w = STAR_HALF_WIDTH * t.scale;

    for i in 0..t.count {
        let angle = hash01(a.seed, i, 1) * std::f32::consts::TAU;
        let radius = (RADIUS_MIN + RADIUS_SPAN * hash01(a.seed, i, 2)) * t.scale;
        let z0 = (hash01(a.seed, i, 3) - 0.5) * depth;
        let velocity = (VEL_MIN + VEL_SPAN * hash01(a.seed, i, 4)) * t.scale;

        let z = wrap_z(z0 + velocity * travelled, ahead, depth);
        let len = curves::streak_len(velocity, a.age, t.scale);

        let centre = t.origin
            + t.right * (angle.cos() * radius)
            + t.up * (angle.sin() * radius)
            + t.axis * z;
        // The width floor, in world units at *this* star's distance. See
        // `STAR_MIN_HALF_PX`.
        let half_w = authored_half_w.max(centre.distance(view.eye) * view.min_half_angle);
        build.bar(
            centre,
            t.axis,
            len * 0.5,
            half_w,
            view.eye,
            Brush::CORE,
            STAR_TINT,
            opacity,
        );
    }
}

/// `if (posZ[i] > 100) posZ[i] -= 1000`, for a star that may have lapped the
/// tunnel several times.
///
/// The JS subtracts one depth per frame, which is correct only because it never
/// moves more than one depth in a frame. This is the same interval — stars live
/// in `(ahead - depth, ahead]` — reached in one step from any distance, which
/// is what a closed-form position needs.
fn wrap_z(z: f32, ahead: f32, depth: f32) -> f32 {
    let lo = ahead - depth;
    lo + (z - lo).rem_euclid(depth)
}

/// The arrival flash: a hot core and a soft halo at the point the ship came in.
///
/// Two quads. It is not in `warp.js` — see [`FLASH_TIME`] — and it is kept
/// small on purpose: its whole job is to punctuate the instant the tunnel's
/// opacity envelope is still ramping up from zero through.
fn draw_flash(build: &mut MeshBuild, a: &Arrival, cam: &GlobalTransform) {
    let flash = curves::flash01(a.age);
    if flash <= 0.0 {
        return;
    }
    let right = cam.right().as_vec3();
    let up = cam.up().as_vec3();
    // Opening outward as it fades, so it reads as a burst and not as a lamp
    // being turned down.
    let radius = FLASH_RADIUS * (0.35 + 0.65 * (1.0 - flash));
    build.quad(
        a.pos,
        right * radius,
        up * radius,
        Brush::GLOW,
        FLASH_TINT,
        flash,
    );
    build.quad(
        a.pos,
        right * (radius * 0.35),
        up * (radius * 0.35),
        Brush::CORE,
        FLASH_TINT,
        flash,
    );
}

/// A stable `0..1` from an arrival seed, an index, and a salt.
///
/// A hash rather than an RNG so a star's scatter can be recomputed every frame
/// from nothing but its index — there is no per-star state anywhere in this
/// module, which is what keeps 3 000 of them free where `warp.js` keeps four
/// `Float32Array`s.
///
/// This is renderer-side scatter and is deliberately **not** `sim::rng`: it
/// affects nothing the simulation can see, and `sim`'s streams must not be
/// advanced by a cosmetic effect.
fn hash01(seed: u32, i: u32, salt: u32) -> f32 {
    let mut h = seed ^ i.wrapping_mul(0x9E37_79B9) ^ salt.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846C_A68B);
    h ^= h >> 16;
    // The top 24 bits, which is every bit of mantissa an f32 in 0..1 has.
    (h >> 8) as f32 / 16_777_216.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim::world::ShipView;

    fn ship(id: EntityId, x: f32) -> ShipView {
        ShipView {
            id,
            pos: [x, 0.0, 0.0],
            quat: [0.0, 0.0, 0.0, 1.0],
            flags: ShipFlags::ALIVE,
            ..Default::default()
        }
    }

    // -- the invulnerability window ----------------------------------------

    /// The headline constraint from `BACKLOG.md`: the arrival communicates
    /// spawn protection, so it must fit inside it. The `const` assertion above
    /// enforces this at compile time; this says out loud what it is for.
    #[test]
    fn the_arrival_fits_inside_spawn_protection() {
        assert!(
            f64::from(DURATION) <= RULES.combat.spawn_invuln,
            "a {DURATION}s arrival outlasts {}s of protection",
            RULES.combat.spawn_invuln
        );
        // And it is `warp.js`'s own duration, not something invented here.
        assert_eq!(DURATION, 1.5);
        // Every sub-phase is inside the arrival, or a stray quad would outlive
        // the effect that owns it. Const blocks because clippy is right that
        // these are decided at compile time — which is the point.
        const { assert!(FLASH_TIME <= DURATION) };
        const { assert!(FADE_OUT <= DURATION) };
    }

    // -- the FOV punch ------------------------------------------------------

    #[test]
    fn the_fov_punches_from_175_and_lands_on_the_base() {
        assert_eq!(curves::fov(0.0, BASE_FOV), MAX_FOV);
        assert_eq!(curves::fov(DURATION, BASE_FOV), BASE_FOV);
        assert_eq!(curves::fov(9.0, BASE_FOV), BASE_FOV);

        // 175 degrees, to the precision the constant is written to.
        assert!((MAX_FOV.to_degrees() - 175.0).abs() < 1e-3);
        // And it lands on whatever it was given, not on a hardcoded value —
        // `cockpit.rs` swaps the projection out from under it.
        let seated = 1.0;
        assert_eq!(curves::fov(DURATION, seated), seated);
    }

    #[test]
    fn the_fov_decelerates_rather_than_ramping() {
        let mut prev = f32::INFINITY;
        for step in 0..=60 {
            let v = curves::fov(step as f32 / 60.0 * DURATION, BASE_FOV);
            assert!(v <= prev + 1e-6, "the punch reversed at step {step}");
            assert!((BASE_FOV..=MAX_FOV).contains(&v));
            prev = v;
        }
        // `1 - (1 - p)^6` puts three quarters of the punch in the first
        // quarter of the time. That asymmetry is what reads as deceleration.
        let quarter = curves::fov(DURATION * 0.25, BASE_FOV);
        let travelled = (MAX_FOV - quarter) / (MAX_FOV - BASE_FOV);
        assert!(travelled > 0.75, "only {travelled} of the punch by t/4");
    }

    /// The FOV must be a legal one for glam's perspective matrix at every point
    /// on the curve, or the projection panics mid-arrival.
    #[test]
    fn the_fov_stays_inside_glams_limits() {
        for step in 0..=200 {
            let v = curves::fov(step as f32 / 100.0, BASE_FOV);
            assert!(v > 0.0 && v < std::f32::consts::PI, "illegal fov {v}");
        }
    }

    // -- warp.js's envelope and motion --------------------------------------

    /// The opacity envelope, transcribed from `warp.js:56`: in over the first
    /// 20 %, out over the last 0.8 s, and nothing left at the end.
    #[test]
    fn the_opacity_envelope_matches_the_js() {
        assert_eq!(curves::opacity01(0.0), 0.0);
        assert_eq!(curves::opacity01(DURATION * FADE_IN), 1.0);
        assert_eq!(curves::opacity01(DURATION - FADE_OUT), 1.0);
        assert_eq!(curves::opacity01(DURATION), 0.0);
        assert_eq!(curves::opacity01(9.0), 0.0);

        // Half way through the fade in and the fade out.
        assert!((curves::opacity01(DURATION * FADE_IN * 0.5) - 0.5).abs() < 1e-5);
        assert!((curves::opacity01(DURATION - FADE_OUT * 0.5) - 0.5).abs() < 1e-5);

        for step in 0..=200 {
            let v = curves::opacity01(step as f32 / 100.0);
            assert!((0.0..=1.0).contains(&v), "opacity left 0..1: {v}");
        }
    }

    /// `speedMult = lerp(2.0, 0.05, progress)`, and the tunnel therefore comes
    /// very nearly to a stop rather than cutting out at speed.
    #[test]
    fn the_tunnel_decelerates_to_a_halt() {
        assert_eq!(curves::speed_mult(0.0), SPEED_MULT_START);
        assert_eq!(curves::speed_mult(DURATION), SPEED_MULT_END);
        assert!(curves::speed_mult(DURATION * 0.5) < SPEED_MULT_START);
        assert!(curves::flash01(0.0) == 1.0 && curves::flash01(FLASH_TIME) == 0.0);
    }

    /// The closed-form travel has to match what `warp.js`'s per-frame loop
    /// actually integrates, or the stars move at the wrong speed. Reproduce the
    /// JS loop at 60 Hz and compare.
    #[test]
    fn the_closed_form_travel_matches_the_js_loop() {
        let dt = 1.0f32 / 60.0;
        let mut timer = DURATION;
        let mut z = 0.0f32;
        let mut steps = 0;
        while timer > 0.0 {
            timer -= dt;
            let progress = (1.0 - (timer / DURATION)).clamp(0.0, 1.0);
            let speed_mult = SPEED_MULT_START + (SPEED_MULT_END - SPEED_MULT_START) * progress;
            z += speed_mult * dt;
            steps += 1;
        }
        let closed = curves::travel(DURATION);
        // 1.5 s at 60 Hz is ninety frames, plus whichever side of zero the
        // accumulated `timer` lands on for the last one.
        assert!((90..=91).contains(&steps), "{steps} frames, expected ~90");
        assert!(
            (z - closed).abs() / closed < 0.02,
            "the JS integrates to {z} and this to {closed}"
        );
    }

    #[test]
    fn travel_is_monotone_and_stops_with_the_arrival() {
        let mut prev = -1.0;
        for step in 0..=200 {
            let v = curves::travel(step as f32 / 100.0);
            assert!(v >= prev, "travel went backwards at {step}");
            prev = v;
        }
        assert_eq!(curves::travel(0.0), 0.0);
        assert_eq!(curves::travel(DURATION), curves::travel(9.0));
    }

    /// Streaks are long while the tunnel is fast and short once it has slowed,
    /// which is the whole of `scale.z = max(1, speed * 0.5 + 20)`.
    #[test]
    fn a_streak_is_as_long_as_the_ground_it_covers() {
        let fast = curves::streak_len(VEL_MIN + VEL_SPAN, 0.0, 1.0);
        let slow = curves::streak_len(VEL_MIN + VEL_SPAN, DURATION, 1.0);
        assert!(fast > slow, "{fast} is not longer than {slow}");
        assert!(slow >= LENGTH_BASE, "a stopped star is still {LENGTH_BASE}");
        // A remote tunnel is the same shape at `REMOTE_SCALE`, so its stars
        // must be shorter in the same proportion, not the same length.
        let remote = curves::streak_len((VEL_MIN + VEL_SPAN) * REMOTE_SCALE, 0.0, REMOTE_SCALE);
        assert!((remote / fast - REMOTE_SCALE).abs() < 1e-3);
    }

    /// Stars must stay inside `(ahead - depth, ahead]` however many times they
    /// have lapped, or one would drift out of the tunnel and hang in space.
    #[test]
    fn a_star_wraps_into_the_tunnel_from_any_distance() {
        let (ahead, depth) = (WRAP_AHEAD, TUNNEL_DEPTH);
        for z in [-4000.0, -500.0, 0.0, 99.0, 100.0, 101.0, 7000.0] {
            let w = wrap_z(z, ahead, depth);
            assert!(
                w > ahead - depth - 1e-3 && w <= ahead + 1e-3,
                "{z} wrapped to {w}"
            );
        }
        // And a star that has not moved is exactly where it started.
        assert!((wrap_z(-250.0, ahead, depth) - -250.0).abs() < 1e-4);
    }

    // -- what starts an arrival --------------------------------------------

    /// Match start: every ship in the first tick arrives, and none of them
    /// arrives twice.
    #[test]
    fn every_ship_in_the_first_tick_warps_in() {
        let mut warps = Warps::default();
        let mut frame = Frame::new();
        frame.ships = vec![ship(LOCAL_ID, 0.0), ship(2, 10.0), ship(3, 20.0)];

        warps.observe(&frame);
        assert_eq!(warps.live.len(), 3);
        assert_eq!(warps.local().map(|a| a.id), Some(LOCAL_ID));
        assert_eq!(warps.live.iter().filter(|a| a.local).count(), 1);

        warps.observe(&frame);
        assert_eq!(warps.live.len(), 3, "the same ships arrived twice");
    }

    /// Joining in progress has no `ShipRespawned` — the id simply appears.
    #[test]
    fn a_ship_that_joins_in_progress_warps_in() {
        let mut warps = Warps::default();
        let mut frame = Frame::new();
        frame.ships = vec![ship(LOCAL_ID, 0.0)];
        warps.observe(&frame);
        warps.advance(DURATION);
        assert!(warps.live.is_empty());

        frame.ships.push(ship(7, 99.0));
        warps.observe(&frame);
        assert_eq!(warps.live.len(), 1);
        let a = warps.live[0];
        assert_eq!(a.id, 7);
        assert!(!a.local);
        assert_eq!(a.pos, Vec3::new(99.0, 0.0, 0.0));
        // The nose is local +z, and the tunnel streams down it.
        assert!((a.dir - Vec3::Z).length() < 1e-5);
    }

    /// Respawn: the ship never left `Frame::ships`, so only the event can fire
    /// it — and it must fire at the position the event carries, not at the
    /// ship's stale one.
    #[test]
    fn a_respawn_warps_in_at_the_event_position() {
        let mut warps = Warps::default();
        let mut frame = Frame::new();
        frame.ships = vec![ship(LOCAL_ID, 0.0), ship(2, 0.0)];
        warps.observe(&frame);
        warps.advance(DURATION);

        frame.events = vec![SimEvent::ShipRespawned {
            id: 2,
            pos: sim::math::Vec3::new(4.0, 5.0, 6.0),
        }];
        warps.observe(&frame);
        assert_eq!(warps.live.len(), 1, "a respawn must not fire twice");
        assert_eq!(warps.live[0].id, 2);
        assert_eq!(warps.live[0].pos, Vec3::new(4.0, 5.0, 6.0));
    }

    /// Boss hitboxes are ships. They are never drawn and must never warp in.
    #[test]
    fn boss_hitboxes_do_not_warp_in() {
        let mut warps = Warps::default();
        let mut frame = Frame::new();
        let mut hitbox = ship(9001, 0.0);
        hitbox.flags = ShipFlags::ALIVE.with(ShipFlags::BOSS_HITBOX);
        frame.ships = vec![ship(LOCAL_ID, 0.0), hitbox];

        warps.observe(&frame);
        assert_eq!(warps.live.len(), 1);
        assert_eq!(warps.live[0].id, LOCAL_ID);
    }

    /// Two spawns in quick succession restart one arrival rather than running
    /// two, which would double every additive quad the ship draws.
    #[test]
    fn a_second_spawn_restarts_rather_than_stacks() {
        let mut warps = Warps::default();
        warps.begin(4, Vec3::ZERO, Vec3::Z);
        warps.advance(0.4);
        warps.begin(4, Vec3::X, Vec3::Z);
        assert_eq!(warps.live.len(), 1);
        assert_eq!(warps.live[0].age, 0.0);
        assert_eq!(warps.live[0].pos, Vec3::X);
    }

    #[test]
    fn an_arrival_expires_exactly_at_the_duration() {
        let mut warps = Warps::default();
        warps.begin(LOCAL_ID, Vec3::ZERO, Vec3::Z);
        warps.advance(DURATION - 0.001);
        assert_eq!(warps.live.len(), 1);
        warps.advance(0.001);
        assert!(warps.live.is_empty(), "the arrival outlived its duration");
        assert!(warps.local().is_none());
    }

    // -- the scatter --------------------------------------------------------

    /// Every star's position is recomputed from its index every frame, so the
    /// hash has to be stable, in range, and actually spread out — a constant
    /// would draw 3 000 quads on top of each other.
    #[test]
    fn the_star_scatter_is_stable_and_spread() {
        assert_eq!(hash01(7, 3, 1), hash01(7, 3, 1));
        assert_ne!(hash01(7, 3, 1), hash01(7, 3, 2));
        assert_ne!(hash01(7, 3, 1), hash01(7, 4, 1));

        let mut buckets = [0u32; 8];
        for i in 0..STAR_COUNT {
            let v = hash01(0xC0FF_EE00, i, 2);
            assert!((0.0..1.0).contains(&v), "hash01 left 0..1: {v}");
            buckets[(v * 8.0) as usize] += 1;
        }
        for (b, n) in buckets.iter().enumerate() {
            assert!(*n > 250, "bucket {b} holds only {n} of {STAR_COUNT}");
        }
    }
}
