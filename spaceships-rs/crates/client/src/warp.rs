//! Warp-in on spawn — the sky bends, holds, and snaps back, on every spawn.
//!
//! Built on a port of `public/src/warp.js`, which in the JS client fires only on
//! a campaign respawn and is only a star tunnel. `BACKLOG.md` §9 is the
//! specification, and the reference it names is Star Wars: **the sky bends
//! around the ship and then snaps**.
//!
//! # The four things happening at once
//!
//! 1. **The stars stretch, and the world does not.** The streaking belongs to
//!    the *skybox*: [`skybox::Starfield`] hands over the same stars the cubemap
//!    was baked from, and [`draw_sky`] redraws them as lines running radially
//!    away from the point dead ahead while the cubemap itself dims out from
//!    under them. Ships, rocks and terrain stay sharp, because nothing touches
//!    them. A full-screen version of this is motion blur, which is a different
//!    effect.
//! 2. **The near streaks**, which is `warp.js`'s tunnel, converging inward on
//!    the axis as it goes rather than merely streaming past — arriving, not
//!    travelling.
//! 3. **The field of view**, opening slowly to 175° and then decelerating back
//!    into the camera's resting angle. `1 - (1 - p)^6` spends three quarters of
//!    the punch in the first quarter of the time, which is what reads as
//!    falling out of warp rather than as a zoom. It is the part of `warp.js`
//!    worth keeping, and all that changed is where on the timeline it sits.
//! 4. **The lens**: a radial bend in screen space, a chromatic split on the same
//!    curve, and an expanding shockwave ring that distorts what it passes over.
//!    All three ride in `camera.rs`'s grade pass — see [`camera::WarpLens`] —
//!    because it is already the last node to touch the image.
//!
//! # The snap is the moment
//!
//! The timeline is deliberately **asymmetric**, which is the second thing §9
//! says is easy to get wrong: the collapse from lines back to points has to be
//! far faster than the build-up, or the whole thing reads as a dissolve rather
//! than as an arrival.
//!
//! ```text
//!   0                 BEND_IN        SNAP_AT  ARRIVAL              DURATION
//!   |--- bend in -------|-- hold ------|-snap-|---- relax ------------|
//!   0.00               0.55           0.80   0.90                   1.50
//! ```
//!
//! Everything opens together over 0.9 s and shuts in a tenth of one. In numbers:
//! the FOV takes 0.71 s to travel half its range on the way out and 0.07 s to
//! travel it back. `the_snap_is_faster_than_the_bend` below pins that ratio,
//! because it is the difference between a warp and a fade.
//!
//! `warp.js`'s own envelope — in over the first 20 %, out over the last 0.8 s —
//! was exactly the symmetric shape §9 warns about, and is gone. Everything else
//! of the JS survives: 10–210 unit annulus, 1 000 unit tunnel, velocities of
//! 2 000–5 000 scaled by a `2.0 → 0.05` multiplier, streak length
//! `speed × 0.5 + 20`. Where something deviates it says so at the constant.
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
//! screen-space: the FOV punch, the sky stretch and the lens are the local
//! player's alone, because a remote ship arriving must not move the camera it is
//! being watched through, and the sky is not bending around *it*.
//!
//! # An arrival is a ship's, not the player's
//!
//! [`Warps::begin`] takes a ship id and a pose. Nothing in this module knows or
//! cares whether that ship is the one on this machine beyond one boolean, which
//! is what `BACKLOG.md` §13 needs: warping a whole flight in at match start is
//! this same call in a loop, and [`Warps::begin_after`] already carries the
//! per-ship delay that turns a simultaneous set of arrivals into a formation
//! rippling down the flight. What §13 still needs and this cannot supply is a
//! *shared* clock — the stagger has to come off the `start` frame's spawn list
//! so eight clients agree — and a camera that is not the chase camera.
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
//! `SPACESHIPS_WARP_FREEZE=<secs>` pins every live arrival at that age and
//! never retires it. This is a *motion* effect and a still of a random frame
//! says nothing about whether the snap reads; freezing the clock is what makes
//! "the build-up", "peak stretch", "the instant of collapse" and "settled" four
//! comparable screenshots rather than four lucky ones.
//!
//! # Not done here
//!
//! `BACKLOG.md` also asks for a rising whoosh that cuts hard at arrival. Audio
//! lives in `audio.rs`, which is not this module's file; the trigger it would
//! need is [`Warps::live`], which is already public to this crate, and the beat
//! it should cut on is [`ARRIVAL`].
//!
//! **The Sierras map still warps you in**, minus the sky stretch, because there
//! is no starfield there to stretch — so what is left is a tunnel of white
//! streaks over a blue sky and a runway, which is not right. Suppressing it
//! would put that map's spawn back to nothing at all, and §13 has the actual
//! answer: terrain gets a take-off roll instead of an arrival. This is a
//! placeholder either way, and the better one of the two.

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::image::{ImageSampler, ImageSamplerDescriptor};
use bevy::light::{NotShadowCaster, Skybox};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::post_process::effect_stack::ChromaticAberration;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use sim::world::{EntityId, Frame, ShipFlags, SimEvent};
use spaceships_sim as sim;

use crate::camera::{FilmGrade, FlightCamera, WarpLens, BASE_FOV};
use crate::sim_bridge::{pos as to_vec3, rot, SimFrame, SimSet, LOCAL_ID};
use crate::skybox::{SkyStar, Starfield, TEXEL_RADIANS};

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

/// How long the sky takes to bend, in seconds.
///
/// The slow end of the asymmetry. Long enough to read as space folding rather
/// than as a flicker, and it is the number to raise if the effect ever feels
/// like it starts mid-way.
const BEND_IN: f32 = 0.55;

/// When the lines start collapsing back to points.
///
/// The gap between here and [`BEND_IN`] is the *hold* — a quarter second at
/// full stretch. Without it the stretch reverses the instant it peaks, and a
/// triangle wave reads as a wobble; the hold is what makes the collapse feel
/// like something that was interrupted.
const SNAP_AT: f32 = 0.80;

/// How long the collapse takes. A tenth of a second against 0.9 of build-up.
const SNAP_TIME: f32 = 0.10;

/// The arrival instant: the moment the lines finish collapsing.
///
/// Everything punctuating the arrival hangs off this — the flash, the shockwave,
/// the peak of the lens, and the start of the FOV's deceleration.
pub const ARRIVAL: f32 = SNAP_AT + SNAP_TIME;

/// What is left afterwards for the lens to relax and the FOV to settle through.
///
/// `BACKLOG.md` §9 asks for the bend to relax "on an ease-out over roughly
/// 0.6 s", and this is 0.6 s exactly — [`DURATION`] and [`ARRIVAL`] were chosen
/// to make it so rather than the other way round.
const RELAX: f32 = DURATION - ARRIVAL;

/// The phases have to tile the arrival in order, or a curve would be evaluated
/// on a segment that does not exist.
const _: () = assert!(0.0 < BEND_IN && BEND_IN <= SNAP_AT && ARRIVAL < DURATION);

/// How far apart consecutive ships arrive when a whole roster appears at once.
///
/// `BACKLOG.md` §13's "half-second ripple", divided across a nine-ship flight
/// rather than spent between each pair — nine times half a second would leave
/// the last ship arriving four seconds into the match. Only match start ever
/// sees this: a respawn is one ship and has nothing to be staggered against.
const STAGGER: f32 = 0.055;

/// `starCount`, halved.
///
/// `warp.js` draws 3 000 because the tunnel is its *entire* effect. Here it is
/// the near-field layer over a sky of 1 200 stretched stars, and the count that
/// mattered when it was alone is just fill and vertex bandwidth once something
/// else is drawing the lines. The mesh below pads to a fixed size every frame
/// whatever is happening, so this is 1 600 quads of upload on every frame of the
/// game, not only on the frames that warp — which is what makes it worth
/// halving rather than leaving alone.
const STAR_COUNT: u32 = 1400;

/// Fixed vertex and index budget for the warp mesh.
///
/// Rebuilt every frame, and it **must not change size** doing so — see the
/// padding in the rebuild.
///
/// The worst case is a match-start arrival for a full ten-ship room
/// (`BACKLOG.md` §13): the local tunnel and its sky, plus nine remote tunnels,
/// plus a flash and a shockwave each.
const MESH_QUAD_CAPACITY: usize = 4352;
const MESH_VERTEX_CAPACITY: usize = MESH_QUAD_CAPACITY * 4;
const MESH_INDEX_CAPACITY: usize = MESH_QUAD_CAPACITY * 6;

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

/// How far the near streaks funnel inward at full stretch.
///
/// `BACKLOG.md` §9 wants the existing streaks "collapsing inward to a point
/// rather than streaming past. Arriving, not travelling." The tunnel's motion
/// along its axis is `warp.js`'s and stays; what this adds is the annulus
/// closing on that axis as the bend builds, so the streaks converge on the
/// vanishing point instead of running parallel to it. Not 1.0: at zero radius
/// every streak is the same line and the tunnel stops having a shape.
const CONVERGE: f32 = 0.62;

// -- the parts that are not in warp.js --------------------------------------

/// How long the arrival flash lasts, in seconds.
///
/// Not in `warp.js`, which is a *transition* — it cuts from one scene to
/// another and has no instant to punctuate. A spawn does: [`ARRIVAL`] is the
/// beat the whole timeline is built around, and it needs something on it.
const FLASH_TIME: f32 = 0.20;

/// The flash's radius at its widest, in world units. About a ship's length, and
/// used for *remote* arrivals only — see [`draw_flash`].
const FLASH_RADIUS: f32 = 9.0;

/// The local flash's radius at its widest, as a screen radius (0.5 is half the
/// frame height). Past the corner, so the arrival instant is a wash rather than
/// a bright blob with the old frame still visible around it.
const FLASH_SCREEN: f32 = 1.05;

/// How long the shockwave ring takes to run out, in seconds.
///
/// Comfortably inside [`RELAX`] so the wave has left frame before the arrival
/// is over — a ring still crawling outward when the HUD comes back reads as a
/// stuck sprite.
const RING_TIME: f32 = 0.42;

/// The world-space shockwave's radius at full expansion, in world units.
///
/// About six ship lengths, which from the chase camera's 11 units is most of the
/// frame. It exists so a *remote* arrival gets a shockwave too — the lens is the
/// local player's alone, and without this an enemy folding into existence would
/// have a flash and nothing expanding away from it.
const RING_RADIUS: f32 = 62.0;

/// The lens ring's radius at full expansion, in aspect-corrected screen radii.
///
/// Past the corner of a 16:9 frame (0.98), so the wave leaves rather than
/// stopping at the edge. Deliberately *not* derived by projecting [`RING_RADIUS`]
/// — at 175° a 62-unit circle 11 units from the eye does not project to anything
/// resembling a circle, and the two are tuned to move together by eye instead.
const RING_SCREEN: f32 = 1.20;

/// How hard the shockwave drags the image it crosses. See [`WarpLens::ring_gain`].
const RING_LENS_GAIN: f32 = 0.055;

/// The width of the lens ring's influence, in screen radii.
const RING_LENS_WIDTH: f32 = 0.16;

/// Where in its cell the [`Brush::RING`] annulus peaks, as a fraction of the
/// quad's half-size. The quad has to be a little larger than the ring it draws
/// or the annulus would be clipped by its own edge; this is the conversion.
const RING_BRUSH_PEAK: f32 = 0.85;

/// The shockwave's colour, cooler and much dimmer than the flash.
///
/// It is a lensing artefact rather than a light source: a ring authored as
/// bright as the flash reads as a smoke ring painted over the scene, which is
/// what the first pass at this looked like.
const RING_TINT: LinearRgba = LinearRgba::rgb(0.55, 0.78, 1.0);

/// How far the frame's edge bows at the peak of the bend, as a fraction of
/// frame height. **Negative magnifies** — see [`WarpLens::bend`], where the sign
/// is the difference between a lens and a smeared border.
///
/// Small on purpose. This is the term that sells "space folding" and it is also
/// the one that turns into a fisheye joke if it is generous: 6 % of frame height
/// at the very edge is plainly visible in motion and nearly invisible as a
/// still, which is the correct amount for something that lasts a fifth of a
/// second.
const LENS_BEND: f32 = -0.06;

/// How much the chromatic split grows at the peak of the bend, as a multiple of
/// the camera's resting aberration.
///
/// `BACKLOG.md` §9: "`ChromaticAberration` is already there — animate its
/// intensity on the same curve. Real lensing splits colour; a static value
/// cannot."
///
/// 2.5× of `camera.rs`'s 0.006 is 0.015, near Bevy's own default of 0.02 and
/// about as far as this can go. The effect's brightest objects are one-pixel
/// stars, and the split is a *displacement*: at 6× every star in the sky became
/// a thirty-pixel rainbow dash, which is a different and much worse effect.
const LENS_ABERRATION: f32 = 2.5;

// -- the sky ----------------------------------------------------------------

/// How far out the stretched starfield is drawn, in world units.
///
/// Behind everything any map contains — the moon is 80 across at the origin, the
/// motherships sit at Z = ±600, the asteroid field reaches 400 — and well inside
/// the camera's 20 000 unit far plane. Depth testing is on for the additive
/// material even though depth *writing* is not, so a stretched star behind the
/// moon is correctly hidden by the moon. That is the whole argument for doing
/// this as geometry: a full-screen pass could not know.
const SKY_RADIUS: f32 = 6000.0;

/// How far the sky is dragged along the warp axis at full stretch.
///
/// `d' = normalize(d - k·axis)` is the exact apparent motion of a star field
/// when the viewer travels `k` times the stars' distance along `axis`, and it is
/// what draws every line radially away from the point dead ahead — the vanishing
/// point the reference puts them on. Stars near the axis barely move; stars
/// abeam sweep most of a right angle.
///
/// It must stay under 1: at exactly 1 a star sitting on the axis has nowhere to
/// go, and the normalise flips it to the opposite pole of the sky.
const SKY_STRETCH_K: f32 = 0.86;

/// How much of the cubemap is taken away at full stretch.
///
/// The stars are being redrawn as lines, so the points they came from have to
/// leave or every star is drawn twice. Dimming the whole sky rather than hiding
/// only the stars takes the nebula with it, which is right: a folded sky is
/// streaks on black, and the nebula coming back at the snap is part of what
/// arrives.
///
/// Not 1.0 — a sky that goes fully black for a quarter second reads as a dropped
/// frame.
const SKY_DIM: f32 = 0.88;

/// How far past 1.0 a stretched sky star is authored.
///
/// The cubemap version of the same star goes through [`skybox::SKY_BRIGHTNESS`]
/// and the tone mapper; this one is an unlit additive vertex colour, so there is
/// no shared scale between them and this is matched by eye against a sky at rest.
/// Held down rather than pushed up: a thousand additive lines saturate to a
/// sheet of white long before any one of them is individually bright, and the
/// effect is lines, not glare.
const SKY_STAR_GLOW: f32 = 2.4;

/// The narrowest a stretched sky star may get on screen, as a half-width in
/// pixels. The same argument as [`STAR_MIN_HALF_PX`], and it matters more here:
/// at 175° a one-texel star is a fifth of a pixel.
const SKY_MIN_HALF_PX: f32 = 0.7;

/// How much of a remote ship's tunnel fits inside the local one.
///
/// The local tunnel is 1 000 units deep and 420 across because it wraps the
/// camera. Anchored on a ship instead it has to read as *that ship's* arrival
/// from across the map, so radius, depth, velocity and streak length all take
/// this factor together — the shape is identical, the scale is not.
const REMOTE_SCALE: f32 = 0.16;

/// Stars in a remote ship's tunnel. Fewer than [`STAR_COUNT`] in proportion to
/// the volume they fill, so the density matches — and trimmed again to keep ten
/// simultaneous arrivals inside [`MESH_QUAD_CAPACITY`]. A tunnel seen from
/// across the map is a few dozen pixels wide.
const REMOTE_STARS: u32 = 160;

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
        ARRIVAL, BEND_IN, DURATION, FLASH_TIME, LENGTH_BASE, LENGTH_PER_SPEED, MAX_FOV, REF_DT,
        RELAX, RING_TIME, SNAP_AT, SNAP_TIME, SPEED_MULT_END, SPEED_MULT_START,
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

    /// How folded the sky is: 0 at rest, 1 at full stretch.
    ///
    /// The shape of the whole effect, and the one place its asymmetry is
    /// written down. Smoothstep in over [`BEND_IN`], hold to [`SNAP_AT`], then
    /// a **cubic fall** over [`SNAP_TIME`] — which loses half the stretch in
    /// the first fifth of an already short window. That is the snap.
    ///
    /// This replaces `warp.js`'s opacity envelope, which faded in over the
    /// first 20 % and out over the last 0.8 s of 1.5. Symmetric ends are
    /// exactly what `BACKLOG.md` §9 says makes a warp look like a dissolve, and
    /// the JS has that shape because it is a scene *transition* with nothing on
    /// either side of it to arrive at.
    #[must_use]
    pub fn stretch01(age: f32) -> f32 {
        if age <= 0.0 {
            0.0
        } else if age < BEND_IN {
            let t = age / BEND_IN;
            // Smoothstep, so it leaves rest and reaches the hold without a
            // corner at either end.
            t * t * (3.0 - 2.0 * t)
        } else if age < SNAP_AT {
            1.0
        } else if age < ARRIVAL {
            let q = 1.0 - (age - SNAP_AT) / SNAP_TIME;
            q * q * q
        } else {
            0.0
        }
    }

    /// The screen-space bend: 0 at rest, 1 at [`ARRIVAL`].
    ///
    /// Cubed on the way in so it stays out of the way while the sky is doing
    /// the work and arrives all at once, and a quadratic ease-out over
    /// [`RELAX`] afterwards — §9's "strongest at the arrival instant and
    /// relaxing on an ease-out over roughly 0.6 s".
    #[must_use]
    pub fn bend01(age: f32) -> f32 {
        if age <= 0.0 || age >= DURATION {
            0.0
        } else if age < ARRIVAL {
            let t = age / ARRIVAL;
            t * t * t
        } else {
            let q = 1.0 - (age - ARRIVAL) / RELAX;
            q * q
        }
    }

    /// How far the shockwave has run, `0..=1`, from [`ARRIVAL`].
    ///
    /// Decelerating, because a wave loses energy: `1 - (1 - t)^3` puts most of
    /// the travel in the first third and lets the tail drift.
    #[must_use]
    pub fn ring01(age: f32) -> f32 {
        if age < ARRIVAL || RING_TIME <= 0.0 {
            return 0.0;
        }
        let t = ((age - ARRIVAL) / RING_TIME).clamp(0.0, 1.0);
        let q = 1.0 - t;
        1.0 - q * q * q
    }

    /// How bright the shockwave is, `0..=1`. Fades as it expands.
    #[must_use]
    pub fn ring_fade(age: f32) -> f32 {
        if age < ARRIVAL || RING_TIME <= 0.0 {
            return 0.0;
        }
        let q = 1.0 - ((age - ARRIVAL) / RING_TIME).clamp(0.0, 1.0);
        q * q
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

    /// The arrival flash: 1 at [`ARRIVAL`], 0 [`FLASH_TIME`] later, cubic decay.
    ///
    /// Cubic rather than quadratic so it is gone rather than lingering as a
    /// haze: its whole job is to punctuate one instant, and anything still
    /// visible a fifth of a second later is drawing attention to itself.
    #[must_use]
    pub fn flash01(age: f32) -> f32 {
        if FLASH_TIME <= 0.0 || age < ARRIVAL {
            return 0.0;
        }
        let q = 1.0 - ((age - ARRIVAL) / FLASH_TIME).clamp(0.0, 1.0);
        q * q * q
    }

    /// The field of view, in radians: `base` at t=0, [`MAX_FOV`] at
    /// [`ARRIVAL`], `base` again at [`DURATION`].
    ///
    /// Two curves meeting at the snap, and the asymmetry between them is the
    /// point:
    ///
    /// - **Out**, over 0.9 s: a quintic ease-in. Slow to start, so the first
    ///   half second is a bend you feel rather than a zoom you notice, and
    ///   steepest right as the lines collapse. Quintic and not cubic because
    ///   the last 20° of the opening are severe — the world compresses to a
    ///   spot in the middle of the frame — and the fifth power confines that to
    ///   the 80 ms either side of the snap where it belongs. A cube left the
    ///   scene unreadable for a quarter of a second.
    /// - **Back**, over 0.6 s: `warp.js:59`'s `1 - (1 - p)^6`, which spends
    ///   three quarters of the travel in the first quarter of the time. This is
    ///   the "poosh", and it is the part of the JS effect worth keeping. All
    ///   that moved is where on the timeline it sits: in `warp.js` it runs from
    ///   the first frame, because there the warp *is* the transition and there
    ///   is no arrival instant to hang it on.
    ///
    /// Halfway out takes 0.71 s and halfway back takes 0.07 s.
    #[must_use]
    pub fn fov(age: f32, base: f32) -> f32 {
        // The precise form of a lerp in both branches, not `a + (b - a) * t`:
        // these are *exactly* `base` at the ends and *exactly* `MAX_FOV` at the
        // snap, and landing back on the resting angle bit for bit is what stops
        // a warp from leaving the camera a rounding error away from where it
        // found it.
        if age <= 0.0 || age >= DURATION {
            base
        } else if age < ARRIVAL {
            let t = age / ARRIVAL;
            let open = t * t * t * t * t;
            base * (1.0 - open) + MAX_FOV * open
        } else {
            let q = 1.0 - (age - ARRIVAL) / RELAX;
            let punch = 1.0 - q * q * q * q * q * q;
            MAX_FOV * (1.0 - punch) + base * punch
        }
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
    /// Seconds since the arrival began. **May be negative**, and that is how a
    /// staggered arrival waits its turn: every curve in [`curves`] reads zero
    /// below zero, so a not-yet-started arrival draws nothing and holds the
    /// camera at its resting FOV without needing a state of its own. See
    /// [`Warps::begin_after`].
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
    /// The same trick for the sky's brightness and the camera's resting
    /// chromatic split, which the arrival also drives and also has to hand back
    /// untouched. Captured rather than read from a constant for the reason
    /// [`drive_fov`] gives about `cockpit.rs`: another module may have moved it,
    /// and `terrain::apply_map` moves the sky's.
    base_sky: Option<f32>,
    base_aberration: Option<f32>,
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

        // The stagger. One ship appearing is a respawn and goes now; a whole
        // roster appearing is a match start, and `BACKLOG.md` §13 is explicit
        // that "a half-second ripple down each flight reads as a formation
        // arriving, where a single instant reads as a glitch".
        //
        // The order is `Frame::ships`, which is the simulation's and therefore
        // the same on every client that ran the same tick — which is the
        // property §13 asks for when it says the arrival has to be driven from
        // something everyone agrees on. The local player is always first: you
        // do not watch other people arrive before you do.
        let mut nth = 0u32;
        for ship in &frame.ships {
            // Boss hitboxes are ships so that one damage path can serve the
            // capital ship. They are never drawn, so they never arrive.
            if ship.flags.contains(ShipFlags::BOSS_HITBOX) {
                continue;
            }
            if !self.seen.contains(&ship.id) {
                let delay = if ship.id == LOCAL_ID {
                    0.0
                } else {
                    nth += 1;
                    STAGGER * nth as f32
                };
                self.begin_after(ship.id, to_vec3(ship.pos), rot(ship.quat) * Vec3::Z, delay);
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
        self.begin_after(id, pos, dir, 0.0);
    }

    /// The same, `delay` seconds from now.
    ///
    /// Nothing calls this with a non-zero delay yet. It is here because
    /// `BACKLOG.md` §13 turns on exactly one thing this module does not
    /// otherwise express — "staged rather than simultaneous — a half-second
    /// ripple down each flight reads as a formation arriving, where a single
    /// instant reads as a glitch" — and the cost of expressing it is a negative
    /// starting age. What §13 still has to supply is the *ordering*, which has
    /// to come off the `start` frame's spawn list rather than from anything
    /// local, or eight clients would each ripple their own way.
    pub fn begin_after(&mut self, id: EntityId, pos: Vec3, dir: Vec3, delay: f32) {
        self.next_seed = self.next_seed.wrapping_add(0x9E37_79B9);
        let arrival = Arrival {
            id,
            pos,
            dir: dir.normalize_or(Vec3::Z),
            age: -delay.max(0.0),
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
            //
            // `drive_fov` before `build_surface`, and not merely beside it: the
            // mesh sizes its streaks against the live projection, and reading a
            // FOV one frame stale during a punch that moves it by a factor of
            // four is a visible pop in the streak widths.
            .add_systems(
                PostUpdate,
                (drive_fov, drive_sky, drive_lens, build_surface)
                    .chain()
                    .after(TransformSystems::Propagate),
            );
    }
}

/// Copies this tick's spawns into [`Warps`].
fn watch_spawns(frame: Res<SimFrame>, mut warps: ResMut<Warps>) {
    warps.observe(&frame.0);
}

/// Ages every arrival, or pins them all if `SPACESHIPS_WARP_FREEZE` is set.
///
/// The freeze is how this effect gets looked at. It is 1.5 s of continuous
/// motion whose whole quality is *when* things happen relative to each other,
/// and a screenshot lands wherever the three-second timer lands; pinning the
/// clock turns "take a picture of the snap" from luck into an argument.
fn advance(time: Res<Time>, mut warps: ResMut<Warps>, mut freeze: Local<Option<Option<f32>>>) {
    let freeze = *freeze.get_or_insert_with(|| {
        let at = times("SPACESHIPS_WARP_FREEZE").first().copied();
        if let Some(at) = at {
            info!("warp frozen at age {at}s");
        }
        at
    });

    match freeze {
        // Clamped below `DURATION` so `advance` cannot retire what it just
        // pinned, and a request for the very end still shows the last frame.
        Some(at) => {
            let at = at.min(DURATION - 1e-4);
            for a in &mut warps.live {
                a.age = at;
            }
        }
        None => warps.advance(time.delta_secs()),
    }
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
/// # `FlightCamera`, and why this did nothing at all
///
/// This used to ask for `single_mut()` over `With<Camera3d>`. `ui.rs` runs a
/// second `Camera3d` for the menu's ship preview, and a `single` over two
/// matches does not pick one — it returns `Err`, which the `let ... else` above
/// swallows. So the FOV punch, the loudest part of the whole effect, had been
/// silently switched off since the day the preview landed; `cockpit.rs` records
/// being caught by the identical bug and is why [`FlightCamera`] exists. Every
/// camera query in this module filters on it now.
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
fn drive_fov(mut warps: ResMut<Warps>, mut cam: Query<&mut Projection, With<FlightCamera>>) {
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
// The sky, and the lens
// ---------------------------------------------------------------------------

/// Dims the cubemap while its stars are being drawn as lines.
///
/// [`draw_sky`] redraws the starfield stretched; without this the points would
/// still be sitting under the lines they came from, which reads as a double
/// exposure rather than as motion. The same capture-and-restore shape as
/// [`drive_fov`], and for the same reason — `terrain::apply_map` swaps this
/// component out when the map changes, so the value to put back is whatever was
/// there rather than [`skybox::SKY_BRIGHTNESS`].
///
/// On the Sierras map there is no [`Skybox`] on the camera at all, and the query
/// simply finds nothing: no starfield, no stretch, and the daylight sky is left
/// alone. That is the right answer there — `BACKLOG.md` §13 gives terrain its
/// own entrance, a take-off roll, rather than a warp.
fn drive_sky(mut warps: ResMut<Warps>, mut cam: Query<&mut Skybox, With<FlightCamera>>) {
    let Ok(mut sky) = cam.single_mut() else {
        return;
    };

    let Some(age) = warps.local().map(|a| a.age) else {
        if let Some(base) = warps.base_sky.take() {
            sky.brightness = base;
        }
        return;
    };

    let base = *warps.base_sky.get_or_insert(sky.brightness);
    sky.brightness = base * (1.0 - SKY_DIM * curves::stretch01(age));
}

/// Drives the grade pass's lens and the camera's chromatic split.
///
/// Both ride [`curves::bend01`], which is `BACKLOG.md` §9's "animate its
/// intensity on the same curve": a lens that bends the image without splitting
/// its colour is a lens made of nothing.
fn drive_lens(
    mut warps: ResMut<Warps>,
    mut cam: Query<(&mut FilmGrade, &mut ChromaticAberration), With<FlightCamera>>,
) {
    let Ok((mut grade, mut aberration)) = cam.single_mut() else {
        return;
    };

    let Some(age) = warps.local().map(|a| a.age) else {
        grade.set_lens(WarpLens::NONE);
        if let Some(base) = warps.base_aberration.take() {
            aberration.intensity = base;
        }
        return;
    };

    let base = *warps.base_aberration.get_or_insert(aberration.intensity);
    let bend = curves::bend01(age);
    aberration.intensity = base * (1.0 + LENS_ABERRATION * bend);
    grade.set_lens(WarpLens {
        bend: LENS_BEND * bend,
        ring_radius: RING_SCREEN * curves::ring01(age),
        ring_width: RING_LENS_WIDTH,
        ring_gain: RING_LENS_GAIN * curves::ring_fade(age),
    });
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
    const GLOW: Brush = Brush { u0: 0.0, u1: CELL };
    /// A near-solid bar with soft ends: a star streak, which in `warp.js` is a
    /// box with a hard edge.
    const CORE: Brush = Brush {
        u0: CELL,
        u1: 2.0 * CELL,
    };
    /// A bright annulus with soft edges: the shockwave.
    const RING: Brush = Brush {
        u0: 2.0 * CELL,
        u1: 1.0,
    };
}

/// How many brushes share the atlas, and one cell's width in UV.
const ATLAS_CELLS: u32 = 3;
const CELL: f32 = 1.0 / ATLAS_CELLS as f32;

/// Atlas cell size. Every brush is a smooth ramp magnified far past 1:1.
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

/// Bakes the three brushes into one 192×64 RGBA image.
///
/// The shape rides entirely in **alpha** with RGB left white: `AlphaMode::Add`
/// premultiplies in the shader, so a fragment contributes
/// `vertex_colour.rgb × vertex_colour.a × texture.a` — colour and brightness
/// from the vertex, silhouette from the texture.
fn build_brush_atlas() -> Image {
    let w = ATLAS_CELL * ATLAS_CELLS;
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
            } else if cell == 1 {
                // A solid disc with a soft rim: as close to the JS's hard-edged
                // box as a textured quad gets without aliasing.
                (1.0 - ((r - 0.6) / 0.4)).clamp(0.0, 1.0)
            } else {
                // A narrow annulus at `RING_BRUSH_PEAK`, dying to zero at the
                // rim and hollow inside — the shockwave. Cubed so it is a thin
                // bright line with a soft skirt rather than a fat band: this is
                // meant to be the *edge* of a wave, and the wave itself is the
                // distortion the lens is doing on either side of it.
                let band = ((r - RING_BRUSH_PEAK) / 0.11).abs();
                (1.0 - band).clamp(0.0, 1.0).powi(3)
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

    /// A billboarded bar between two points.
    ///
    /// [`Self::bar`] in the form a stretched star wants it: the sky knows where
    /// the line starts and ends, not where its middle is and how long it is.
    /// A degenerate segment — an unstretched star, or one on the axis that has
    /// nowhere to go — still draws, as a round dot of `half_w`, which is what it
    /// should look like.
    #[allow(clippy::too_many_arguments)]
    fn segment(
        &mut self,
        p0: Vec3,
        p1: Vec3,
        half_w: f32,
        eye: Vec3,
        brush: Brush,
        tint: LinearRgba,
        alpha: f32,
    ) {
        let mid = (p0 + p1) * 0.5;
        let span = p1 - p0;
        let half_len = span.length() * 0.5;
        if half_len <= half_w {
            // Shorter than it is wide: `bar` would pick a width axis off a
            // near-zero direction and give up. A square quad is the same
            // silhouette and cannot degenerate.
            let to_eye = (eye - mid).normalize_or(Vec3::Z);
            let (right, up) = to_eye.any_orthonormal_pair();
            self.quad(mid, right * half_w, up * half_w, brush, tint, alpha);
            return;
        }
        self.bar(mid, span, half_len, half_w, eye, brush, tint, alpha);
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
    stars: Option<Res<Starfield>>,
    cameras: Query<(&GlobalTransform, &Camera, &Projection, Has<Skybox>), With<FlightCamera>>,
    mut build: Local<MeshBuild>,
) {
    let Ok((cam, camera, projection, has_sky)) = cameras.single() else {
        return;
    };
    let Some(mut mesh) = meshes.get_mut(&assets.mesh) else {
        return;
    };

    build.clear();

    let cam_fwd = cam.forward().as_vec3();
    let (angle_per_px, tan_half_fov) = view_scales(camera, projection);
    let view = View {
        eye: cam.translation(),
        angle_per_px,
        tan_half_fov,
    };

    // The sky stretch is the local player's alone and only where there *is* a
    // sky: the Sierras camera has no `Skybox`, and stretching a starfield over
    // green hills at noon would be nonsense.
    if let (Some(stars), Some(arrival), true) = (stars.as_deref(), warps.local(), has_sky) {
        draw_sky(&mut build, arrival, &stars.stars, cam, &view);
    }

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
        draw_flash(&mut build, arrival, cam, &view);
        draw_shockwave(&mut build, arrival, cam, &view);
    }

    // Pad to a fixed size rather than submitting a shorter list on a quiet
    // frame. A mesh that changes vertex count between frames makes Bevy's slab
    // allocator free and reallocate its GPU entry, and the render world's copy
    // step then references the key that just went away:
    //
    //     ERROR bevy_render::slab_allocator: Use-after-free: attempted to copy
    //     element data for an unallocated key
    //
    // With this module idle and `weapons.rs` doing the same thing, that fired
    // twice a frame for an entire session. The padding is degenerate triangles:
    // zero-area, so the rasteriser discards them, and zero-alpha, so the
    // additive blend would contribute nothing even if one survived.
    //
    // Overflow is dropped whole quads rather than truncated buffers, the shape
    // `weapons.rs` settled on: a `resize` down mid-quad would leave indices
    // pointing past the end of the vertex list.
    if build.pos.len() > MESH_VERTEX_CAPACITY {
        warn_once!("warp mesh overran its {MESH_QUAD_CAPACITY}-quad budget");
        build.pos.truncate(MESH_VERTEX_CAPACITY);
        build.uv.truncate(MESH_VERTEX_CAPACITY);
        build.color.truncate(MESH_VERTEX_CAPACITY);
        build.idx.truncate(MESH_INDEX_CAPACITY);
    }
    build.pos.resize(MESH_VERTEX_CAPACITY, [0.0; 3]);
    build.uv.resize(MESH_VERTEX_CAPACITY, [0.0; 2]);
    build.color.resize(MESH_VERTEX_CAPACITY, [0.0; 4]);
    build.idx.resize(MESH_INDEX_CAPACITY, 0);

    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, std::mem::take(&mut build.pos));
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, std::mem::take(&mut build.uv));
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, std::mem::take(&mut build.color));
    mesh.insert_indices(Indices::U32(std::mem::take(&mut build.idx)));
}

/// Where the camera is and how coarsely it samples: what a star needs to
/// billboard itself and to hold the screen-space width floor.
struct View {
    eye: Vec3,
    /// How many radians one screen pixel subtends vertically.
    angle_per_px: f32,
    /// `tan(fov / 2)`, the exact projection of a billboard at the centre of
    /// frame. See [`View::half_height_at`].
    tan_half_fov: f32,
}

impl View {
    /// The world-space half-width that covers `px` pixels at `dist`.
    ///
    /// The small-angle approximation, which is what a *width* floor wants: it
    /// is applied to things a pixel or two across, where the two agree.
    fn px_at(&self, dist: f32, px: f32) -> f32 {
        dist * self.angle_per_px * px
    }

    /// How much world half of the frame's height covers, `dist` in front.
    ///
    /// The exact `tan`, not the small-angle form, because this one is asked
    /// about things that fill the screen at a field of view of 175°, where the
    /// two differ by a factor of *twenty-three*. Reading the flash's size off
    /// the linear approximation is why it was a speck at the moment it was
    /// meant to be everything.
    fn half_height_at(&self, dist: f32) -> f32 {
        dist * self.tan_half_fov
    }

    /// A billboard half-size, in world units, that covers `lens` — the same
    /// aspect-corrected screen radius [`WarpLens`] measures in, where 0.5 is
    /// half the frame height. This is what puts the drawn shockwave on top of
    /// the one the lens is bending.
    fn lens_radius_at(&self, dist: f32, lens: f32) -> f32 {
        2.0 * self.half_height_at(dist) * lens
    }
}

/// How many radians one pixel subtends on this camera, and `tan(fov / 2)`.
///
/// Read from the live projection rather than from a constant because the FOV
/// punch moves it by a factor of four over the arrival: at 175° a pixel is four
/// times the angle it is at 45°, and a floor derived from the resting FOV would
/// be four times too thin at the moment there is most to see.
fn view_scales(camera: &Camera, projection: &Projection) -> (f32, f32) {
    let Projection::Perspective(p) = projection else {
        return (0.0, 1.0);
    };
    let height = camera
        .physical_viewport_size()
        .map_or(0.0, |s| s.y as f32)
        .max(1.0);
    // `set_fov` already clamps to `PI - 0.01`, so the tangent cannot run away.
    (p.fov / height, (p.fov * 0.5).tan())
}

/// The star tunnel: `warp.js`'s update loop, in closed form.
///
/// Two things are not the JS's. The envelope is [`curves::stretch01`], so the
/// tunnel dies with the snap instead of dissolving over the last 0.8 s. And the
/// annulus closes on the axis as the bend builds ([`CONVERGE`]), so the streaks
/// converge on the vanishing point rather than running parallel past it — §9's
/// "arriving, not travelling".
fn draw_tunnel(build: &mut MeshBuild, a: &Arrival, t: &Tunnel, view: &View) {
    let stretch = curves::stretch01(a.age);
    if stretch <= 0.0 {
        return;
    }
    let travelled = curves::travel(a.age);
    let depth = TUNNEL_DEPTH * t.scale;
    let ahead = WRAP_AHEAD * t.scale;
    let authored_half_w = STAR_HALF_WIDTH * t.scale;
    let converge = 1.0 - CONVERGE * stretch;

    for i in 0..t.count {
        let angle = hash01(a.seed, i, 1) * std::f32::consts::TAU;
        let radius = (RADIUS_MIN + RADIUS_SPAN * hash01(a.seed, i, 2)) * t.scale * converge;
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
        let half_w = authored_half_w.max(view.px_at(centre.distance(view.eye), STAR_MIN_HALF_PX));
        build.bar(
            centre,
            t.axis,
            len * 0.5,
            half_w,
            view.eye,
            Brush::CORE,
            STAR_TINT,
            stretch,
        );
    }
}

/// The starfield, stretched into lines toward the vanishing point.
///
/// The headline of `BACKLOG.md` §9 and the thing that makes the effect read as
/// Star Wars rather than as a particle system. Each star of the cubemap is
/// redrawn as the segment between where it is at rest and where a viewer
/// travelling `k` star-distances along the warp axis would see it:
///
/// ```text
///     d' = normalize(d - k · axis)
/// ```
///
/// which leaves a star dead ahead alone, sweeps a star abeam most of a right
/// angle, and draws every line on a great circle radiating from the point the
/// ship is pointing at. [`drive_sky`] takes the cubemap down underneath by the
/// same amount, so what the player sees is one starfield changing shape rather
/// than two starfields.
///
/// Local arrivals only, and the caller enforces that: the sky bends around the
/// viewer, not around a ship a kilometre away.
fn draw_sky(
    build: &mut MeshBuild,
    a: &Arrival,
    stars: &[SkyStar],
    cam: &GlobalTransform,
    view: &View,
) {
    let stretch = curves::stretch01(a.age);
    if stretch <= 0.0 {
        return;
    }
    let axis = cam.forward().as_vec3();
    let k = SKY_STRETCH_K * stretch;
    let eye = view.eye;
    // Every star sits on the same shell, so the pixel floor is one divide for
    // the whole layer rather than one per star.
    let floor = view.px_at(SKY_RADIUS, SKY_MIN_HALF_PX);

    for star in stars {
        let rest = star.dir;
        // `k < 1` is guaranteed by `SKY_STRETCH_K`, which is what keeps this
        // normalise from flipping a star on the axis to the far pole.
        let moved = (rest - axis * k).normalize_or(rest);
        let p0 = eye + rest * SKY_RADIUS;
        let p1 = eye + moved * SKY_RADIUS;

        let half_w = floor.max(SKY_RADIUS * star.radius_px * TEXEL_RADIANS);
        let tint = LinearRgba::rgb(
            star.color.red * SKY_STAR_GLOW,
            star.color.green * SKY_STAR_GLOW,
            star.color.blue * SKY_STAR_GLOW,
        );
        build.segment(p0, p1, half_w, eye, Brush::CORE, tint, 1.0);
    }
}

/// The shockwave: a ring expanding away from the point the ship came in at.
///
/// Two sizes again, on the same argument as [`draw_flash`], and here the local
/// one is not merely legible but *load bearing*: [`drive_lens`] is bending the
/// image in a band at `RING_SCREEN × ring01`, and this is the visible ring that
/// band is supposed to belong to. Sizing it through [`View::lens_radius_at`] is
/// what puts the two on top of each other rather than near each other, which is
/// the difference between a shockwave and a painted circle next to a bulge.
///
/// The atlas draws its annulus at [`RING_BRUSH_PEAK`] of the quad's half-size,
/// so the quad is that much larger than the ring it draws.
///
/// A remote arrival keeps world units, and is the only part of the punctuation
/// anyone else can see.
fn draw_shockwave(build: &mut MeshBuild, a: &Arrival, cam: &GlobalTransform, view: &View) {
    let fade = curves::ring_fade(a.age);
    if fade <= 0.0 {
        return;
    }
    let expand = curves::ring01(a.age) / RING_BRUSH_PEAK;
    let radius = if a.local {
        view.lens_radius_at(a.pos.distance(view.eye).max(1.0), RING_SCREEN * expand)
    } else {
        RING_RADIUS * expand
    };
    let right = cam.right().as_vec3();
    let up = cam.up().as_vec3();
    build.quad(
        a.pos,
        right * radius,
        up * radius,
        Brush::RING,
        RING_TINT,
        fade,
    );
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
/// Two quads, and not in `warp.js` — see [`FLASH_TIME`].
///
/// # Two sizes, for the same reason the tunnel has two
///
/// A remote arrival is an event happening over *there*, and gets a world-space
/// flash a ship's length across. The local one is an event happening to the
/// *viewer*, at a field of view of 175° where world sizes stop meaning anything:
/// [`FLASH_RADIUS`] eleven units from the eye at that angle is four pixels, so
/// the loudest instant of the whole effect was reading as the screen going dark.
/// Sized against the frame instead, through [`View::lens_radius_at`], it is the
/// white wash an arrival is supposed to be.
fn draw_flash(build: &mut MeshBuild, a: &Arrival, cam: &GlobalTransform, view: &View) {
    let flash = curves::flash01(a.age);
    if flash <= 0.0 {
        return;
    }
    let right = cam.right().as_vec3();
    let up = cam.up().as_vec3();
    // Opening outward as it fades, so it reads as a burst and not as a lamp
    // being turned down. It starts at a bit over half rather than at a third:
    // the brightest instant is also the first one, and a wash that has to grow
    // into covering the frame spends that instant not covering it.
    let grow = 0.55 + 0.45 * (1.0 - flash);
    let radius = if a.local {
        view.lens_radius_at(a.pos.distance(view.eye).max(1.0), FLASH_SCREEN) * grow
    } else {
        FLASH_RADIUS * grow
    };
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
        const { assert!(ARRIVAL + FLASH_TIME <= DURATION) };
        const { assert!(ARRIVAL + RING_TIME <= DURATION) };
        const { assert!(ARRIVAL + RELAX <= DURATION) };
    }

    // -- the FOV punch ------------------------------------------------------

    #[test]
    fn the_fov_opens_to_175_at_the_snap_and_lands_on_the_base() {
        assert_eq!(curves::fov(0.0, BASE_FOV), BASE_FOV);
        assert_eq!(curves::fov(ARRIVAL, BASE_FOV), MAX_FOV);
        assert_eq!(curves::fov(DURATION, BASE_FOV), BASE_FOV);
        assert_eq!(curves::fov(9.0, BASE_FOV), BASE_FOV);
        // A staggered arrival waits at a negative age, and must not move the
        // camera while it does. See `Warps::begin_after`.
        assert_eq!(curves::fov(-0.4, BASE_FOV), BASE_FOV);

        // 175 degrees, to the precision the constant is written to.
        assert!((MAX_FOV.to_degrees() - 175.0).abs() < 1e-3);
        // And it lands on whatever it was given, not on a hardcoded value —
        // `cockpit.rs` swaps the projection out from under it.
        let seated = 1.0;
        assert_eq!(curves::fov(DURATION, seated), seated);
        assert_eq!(curves::fov(0.0, seated), seated);
    }

    #[test]
    fn the_fov_opens_then_decelerates_back() {
        // Monotone out to the snap, monotone back after it, and never outside
        // the two angles it runs between.
        let sample = |t: f32| curves::fov(t, BASE_FOV);
        let mut prev = -f32::INFINITY;
        for step in 0..=60 {
            let v = sample(step as f32 / 60.0 * ARRIVAL);
            assert!(v >= prev - 1e-6, "the bend reversed at step {step}");
            assert!((BASE_FOV..=MAX_FOV).contains(&v));
            prev = v;
        }
        let mut prev = f32::INFINITY;
        for step in 0..=60 {
            let v = sample(ARRIVAL + step as f32 / 60.0 * RELAX);
            assert!(v <= prev + 1e-6, "the punch reversed at step {step}");
            assert!((BASE_FOV..=MAX_FOV).contains(&v));
            prev = v;
        }

        // `1 - (1 - p)^6` puts three quarters of the punch in the first
        // quarter of the time. That asymmetry is what reads as deceleration
        // rather than as a zoom, and it is `warp.js:59`'s.
        let quarter = sample(ARRIVAL + RELAX * 0.25);
        let travelled = (MAX_FOV - quarter) / (MAX_FOV - BASE_FOV);
        assert!(travelled > 0.75, "only {travelled} of the punch by t/4");
    }

    /// The headline of `BACKLOG.md` §9's second bullet: "the collapse from lines
    /// back to points wants to be much faster than the build-up ... easing both
    /// ends symmetrically is what makes a warp look like a dissolve."
    ///
    /// Measured on the two curves that carry the whole effect — how long each
    /// takes to travel half its range, out and back.
    #[test]
    fn the_snap_is_faster_than_the_bend() {
        let half = |f: &dyn Fn(f32) -> f32, from: f32, to: f32, target: f32| {
            let steps = 20_000;
            for i in 0..=steps {
                let t = from + (to - from) * i as f32 / steps as f32;
                if f(t) >= target {
                    return t - from;
                }
            }
            to - from
        };

        // The field of view, base to halfway and back.
        let mid_fov = (BASE_FOV + MAX_FOV) * 0.5;
        let out = half(&|t| curves::fov(t, BASE_FOV), 0.0, ARRIVAL, mid_fov);
        let back = half(&|t| -curves::fov(t, BASE_FOV), ARRIVAL, DURATION, -mid_fov);
        assert!(out > 0.6, "the bend takes only {out}s, which is a jolt");
        assert!(
            back * 5.0 < out,
            "{back}s back against {out}s out is not a snap"
        );

        // And the stretch itself, which is what actually collapses.
        let up = half(&curves::stretch01, 0.0, SNAP_AT, 0.5);
        let down = half(&|t| -curves::stretch01(t), SNAP_AT, DURATION, -0.5);
        assert!(
            down * 10.0 < up,
            "the sky unbends in {down}s against {up}s bending, which is a fade"
        );
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

    /// The envelope, phase by phase: nothing at rest, full across the hold,
    /// nothing again the moment the snap finishes.
    #[test]
    fn the_stretch_bends_in_holds_and_snaps_out() {
        assert_eq!(curves::stretch01(-1.0), 0.0);
        assert_eq!(curves::stretch01(0.0), 0.0);
        assert_eq!(curves::stretch01(BEND_IN), 1.0);
        assert_eq!(curves::stretch01(SNAP_AT), 1.0);
        assert_eq!(curves::stretch01(ARRIVAL), 0.0);
        assert_eq!(curves::stretch01(DURATION), 0.0);
        assert_eq!(curves::stretch01(9.0), 0.0);

        // Smoothstep is symmetric about its midpoint.
        assert!((curves::stretch01(BEND_IN * 0.5) - 0.5).abs() < 1e-5);

        for step in 0..=200 {
            let v = curves::stretch01(step as f32 / 100.0);
            assert!((0.0..=1.0).contains(&v), "stretch left 0..1: {v}");
        }

        // The hold is real — a triangle wave here would read as a wobble.
        const { assert!(SNAP_AT - BEND_IN > 0.15) };
    }

    /// The lens peaks *on* the collapse and eases out over §9's 0.6 s, and the
    /// shockwave leaves with it.
    #[test]
    fn the_lens_peaks_at_the_arrival_instant() {
        assert_eq!(curves::bend01(0.0), 0.0);
        assert_eq!(curves::bend01(ARRIVAL), 1.0);
        assert_eq!(curves::bend01(DURATION), 0.0);
        assert!((RELAX - 0.6).abs() < 1e-5, "§9 asks for roughly 0.6 s");

        // Continuous across the seam, or the bend would jump on the one frame
        // everything else is jumping on and read as a glitch.
        let before = curves::bend01(ARRIVAL - 1e-4);
        assert!((before - 1.0).abs() < 1e-2, "the lens jumps at the snap");

        // Nothing before the arrival, everything after it, gone by the end.
        assert_eq!(curves::ring01(ARRIVAL - 0.01), 0.0);
        assert_eq!(curves::ring_fade(ARRIVAL), 1.0);
        assert_eq!(curves::ring01(ARRIVAL + RING_TIME), 1.0);
        // Not `== 0.0`: `1 - (age - ARRIVAL) / RING_TIME` is a difference of
        // sums that lands a couple of ulps off zero, and squaring it leaves
        // 1e-22. That is nothing, and asking for a bit-exact zero here would be
        // asking the wrong question — the ring is invisible either way.
        assert!(curves::ring_fade(ARRIVAL + RING_TIME) < 1e-9);
        // The wave decelerates: half the travel well inside half the time.
        assert!(curves::ring01(ARRIVAL + RING_TIME * 0.5) > 0.8);

        assert_eq!(curves::flash01(ARRIVAL - 0.01), 0.0);
        assert_eq!(curves::flash01(ARRIVAL), 1.0);
        assert!(curves::flash01(ARRIVAL + FLASH_TIME) < 1e-9);
    }

    /// `speedMult = lerp(2.0, 0.05, progress)`, and the tunnel therefore comes
    /// very nearly to a stop rather than cutting out at speed.
    #[test]
    fn the_tunnel_decelerates_to_a_halt() {
        assert_eq!(curves::speed_mult(0.0), SPEED_MULT_START);
        assert_eq!(curves::speed_mult(DURATION), SPEED_MULT_END);
        assert!(curves::speed_mult(DURATION * 0.5) < SPEED_MULT_START);
    }

    /// The mesh pads to a fixed size every frame, so the budget has to cover
    /// the worst case rather than the usual one — `BACKLOG.md` §13's ten-ship
    /// match start, where every ship in the room arrives at once.
    #[test]
    fn the_mesh_budget_covers_a_full_rooms_arrival() {
        // The local player: a tunnel, the whole stretched sky, two flash quads
        // and a shockwave. `skybox::SHELL_MIN_ALPHA` keeps about 40 % of 2 532
        // stars plus 60 bright cores — a shade over a thousand, and 1 200 here
        // for headroom. `attach_sky` logs the real figure at startup.
        let sky = 1200;
        let local = STAR_COUNT as usize + sky + 3;
        // Nine others, each a tunnel plus a flash and a shockwave.
        let remote = 9 * (REMOTE_STARS as usize + 3);
        assert!(
            local + remote <= MESH_QUAD_CAPACITY,
            "{} quads against a {MESH_QUAD_CAPACITY} budget",
            local + remote
        );
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

    /// A whole roster appearing at once is a match start, and ripples;
    /// the local player never waits for it.
    #[test]
    fn a_match_start_ripples_down_the_flight() {
        let mut warps = Warps::default();
        let mut frame = Frame::new();
        frame.ships = vec![ship(2, 0.0), ship(LOCAL_ID, 10.0), ship(3, 20.0)];
        warps.observe(&frame);

        let age = |id| warps.live.iter().find(|a| a.id == id).unwrap().age;
        assert_eq!(age(LOCAL_ID), 0.0, "you do not queue behind your own team");
        // In `Frame::ships` order, which every client shares, and not in the
        // order they happen to be found.
        assert_eq!(age(2), -STAGGER);
        assert_eq!(age(3), -2.0 * STAGGER);
        // The last of them still fits inside spawn protection, arrival and all.
        let last = 2.0 * STAGGER + DURATION;
        assert!(f64::from(last) <= RULES.combat.spawn_invuln * 2.0);
    }

    /// A respawn is one ship, and one ship has nothing to be staggered against.
    #[test]
    fn a_lone_respawn_does_not_wait() {
        let mut warps = Warps::default();
        let mut frame = Frame::new();
        frame.ships = vec![ship(LOCAL_ID, 0.0), ship(2, 0.0)];
        warps.observe(&frame);
        warps.advance(DURATION);

        frame.events = vec![SimEvent::ShipRespawned {
            id: 2,
            pos: sim::math::Vec3::new(0.0, 0.0, 0.0),
        }];
        warps.observe(&frame);
        assert_eq!(warps.live[0].age, 0.0);
    }

    /// A staggered arrival is inert until its turn: it draws nothing and, above
    /// all, does not move the camera.
    #[test]
    fn a_waiting_arrival_changes_nothing() {
        assert_eq!(curves::stretch01(-0.3), 0.0);
        assert_eq!(curves::bend01(-0.3), 0.0);
        assert_eq!(curves::flash01(-0.3), 0.0);
        assert_eq!(curves::ring01(-0.3), 0.0);
        assert_eq!(curves::ring_fade(-0.3), 0.0);
        assert_eq!(curves::fov(-0.3, BASE_FOV), BASE_FOV);
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
        // Within a fifth of an even eighth. A tighter bound is a test of this
        // particular hash's luck rather than of whether the stars are spread.
        let even = STAR_COUNT / 8;
        for (b, n) in buckets.iter().enumerate() {
            assert!(
                *n > even * 4 / 5,
                "bucket {b} holds only {n} of {STAR_COUNT}"
            );
        }
    }
}
