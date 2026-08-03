//! Static scene setup, the Ultra lighting rig, and the per-frame sync from
//! [`SimFrame`] to transforms.
//!
//! The sync is keyed on the ids the simulation already hands out —
//! `ShipView::id` and `RockView::id` — which is what `Frame` was shaped for:
//! the renderer keeps its own id→entity map and never rebuilds the scene graph.
//!
//! Lighting and materials target **Ultra Graphics**
//! (`public/src/graphics.js`), not the default forward path. See
//! [`crate::camera`] for the post-processing half of that.
//!
//! # Render interpolation
//!
//! The simulation is fixed-step at [`sim::world::TICK_HZ`] and the display is
//! not — 144 Hz here, so most frames run no tick at all and some run two.
//! Applying the latest tick's transform directly held an entity still for two
//! or three frames and then jumped it, which read as the ship stuttering back
//! and forth. The fix is the standard one and it is split across two schedules:
//!
//! - **[`sample_ships`] / [`sample_rocks`] run in `FixedUpdate`**, once per
//!   *tick*, and push each entity's pose into an [`Interp`] — `prev` and
//!   `curr`, two consecutive ticks.
//! - **[`draw_interpolated`] runs in `RunFixedMainLoop`**, once per *frame*,
//!   and writes `Transform = prev.mix(curr, alpha)` where `alpha` is
//!   [`Time<Fixed>::overstep_fraction`] — how far the fixed accumulator has run
//!   past the tick it last executed.
//!
//! Sampling has to be on the tick, not the frame: a frame that ran no tick
//! would otherwise record `prev == curr` and freeze, and a frame that ran two
//! would skip one and lurch.
//!
//! This is *purely visual*. Nothing interpolated is ever read back — `sim`
//! stays authoritative, [`SimFrame`] stays the last tick verbatim, and the
//! renderer draws one tick (16.7 ms) behind it, which is the latency this
//! trades for smoothness.
//!
//! Rotation is **slerp**, never lerp: see [`Pose::mix`]. Spawns and teleports
//! must not interpolate at all: see [`Interp::spawned`] and [`Interp::snap`].
//!
//! # Per-entity appearance, without a material per entity
//!
//! Two things here vary per entity and would each, done the obvious way, cost a
//! material asset per entity: the asteroid damage tint and the ship hit flash.
//! `asteroids.js` does exactly that — `BASE_MAT.clone()` per rock so it can set
//! `emissive` — and it is the reason the JS client's sixty asteroids are sixty
//! draw calls instead of six. See `main.rs` on why this port exists.
//!
//! The two are solved differently, on purpose:
//!
//! - **Asteroids** keep **one** [`RockMaterial`] across the whole field. The
//!   per-rock state is packed into [`MeshTag`], a `u32` that rides in the
//!   per-instance mesh uniform, and [`DamageFlash`]'s fragment shader unpacks
//!   it. `MeshTag` is not part of the batch key, so sixty rocks flashing
//!   independently are still six batches — one per mesh variant. Nothing is
//!   cloned and no material is written at runtime.
//! - **Ships** do clone, once each, at spawn. Ten ships in ten colours are ten
//!   materials however it is arranged, and `main.rs` says as much; ten is not
//!   sixty and they are not the bottleneck. The flash then writes those
//!   already-per-ship materials, gated on `Changed` so an unhit ship writes
//!   nothing.
//!
//! `SPACESHIPS_BATCHES=1` prints the count. If it starts tracking the entity
//! count, something has gone back to cloning — see [`report_batches`].

use bevy::asset::{uuid_handle, RenderAssetUsages};
use bevy::image::{ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::light::{CascadeShadowConfigBuilder, NotShadowCaster};
use bevy::mesh::MeshTag;
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::{Shader, ShaderRef};
use bevy::world_serialization::{WorldAsset, WorldAssetRoot, WorldInstanceReady};
use std::f32::consts::{FRAC_PI_2, PI};

use spaceships_protocol::{ClientMessage, ServerMessage};
use spaceships_sim as sim;

use crate::net::{FromServer, NetSession, NetSet, Phase, ToServer};
use crate::sim_bridge::{pos, rot, SimFrame, SimSet, LOCAL_ID};

/// The player model, at the asset root.
///
/// `jet.glb` — the F-22 — as of packaging the game for people who are not going
/// to set an environment variable. Everything tuned this cycle was tuned on it:
/// [`model_fit`]'s scale and nose-ward shift, `weapons.rs`'s nozzle offsets,
/// `cockpit.rs`'s `JET_PROFILE`. Shipping `spaceship.glb` by default meant a
/// packaged build showed the old blocky hull while every fit constant around it
/// described the aircraft.
///
/// `SPACESHIPS_SHIP_MODEL` still overrides, and `spaceship.glb` still works —
/// its fit is the identity and its cockpit profile is untouched.
/// `spaceshipADMIN.glb` is the third and is 4.9 MB, which is a decision for
/// later, not a default.
const SHIP_MODEL: &str = "jet.glb";

/// Which model to fly, so an alternative can be judged in motion rather than
/// from stills.
///
/// `SPACESHIPS_SHIP_MODEL=jet.glb` flies the converted F-22 (7,249 triangles,
/// against `spaceship.glb`'s 516). It is not the default yet: it is 23%
/// narrower and 22% shorter than the ship it would replace, and it is centred
/// on its own bounding box where the old model's origin sits well forward — so
/// it frames differently in the chase camera. Both are decisions to make after
/// flying it.
///
/// Native only. On the web there is no process environment, so the constant
/// stands.
pub(crate) fn ship_model() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    if let Ok(name) = std::env::var("SPACESHIPS_SHIP_MODEL") {
        if !name.is_empty() {
            return name;
        }
    }
    SHIP_MODEL.to_owned()
}

/// Scale and nose-ward offset applied to the model, in model space.
///
/// The camera anchors on the ship's *origin*, and the two models put their
/// origin in very different places. `spaceship.glb`'s body sits **1.44 units
/// ahead of and 0.22 above** its origin — measured with node transforms
/// applied, which is what actually renders — while `jet.glb` is centred on its
/// own bounding box. Flying the jet unadjusted therefore hangs the body back
/// and low from the point the chase camera frames, so it sits at the bottom of
/// the screen behind the HUD instead of near the middle.
///
/// The jet is also 23% narrower across the span, which is the dimension a chase
/// camera reads as "how big is my ship", so it is scaled to match.
///
/// Numbers are model space, before `ship.js`'s -90 degree yaw and before
/// [`SHIP_SCALE`], which the returned scale folds in. Nose is +x.
fn model_fit(model: &str) -> (f32, Vec3) {
    let (fit, offset) = model_fit_unscaled(model);
    (fit * SHIP_SCALE, offset)
}

/// `main.js:188`. Every ship in the JS is drawn at `1.5`: the local one at
/// `main.js:219` and every remote one at `:667`.
///
/// **This was missing, and it is why ships looked tiny.** The comment above
/// already said these numbers were "before `SHIP_SCALE`" — nothing ever applied
/// it, so hulls rendered at two thirds the size the game is designed around.
///
/// It is not only a look. `main.js:980` derives the collision radius as
/// `2.2 * SHIP_SCALE`, and [`spaceships_sim::rules::ShipRules::collide_radius`]
/// is that 3.3 with the 1.5 already inside it. Drawing at 1.0 therefore put a
/// hitbox around a ship half again bigger than the mesh, so shots that visibly
/// missed still landed. Restoring the scale is what makes the two agree.
///
/// Anything anchored to the hull in world space has to carry it too — see
/// `weapons.rs`'s nozzles.
pub(crate) const SHIP_SCALE: f32 = 1.5;

/// [`model_fit`] before [`SHIP_SCALE`], which is the space the fit was measured
/// in: both entries below compare one model against the other, and neither
/// changes when the shared scale does.
fn model_fit_unscaled(model: &str) -> (f32, Vec3) {
    if model.contains("jet") {
        // span 4.17 -> 5.43 to match the ship it replaces, then the origin
        // moved to the same fraction along the hull the old model used.
        // Span alone (5.43 / 4.17) matched the old ship's width but still read
        // small, because the jet is also a third flatter -- 1.34 against 1.97 --
        // so it presents much less area from behind. Scaled past parity until it
        // has the same presence in the chase view.
        (1.62, Vec3::new(1.98, 0.20, 0.0))
    } else {
        (1.0, Vec3::ZERO)
    }
}

/// [`model_fit`] for the hull actually flying, expressed in **ship** space.
///
/// The model child carries `rotation_y(-PI/2)` *and* the fit, so a model point
/// `p` lands at `scale * R * (p + offset)`. Rewriting that in ship space —
/// where `q = R * p` is where the anchor would have sat unfitted — gives
///
/// ```text
/// q' = scale * (q + R * offset)
/// ```
///
/// a uniform scale about a shifted origin, and that is the whole fit. Anything
/// anchored to the hull has to go through it: the engine nozzles
/// (`weapons.rs`'s `TRAIL_OFFSETS_JET`, which bake it in) and the pilot's eye
/// (`cockpit.rs`'s profiles, which apply it). An anchor that skips it stays
/// where `spaceship.glb` put it while the hull moves out from under it.
pub(crate) fn ship_fit() -> (f32, Vec3) {
    let (scale, offset) = model_fit(&ship_model());
    (scale, Quat::from_rotation_y(-FRAC_PI_2) * offset)
}

/// `upgradeMaterials`'s `anisotropy: 8`.
const ANISOTROPY: u16 = 8;

/// The rules, so the teleport thresholds below are *derived* rather than
/// guessed at. `rules.rs` is where a number like this is allowed to live, and
/// re-deriving it here is how it cannot drift.
const RULES: sim::rules::Rules = sim::rules::Rules::DEFAULT;

/// The fastest a ship can legitimately travel: full throttle, boosting, with a
/// fully charged brake-release on top.
const TOP_SPEED: f64 =
    RULES.ship.max_throttle * RULES.ship.boost_factor + RULES.ship.brake_boost_bonus_max;

/// Squared distance one tick may cover before the renderer calls it a teleport
/// rather than motion.
///
/// Eight ticks of [`TOP_SPEED`] — roughly 25 units. Comfortably above anything
/// `resolve_world_collisions` can push a ship out by in a single step, and two
/// orders of magnitude below the several hundred units a respawn or a campaign
/// warp moves it. This is the backstop under [`Snap`] and
/// `SimEvent::ShipRespawned`, for a discontinuity nobody announced.
const TELEPORT_DIST_SQ: f32 = {
    let d = TOP_SPEED * sim::world::TICK_DT * 8.0;
    (d * d) as f32
};

/// The luminance below which a ship's authored material is an **accent**
/// rather than **hull**.
///
/// `isAccentMesh` (`ship.js:34`) and the customizer's preview
/// (`customization.js:155`) both split on `0.2126 r + 0.7152 g + 0.0722 b <
/// 0.35`, and the customizer's split is the one the player actually sees when
/// they pick their colours — so it is a contract, not a heuristic. The JS also
/// checks the mesh *name* for `cockpit`/`engine`/`window`/`glass`, which is the
/// half that does not survive a model swap; the luminance test is the half that
/// does, and it is the only one used here.
///
/// Rec. 709 coefficients on **linear** components, because that is what
/// `THREE.Color` holds after the glTF loader has converted `baseColorFactor`
/// into the renderer's working space. [`LinearRgba`] is the same quantity.
const ACCENT_LUMA: f32 = 0.35;

/// A `0xRRGGBB` literal as an sRGB colour.
///
/// Exists so that every colour constant below can be diffed against the
/// JS or CSS it came from character for character, which is the only way a
/// table copied out of another language stays honest. `Srgba::hex` does the
/// same job at runtime and from a string; this is the same thing for a `const`.
pub(crate) const fn hex(rgb: u32) -> Srgba {
    Srgba::rgb(
        ((rgb >> 16) & 0xff) as f32 / 255.0,
        ((rgb >> 8) & 0xff) as f32 / 255.0,
        (rgb & 0xff) as f32 / 255.0,
    )
}

/// Hull colour when nothing is saved. `customization.js:8`.
const DEFAULT_HULL: Srgba = hex(0x9f_b6cc);

/// Accent colour when nothing is saved. `customization.js:11`.
const DEFAULT_ACCENT: Srgba = hex(0x2a_3340);

/// The two team hulls, `--color-blue` and `--color-red` from `index.html`'s
/// design tokens — the same pair the scoreboard, the match HUD, and the
/// friend/foe markers already use, so a ship reads as its team at a glance.
const TEAM_HULL: [Srgba; 2] = [hex(0x66_ddff), hex(0xff_5566)];

/// `main.js:538`. The fallback for a pilot whose colours have not arrived yet,
/// keyed by id so two of them are never the same.
const PALETTE: [Srgba; 8] = [
    hex(0xff_5577),
    hex(0x55_ff88),
    hex(0xff_cc55),
    hex(0xaa_66ff),
    hex(0x55_ddff),
    hex(0xff_99cc),
    hex(0xff_8833),
    hex(0x99_ff55),
];

/// The paints a bot's airframe can be sprayed in.
///
/// Bots used to take [`PALETTE`] too, and that list is a *lobby* palette: it was
/// written for `spaceship.glb`, six flat-shaded primitives with a glow, where a
/// saturated primary reads as a toy spaceship because the thing is a toy
/// spaceship. `jet.glb` is an F-22 — a large, smooth, grey airframe — and
/// `#55ff88` on it reads as a die-cast model, not as an aircraft.
///
/// So these are *paints*, not colours. Every one is a scheme a real airframe
/// wears: the two greys of an air-superiority wrap, gunship grey, the sand and
/// field drab of a desert scheme, olive and sea grey from the maritime ones, and
/// the oxblood, brick and slate an aggressor squadron uses precisely because
/// they disappear against ground and sky. None is saturated enough to be
/// mistaken for [`TEAM_HULL`] at a glance, which matters: cyan and red mean
/// *side*, and a bot is not a side.
///
/// Twelve rather than eight so a five-a-side skirmish rarely repeats one.
const BOT_LIVERY: [Srgba; 12] = [
    hex(0x76_8a9c), // air superiority blue
    hex(0x8b_9399), // ghost grey
    hex(0x4e_555c), // gunship grey
    hex(0xb2_9d79), // desert sand
    hex(0x8a_7c5c), // field drab
    hex(0x63_6b47), // olive drab
    hex(0x44_5f55), // sea green
    hex(0x76_4545), // oxblood
    hex(0x8a_5f4a), // brick
    hex(0x53_6684), // slate
    hex(0x50_6d75), // storm grey
    hex(0x3d_4654), // aggressor charcoal
];

/// How far a livery's accent sits below its hull.
///
/// Multiplied in sRGB rather than in linear light, deliberately: this is
/// choosing a second *paint*, not simulating a shadow, and a gamma-space scale
/// is what a painter means by "the same colour, three shades down". At 0.32
/// every entry in [`BOT_LIVERY`] lands well under [`ACCENT_LUMA`], so the
/// canopy, the intakes and the exhaust cans stay dark on all twelve.
const ACCENT_SHADE: f32 = 0.32;

/// The same paint, several shades down. See [`ACCENT_SHADE`].
const fn shade(c: Srgba) -> Srgba {
    Srgba::rgb(
        c.red * ACCENT_SHADE,
        c.green * ACCENT_SHADE,
        c.blue * ACCENT_SHADE,
    )
}

/// How far apart in [`BOT_LIVERY`] two consecutive ids land, and where the walk
/// starts.
///
/// # Why this and not a hash
///
/// The requirement is *variety*, and the constraint is that the answer must be
/// the same in every window looking at the same aircraft. That rules out `rand`
/// immediately — two pilots describing the same bot would disagree — and it
/// rules out `crates/sim`'s seeded streams even more firmly: drawing a colour
/// from them would consume a value the server did not, and desync a simulation
/// over something nobody can see.
///
/// The obvious remaining answer is to hash the id, and it was the first one
/// tried. It is worse than it looks. Nine bots drawing independently from twelve
/// paints is the birthday problem: the expected number of *distinct* colours is
/// `12 * (1 - (11/12)^9)`, about six and a half, and the measured spread for a
/// skirmish's ids was five. Three pairs of identical aircraft is precisely the
/// complaint this feature exists to answer.
///
/// A stride coprime with the table length is a bijection over any twelve
/// consecutive ids, so a skirmish's nine bots take **nine different paints,
/// always**. Five is coprime with twelve and far enough round the table that
/// consecutive ids do not read as a sequence — which is the only thing the hash
/// was buying. This is also what the JS did (`PALETTE[id % PALETTE.length]`),
/// with a stride of one and eight brighter colours.
const LIVERY_STRIDE: i32 = 5;
/// Where the walk starts. Arbitrary, and the one number here that could be
/// anything.
const LIVERY_OFFSET: i32 = 7;

/// One bot's paint, decided by its id and by nothing else.
///
/// `rem_euclid` rather than `%`: bot ids are **negative** on the wire
/// (`server/index.js` allocates them as `-(nextId++)`), and Rust's remainder
/// keeps the sign of the dividend, which would index a table backwards.
fn bot_livery(id: sim::world::EntityId) -> ShipPaint {
    #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
    let index = id
        .wrapping_mul(LIVERY_STRIDE)
        .wrapping_add(LIVERY_OFFSET)
        .rem_euclid(BOT_LIVERY.len() as i32) as usize;
    let hull = BOT_LIVERY[index];
    ShipPaint {
        hull: hull.into(),
        accent: shade(hull).into(),
    }
}

/// Emissive added to a struck ship at `hit_flash == 1`.
///
/// Chosen against the bloom prefilter rather than by eye. `camera.rs` sets
/// `threshold: 0.9, threshold_softness: 0.4`, so the soft knee starts around
/// 0.7 and a flash meant to *bloom* rather than merely brighten has to clear
/// it; 3.6 is four times over, and still on the knee a fifth of a second later,
/// when `HIT_FLASH_DECAY_RATE` has taken it to a quarter.
///
/// **The other channels are what keep it red.** ACES pulls anything much past
/// 1.0 toward white, so a flash with every channel over the knee comes out
/// white however saturated the ratio was — which is what a first pass at
/// `(6.0, 1.2, 0.9)` looked like on screen. Green and blue are held under 0.9
/// deliberately, so the tonemapper has something left to desaturate *toward*
/// and the hull reads as flaring red rather than blowing out.
///
/// Red-hot rather than the rock's orange: a ship taking fire and a rock
/// chipping have to be distinguishable in peripheral vision.
const SHIP_FLASH: LinearRgba = LinearRgba::rgb(3.6, 0.5, 0.35);

/// Emissive added to a struck asteroid at `hit_flash == 1`.
///
/// `asteroids.js:103` is `emissive.setRGB(f, f * 0.6, f * 0.3)` — an orange
/// ramp peaking at 1.0, which under Ultra is then multiplied by `glowBoost`
/// (1.7). That sits barely over this pipeline's bloom threshold, so the JS's
/// ratio is kept exactly and only the magnitude is raised, to 2.4. Same
/// reasoning as [`SHIP_FLASH`] on why it is not raised further: at 4.0 the
/// green channel also saturates and sixty rocks flash white instead of orange.
const ROCK_FLASH: LinearRgba = LinearRgba::rgb(2.4, 2.4 * 0.6, 2.4 * 0.3);

/// Ultra's `glowBoost` (`graphics.js:379`), the one multiplier that turns an
/// authored glow colour into one the bloom pass can see. See [`glow`].
pub(crate) const GLOW_BOOST: f32 = 1.7;

/// What a rock's albedo is multiplied by once its HP reaches zero — a scorched,
/// slightly warm darkening, so a nearly-dead asteroid reads as chewed up before
/// it breaks. Interpolated toward white as HP returns to full.
const ROCK_SCORCH: LinearRgba = LinearRgba::rgb(0.45, 0.38, 0.34);

/// `|dot|` between two unit quaternions is `cos(theta / 2)`, so this is a
/// quarter turn — 94 rad/s against an authored `PITCH_RATE` of 1.75. A rotation
/// that large in one tick is a respawn resetting the ship's attitude, not a
/// roll.
const TELEPORT_DOT: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Marks the root entity of a ship. The glTF model hangs off this as a child;
/// this entity's transform is the simulation's, unmodified.
///
/// The id is carried on the component as well as in [`Registry`] so that a
/// system which does not have the registry — a trail emitter, a nameplate, the
/// HUD's lock-on marker — can still tell which ship it is looking at.
#[derive(Component)]
pub struct ShipRoot(pub sim::world::EntityId);

/// A prop that only exists on the space map: the moon and the two motherships.
///
/// [`crate::terrain`] hides these rather than despawning them, because their
/// meshes and materials are built once in [`setup`] and rebuilding them on
/// every lobby round trip would be work for nothing. Everything marked here is
/// a root with `Visibility`, so hiding the root hides its children too.
#[derive(Component)]
pub(crate) struct SpaceScenery;

/// An entity the *current map* owns, and that is despawned wholesale when the
/// map changes.
///
/// The lighting rig is the shared case — [`install_space_lights`] and the
/// terrain sun are alternatives, not additions — and everything
/// [`crate::terrain`] spawns carries it too. Kept here rather than in
/// `terrain.rs` so the two rigs are marked by the same component in the module
/// that defines one of them.
#[derive(Component)]
pub(crate) struct MapScenery;

/// Marks an asteroid entity. Carries its id for the same reason.
#[derive(Component)]
#[expect(dead_code, reason = "read by systems this slice does not have yet")]
struct Rock(u32);

/// id → entity, so a ship or rock keeps its entity (and its material, and its
/// place in the render world's caches) across frames.
#[derive(Resource, Default)]
struct Registry {
    ships: HashMap<sim::world::EntityId, Entity>,
    rocks: HashMap<u32, Entity>,
}

// ---------------------------------------------------------------------------
// Render interpolation
// ---------------------------------------------------------------------------

/// One tick's pose: the part of a [`Transform`] the simulation drives.
///
/// A `Transform` would do, but a distinct type is what keeps the *sampled* pose
/// and the *drawn* transform from being confused for one another — the drawn
/// one is a blend of two samples and is never what any tick said.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Pose {
    translation: Vec3,
    rotation: Quat,
    scale: Vec3,
}

impl Pose {
    /// A ship, straight off its `ShipView`.
    fn of_ship(view: &sim::world::ShipView) -> Pose {
        Pose {
            translation: pos(view.pos),
            // Normalized because `Quat::slerp` is only defined on unit
            // quaternions and this one has just been narrowed from `f64`. The
            // simulation's is a unit quaternion; this only repairs the last
            // bits the cast cost.
            rotation: rot(view.quat).normalize(),
            scale: Vec3::ONE,
        }
    }

    /// An asteroid, straight off its `RockView`.
    fn of_rock(view: &sim::world::RockView) -> Pose {
        Pose {
            translation: pos(view.pos),
            // The simulation reports asteroid attitude as Euler angles, which
            // wrap. Converting to a quaternion *before* interpolating is not a
            // convenience: interpolating the angles themselves would reverse a
            // rock's spin every time one of them crossed pi.
            rotation: Quat::from_euler(EulerRot::XYZ, view.rot[0], view.rot[1], view.rot[2]),
            // The rock meshes are unit-radius, so `size` is the scale directly.
            scale: Vec3::splat(view.size),
        }
    }

    /// The pose `alpha` of the way from `self` to `to`.
    ///
    /// Rotation is **slerp, not lerp**. Lerping two quaternions and
    /// renormalizing traverses the arc at a non-constant rate — quickest at the
    /// ends, slowest through the middle — so a ship holding one steady roll
    /// would visibly speed up and slow down *within* every tick, which is a
    /// different artifact from the judder this exists to remove and no less
    /// obvious. `Quat::slerp` also negates `to` when the dot product comes out
    /// negative, which is what takes the short way round: `q` and `-q` are the
    /// same orientation, but the arcs from them to a third quaternion are not,
    /// and the wrong one is a 350-degree spin where the ship turned 10.
    fn mix(self, to: Pose, alpha: f32) -> Pose {
        Pose {
            translation: self.translation.lerp(to.translation, alpha),
            rotation: self.rotation.slerp(to.rotation, alpha),
            scale: self.scale.lerp(to.scale, alpha),
        }
    }

    /// Whether `next` is somewhere this pose could have *moved* to in one tick,
    /// as opposed to been placed at. See [`TELEPORT_DIST_SQ`].
    fn is_continuous_to(self, next: Pose) -> bool {
        self.translation.distance_squared(next.translation) <= TELEPORT_DIST_SQ
            && self.rotation.dot(next.rotation).abs() >= TELEPORT_DOT
    }

    fn transform(self) -> Transform {
        Transform {
            translation: self.translation,
            rotation: self.rotation,
            scale: self.scale,
        }
    }
}

/// The two consecutive ticks a rendered entity is drawn between.
///
/// A component rather than a side table so that it despawns with the entity it
/// describes — a `Frame` id that gets reused after a despawn cannot then pick up
/// the previous occupant's pose and streak across the map.
#[derive(Component, Clone, Copy, Debug)]
struct Interp {
    /// The tick before last.
    prev: Pose,
    /// The last tick. The simulation's actual, authoritative state.
    curr: Pose,
}

impl Interp {
    /// An entity that appeared this tick.
    ///
    /// Both ends are the same pose, so it draws exactly where the simulation
    /// put it from its very first frame. Anything else gives a brand new
    /// entity a `prev` it never had — whatever the spawn bundle happened to
    /// carry, which for the old `Transform::default()` on an asteroid was the
    /// origin, and a field seeded at radius 400 then flies in from the middle
    /// of the moon.
    fn spawned(at: Pose) -> Interp {
        Interp { prev: at, curr: at }
    }

    /// Records this tick's pose as motion, to be interpolated from the last.
    ///
    /// Falls back to [`Interp::snap`] for a step too large to be motion, which
    /// is the unannounced-teleport backstop.
    fn advance(&mut self, next: Pose) {
        if self.curr.is_continuous_to(next) {
            self.prev = self.curr;
            self.curr = next;
        } else {
            self.snap(next);
        }
    }

    /// Records this tick's pose as a discontinuity: do not interpolate, the
    /// entity is simply *there* now.
    ///
    /// Respawn and the campaign warp are the cases. Interpolating across either
    /// draws the ship as a streak the length of the map, over one tick.
    fn snap(&mut self, to: Pose) {
        self.prev = to;
        self.curr = to;
    }

    /// The pose to draw, `alpha` of the way through the current tick interval.
    fn at(&self, alpha: f32) -> Pose {
        self.prev.mix(self.curr, alpha)
    }
}

/// Marks a rendered entity as having been moved discontinuously: its next
/// sample snaps instead of interpolating.
///
/// Two other routes reach the same [`Interp::snap`], and this is the one for a
/// system that *knows* it teleported something — insert it in the same frame as
/// the move and the sampler consumes it on the next tick. The other two are
/// `SimEvent::ShipRespawned`, which the simulation announces on its own, and
/// the [`TELEPORT_DIST_SQ`] guard, which catches whatever announces nothing.
#[derive(Component)]
pub struct Snap;

// ---------------------------------------------------------------------------
// The asteroid damage tint, without a material per rock
// ---------------------------------------------------------------------------

/// The rock shader, built from [`DAMAGE_FLASH_WGSL`] at startup.
///
/// A UUID handle and an inline source string rather than a `.wgsl` in the asset
/// root, for two reasons. The asset root is `public/`, shared with the Three.js
/// client, and a Bevy-only shader does not belong in it; and `build-wasm.sh`
/// copies a named list of assets into `web/assets/`, so a file there is a second
/// place to remember. Compiled in, it cannot go missing on either target.
const DAMAGE_FLASH_SHADER: Handle<Shader> = uuid_handle!("6ff1b6b6-0e94-4d75-9a3d-1a7c2f0e5b41");

/// The whole of the damage tint, as a fragment shader.
///
/// It is `pbr.wgsl`'s forward path with two lines inserted, and the two lines
/// read their inputs from `MeshTag` — a `u32` that lives in the *per-instance*
/// mesh uniform, next to the model matrix. That is the entire trick, and it is
/// why this does not cost a draw call: the batch key is `(mesh, material,
/// pipeline)`, and the tag is in none of them. Sixty rocks with sixty different
/// flash values are still six batches.
///
/// The alternative Bevy usually reaches for — a storage buffer in the material,
/// indexed by instance — needs the same instance index and does not work on
/// WebGL2, which has no storage buffers. The tag does.
const DAMAGE_FLASH_WGSL: &str = r#"
#import bevy_pbr::forward_io::{VertexOutput, FragmentOutput}
#import bevy_pbr::mesh_functions::get_tag
#import bevy_pbr::pbr_fragment::pbr_input_from_standard_material
#import bevy_pbr::pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing}
#import bevy_pbr::pbr_types::STANDARD_MATERIAL_FLAGS_UNLIT_BIT

struct DamageFlash {
    // rgb: the emissive added at full flash. a: a plain multiplier on it.
    flash: vec4<f32>,
    // rgb: what albedo is multiplied by at zero HP. a: unused.
    scorch: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> damage: DamageFlash;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    // The per-instance half. See `pack_damage` on the Rust side for the layout.
    let tag = get_tag(in.instance_index);
    let flash01 = f32(tag >> 16u) * (1.0 / 65535.0);
    let hp01 = f32(tag & 0xffffu) * (1.0 / 65535.0);

    var pbr_input = pbr_input_from_standard_material(in, is_front);

    // Persistent damage: albedo walks toward `scorch` as HP falls.
    let wear = mix(damage.scorch.rgb, vec3<f32>(1.0), hp01);
    pbr_input.material.base_color = vec4<f32>(
        pbr_input.material.base_color.rgb * wear,
        pbr_input.material.base_color.a
    );

    // The hit itself: an additive emissive pulse for the bloom pass to find.
    pbr_input.material.emissive = vec4<f32>(
        pbr_input.material.emissive.rgb + damage.flash.rgb * (flash01 * damage.flash.a),
        pbr_input.material.emissive.a
    );

    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);

    var out: FragmentOutput;
    if (pbr_input.material.flags & STANDARD_MATERIAL_FLAGS_UNLIT_BIT) == 0u {
        out.color = apply_pbr_lighting(pbr_input);
    } else {
        out.color = pbr_input.material.base_color;
    }
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
"#;

/// The constants the rock shader reads. **Not** the per-rock state — this is one
/// buffer shared by the whole field, and the only reason it is a uniform at all
/// is so the ramp can be retuned without editing WGSL.
///
/// Two `Vec4`s rather than a `Vec3` and a float: WebGL2 requires 16-byte-aligned
/// uniform struct members, and `vec3` is the classic way to get that wrong.
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
struct DamageFlash {
    #[uniform(100)]
    flash: Vec4,
    #[uniform(100)]
    scorch: Vec4,
}

impl MaterialExtension for DamageFlash {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(DAMAGE_FLASH_SHADER)
    }
}

/// The one material all sixty asteroids share.
type RockMaterial = ExtendedMaterial<StandardMaterial, DamageFlash>;

/// Packs a rock's damage state into the 32 bits of [`MeshTag`].
///
/// `flash` in the high half, `hp01` in the low half, both as unsigned
/// normalized 16-bit. 16 bits is far more than either needs — the flash decays
/// over 15 ticks and HP tops out at 50 — but a `u32` is what the tag *is*, and
/// splitting it evenly means neither field can be the one that runs out.
///
/// Quantizing is not a cost here, it is the point: an unchanged tag is not
/// re-extracted, so a field of rocks nobody is shooting writes nothing at all.
fn pack_damage(flash: f32, hp01: f32) -> u32 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u32;
    (q(flash) << 16) | q(hp01)
}

/// The HP an asteroid of this size started with.
///
/// [`RockView`](sim::world::RockView) carries `hp` but not the tier that set
/// it, and a fraction is what the shader wants. The tiers' size ranges do not
/// overlap, so `size` recovers the tier exactly — and taking it from
/// [`sim::rules::ASTEROID_TIERS`] rather than a local table is what stops the
/// renderer's idea of "damaged" drifting from the simulation's.
fn rock_max_hp(size: f32) -> i32 {
    let tiers = sim::rules::ASTEROID_TIERS;
    for tier in tiers {
        if size as f64 <= tier.max_size {
            return tier.hp;
        }
    }
    tiers[tiers.len() - 1].hp
}

/// A rock's damage state, ready for [`MeshTag`].
fn rock_tag(view: &sim::world::RockView) -> MeshTag {
    let max = rock_max_hp(view.size).max(1) as f32;
    MeshTag(pack_damage(view.hit_flash, view.hp as f32 / max))
}

// ---------------------------------------------------------------------------
// Ship paint
// ---------------------------------------------------------------------------

/// The colours one ship is painted in, decided when it spawns.
///
/// `applyColorsToShip` (`ship.js:21`) takes exactly this pair and splits the
/// model's meshes between them; see [`ACCENT_LUMA`] for how the split is made.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
struct ShipPaint {
    hull: Color,
    accent: Color,
}

/// One of a ship's cloned materials, and what has to be remembered about it.
struct SkinPart {
    material: Handle<StandardMaterial>,
    /// The emissive it settled on once the Ultra sweep was done with it.
    /// [`flash_ships`] *adds* to this — an engine bell that glows on its own
    /// must still glow after a flash has come and gone.
    emissive: LinearRgba,
    /// Which half of the livery this material takes, decided **once**, from the
    /// authored colour, in [`paint_and_upgrade`].
    ///
    /// Recorded rather than re-derived because the split is not a fixed point:
    /// team red is a dark colour by Rec. 709 luma, so asking [`is_accent`] about
    /// a material that has *already been painted* red would call the hull an
    /// accent and swap the two on the next repaint. See
    /// `painting_is_not_a_fixed_point_of_the_split`.
    accent: bool,
}

/// The materials a ship owns.
///
/// Cloned per ship, which is the one place in this file that is allowed to be:
/// ten ships with ten different hulls are ten materials however you arrange it,
/// and ten is not sixty.
#[derive(Component)]
struct ShipSkin(Vec<SkinPart>);

/// The damage flash the ship's materials are currently showing.
///
/// A component and not just a read of `SimFrame` so that [`flash_ships`] can be
/// driven by `Changed`: a ship nobody has hit writes no materials at all, which
/// matters because writing one re-uploads its bind group.
#[derive(Component, Clone, Copy, PartialEq, Default)]
struct HitFlash(f32);

/// The two ways a ship's emissive can come to need rewriting: the flash moved,
/// or the materials it writes have only just been created. See [`flash_ships`]
/// for why the second one is not redundant.
type NeedsFlash = Or<(Changed<HitFlash>, Changed<ShipSkin>)>;

/// The same two ways, one component along: the paint moved, or the materials it
/// writes have only just been created. See [`repaint_ships`].
type NeedsPaint = Or<(Changed<ShipPaint>, Changed<ShipSkin>)>;

/// What [`sample_ships`] writes on a ship that already exists — the pose, the
/// alive/dead visibility, the damage flash, and the paint.
type ShipSample<'w, 's> = Query<
    'w,
    's,
    (
        &'static mut Interp,
        &'static mut Visibility,
        &'static mut HitFlash,
        &'static mut ShipPaint,
        Has<Snap>,
    ),
    With<ShipRoot>,
>;

/// The paint a ship arrives in.
///
/// Five cases, in the order they take precedence:
///
/// 1. **The local pilot** wears the colours they chose, whatever team they are
///    on. `main.js:781` reads `getSavedShipColor()` for the player and consults
///    the team only for *other* people's markers.
/// 2. **A pilot who announced their colours** wears those. That is the `colors`
///    message, and [`read_remote_paint`] is what puts it in [`RemotePaint`].
/// 3. **A bot** gets a muted squadron paint, taken from its id — see
///    [`bot_livery`].
/// 4. **Anyone on a team** wears the team's, which is all `Frame` can tell us
///    about a stranger whose `colors` has not arrived yet.
/// 5. **Anyone else** gets a stable per-id colour from `main.js`'s palette,
///    which is what `createShip({ tint })` does before a team is assigned.
fn paint_for(view: &sim::world::ShipView, livery: &Livery, remote: &RemotePaint) -> ShipPaint {
    if view.id == LOCAL_ID {
        return livery.paint();
    }
    if let Some(&announced) = remote.0.get(&view.id) {
        return announced;
    }
    if view.flags.contains(sim::world::ShipFlags::BOT) {
        return bot_livery(view.id);
    }
    let hull = match usize::try_from(view.team) {
        Ok(team) if team < TEAM_HULL.len() => TEAM_HULL[team],
        // Unassigned, or a team index the palette has outgrown.
        _ => PALETTE[view.id.unsigned_abs() as usize % PALETTE.len()],
    };
    ShipPaint {
        hull: hull.into(),
        accent: DEFAULT_ACCENT.into(),
    }
}

/// Whether an authored material is an **accent** rather than **hull**.
///
/// `isAccentMesh` (`ship.js:34`), minus the mesh-name half. The name test —
/// `cockpit`/`engine`/`window`/`glass` — is the part tied to one particular
/// model's node names, and a model swap silently turns it into a no-op that
/// still compiles. The luminance test is a property of the *material* and
/// survives, which is why it is the one this file keys off; the worst a swap
/// can do is move a panel between the two groups.
fn is_accent(base_color: Color) -> bool {
    let c = LinearRgba::from(base_color);
    0.2126 * c.red + 0.7152 * c.green + 0.0722 * c.blue < ACCENT_LUMA
}

// ---------------------------------------------------------------------------
// The pilot's own paint
// ---------------------------------------------------------------------------

/// `localStorage['spaceships:shipColor']` — `customization.js`'s key, and the
/// name this client keeps it under natively too.
pub(crate) const HULL_KEY: &str = "spaceships:shipColor";
/// `localStorage['spaceships:shipAccentColor']`.
pub(crate) const ACCENT_KEY: &str = "spaceships:shipAccentColor";

/// The colours the local pilot flies in. **The one copy of that fact.**
///
/// It used to have no copies at all, which is the bug behind *"there is supposed
/// to be a way to change the colour of your ship but it doesn't seem to work"*:
/// [`paint_for`] read the saved colour out of storage at the moment a ship
/// entity was spawned, and `ui.rs` wrote the player's choice to storage — but
/// natively `save_setting` wrote it precisely nowhere (it logged
/// `not persisted: the native build has no profile store`), and even on the web
/// nothing re-read it, so a colour picked during a match could not reach the
/// aircraft until the process was restarted. Two halves of one setting, neither
/// of which could see the other.
///
/// Now the livery is a resource: `ui.rs` writes it when the picker moves,
/// [`repaint_ships`] follows it onto the materials, [`persist_livery`] writes it
/// to disk (or to `localStorage`), and [`announce_livery`] tells the room. Every
/// one of those is driven by change detection on this one value.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub(crate) struct Livery {
    pub hull: Srgba,
    pub accent: Srgba,
}

impl Default for Livery {
    /// The authored defaults, and nothing from the environment or the disk —
    /// [`Livery::saved`] is the one that goes looking. Keeping `Default` pure is
    /// what lets the tests below assert against a known paint whatever is in the
    /// shell or in the player's config directory.
    fn default() -> Livery {
        Livery {
            hull: DEFAULT_HULL,
            accent: DEFAULT_ACCENT,
        }
    }
}

impl Livery {
    /// The livery this installation last saved, falling back to the defaults.
    ///
    /// The environment wins over the store on native, so
    /// `SPACESHIPS_HULL_COLOR=#c0ffee` still overrides for a one-off capture
    /// without overwriting what the pilot chose — the same rule `api.rs`'s
    /// credential store applies to `SPACESHIPS_TOKEN`, and for the same reason.
    pub(crate) fn saved() -> Livery {
        Livery {
            hull: saved_color(HULL_KEY, "SPACESHIPS_HULL_COLOR", DEFAULT_HULL),
            accent: saved_color(ACCENT_KEY, "SPACESHIPS_ACCENT_COLOR", DEFAULT_ACCENT),
        }
    }

    fn paint(self) -> ShipPaint {
        ShipPaint {
            hull: self.hull.into(),
            accent: self.accent.into(),
        }
    }
}

/// One saved colour: the environment, then the store, then the fallback.
fn saved_color(key: &str, native_env: &str, fallback: Srgba) -> Srgba {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(from_env) = std::env::var(native_env).ok().and_then(parse_hex) {
        return from_env;
    }
    #[cfg(target_arch = "wasm32")]
    let _ = native_env;

    settings::get(key).and_then(parse_hex).unwrap_or(fallback)
}

/// `#rrggbb`, as `customization.js` writes it. `Srgba::hex` also accepts the
/// form without the `#`, which is what a shell variable usually carries.
fn parse_hex(s: impl AsRef<str>) -> Option<Srgba> {
    Srgba::hex(s.as_ref().trim()).ok()
}

/// A colour as `#rrggbb`, the exact form the JS customizer stores, so a pilot
/// who picks a colour here and then opens the Three.js lobby sees the same one.
fn hex_string(c: Srgba) -> String {
    format!("#{:06x}", pack_rgb(c))
}

/// A colour as the wire's packed `0xRRGGBB`. `ClientMessage::Colors` carries an
/// integer, not a string.
pub(crate) fn pack_rgb(c: Srgba) -> u32 {
    let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    (to_u8(c.red) << 16) | (to_u8(c.green) << 8) | to_u8(c.blue)
}

/// Where this build keeps a setting between runs.
///
/// Two implementations behind one pair of functions, exactly as `api.rs`'s
/// credential store is arranged — and deliberately the *same keys* on both, so
/// the wasm client and `public/src/customization.js` share one `localStorage`
/// entry and a pilot's paint follows them between the two clients.
///
/// **What this is not is the account.** `PUT /api/colors` exists on the server
/// and would make the livery follow the pilot to another machine; wiring it up
/// is `api.rs`'s to do, and until it does, this store is per-installation.
pub(crate) mod settings {
    #[cfg(target_arch = "wasm32")]
    fn storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok().flatten()
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn get(key: &str) -> Option<String> {
        storage()?.get_item(key).ok().flatten()
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn set(key: &str, value: &str) {
        if let Some(store) = storage() {
            let _ = store.set_item(key, value);
        }
    }

    // -- native --------------------------------------------------------------

    /// `settings.json`, beside `api.rs`'s `credentials.json` in the user's own
    /// configuration directory, and honouring the same `SPACESHIPS_STATE_DIR`
    /// override — one state directory for the client, not two.
    ///
    /// Unlike the credential it is not `0600`: a hull colour is not a secret,
    /// and a settings file the user can read and edit is a feature.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn path() -> Option<std::path::PathBuf> {
        let dir = if let Some(explicit) = std::env::var_os("SPACESHIPS_STATE_DIR") {
            std::path::PathBuf::from(explicit)
        } else if cfg!(target_os = "macos") {
            std::path::PathBuf::from(std::env::var_os("HOME")?)
                .join("Library/Application Support/Spaceships")
        } else if cfg!(windows) {
            std::path::PathBuf::from(std::env::var_os("APPDATA")?).join("Spaceships")
        } else if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            std::path::PathBuf::from(xdg).join("spaceships")
        } else {
            std::path::PathBuf::from(std::env::var_os("HOME")?).join(".config/spaceships")
        };
        Some(dir.join("settings.json"))
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn get(key: &str) -> Option<String> {
        get_from(&path()?, key)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn set(key: &str, value: &str) {
        let Some(path) = path() else { return };
        if let Err(e) = set_in(&path, key, value) {
            bevy::log::warn!("scene: could not save {key}: {e}");
        }
    }

    // The two above resolve *where*; the two below do the work and take a path.
    // Split for the reason `api.rs` splits its own store: `std::env::set_var` is
    // process-global and cargo runs tests on threads, so a test that moved
    // `SPACESHIPS_STATE_DIR` would race every other test that reads the
    // environment.

    /// One key out of the store. A file that is missing, truncated, or not JSON
    /// reads as "nothing saved" — this is a preference, and refusing to start
    /// over a corrupt colour would be absurd.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn get_from(path: &std::path::Path, key: &str) -> Option<String> {
        let text = std::fs::read_to_string(path).ok()?;
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        Some(v.get(key)?.as_str()?.to_owned())
    }

    /// One key into the store, keeping every other key that is already there.
    ///
    /// Read-modify-write rather than a serialized struct, so that a key written
    /// by a newer build — or by hand — survives being loaded by an older one.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn set_in(
        path: &std::path::Path,
        key: &str,
        value: &str,
    ) -> Result<(), std::io::Error> {
        let mut doc = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| match v {
                serde_json::Value::Object(map) => Some(map),
                _ => None,
            })
            .unwrap_or_default();
        doc.insert(key.to_owned(), serde_json::Value::String(value.to_owned()));

        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::Value::Object(doc).to_string())
    }
}

/// What the store held when this process started, and has held since.
///
/// The reason it is a resource and not a `Local` in [`persist_livery`]: a
/// `Local` is first seen on the frame that system first runs, and by then
/// `Startup` has been and gone — so a livery chosen during startup (which is
/// what `SPACESHIPS_LIVERY` does) would look like the value that was already on
/// disk and never be written. Recording the store's own contents at plugin build
/// time makes "differs from what is saved" mean exactly that, whenever the
/// difference appeared.
#[derive(Resource, Debug, Clone, Copy)]
struct StoredLivery(Livery);

/// Writes the livery to the store when — and only when — it differs from what
/// is already there.
///
/// The comparison is what keeps a launch from rewriting the file with what it
/// has just read out of it — and, more to the point, what stops a pilot whose
/// saved hull came from the JS customizer's colour wheel having it quietly
/// rounded to the nearest entry of this client's paint box.
fn persist_livery(livery: Res<Livery>, mut stored: ResMut<StoredLivery>) {
    if stored.0 == *livery {
        return;
    }
    stored.0 = *livery;
    settings::set(HULL_KEY, &hex_string(livery.hull));
    settings::set(ACCENT_KEY, &hex_string(livery.accent));
}

// ---------------------------------------------------------------------------
// Everyone else's paint
// ---------------------------------------------------------------------------

/// The colours other pilots have announced, by entity id.
///
/// The `colors` message has been decoded since the crossplay work and then
/// thrown away — `net.rs` says as much in the arm that drops it: *"painting a
/// remote hull is `scene.rs`'s registry to grow"*. This is that registry. Until
/// it existed every other pilot in the room wore their team's colour and nothing
/// else, so a squadron of five looked like one aircraft five times.
#[derive(Resource, Debug, Default)]
struct RemotePaint(HashMap<sim::world::EntityId, ShipPaint>);

/// Applies `colors` frames as they arrive, and forgets a pilot when they leave.
///
/// Reads [`FromServer`] directly rather than asking `net.rs` to carry a new
/// event: the decoded frame is already published to the whole app as a Bevy
/// message, and a paint job is not simulation state — it must not go through
/// [`crate::net::NetInbox`], which is the queue the fixed tick consumes.
///
/// # The one thing borrowed from `net.rs`
///
/// The wire numbers players differently from the simulation: `IdSwap` trades
/// this connection's server id with [`LOCAL_ID`] so that "me" is the same id in
/// every module. That swap is private to `net.rs`, so [`to_entity`] below
/// reproduces it from [`NetSession::you`], which is public. It is self-inverse
/// and it is four lines, but it is still a second copy of a bijection — if
/// `IdSwap::to_entity` is ever made `pub`, this should call it instead.
fn read_remote_paint(
    mut incoming: MessageReader<FromServer>,
    session: Res<NetSession>,
    mut remote: ResMut<RemotePaint>,
) {
    for FromServer(msg) in incoming.read() {
        match msg {
            ServerMessage::Colors {
                id,
                hull_color,
                accent_color,
            } => {
                let Some(id) = to_entity(&session, *id) else {
                    continue;
                };
                remote.0.insert(
                    id,
                    ShipPaint {
                        hull: hex(*hull_color).into(),
                        accent: hex(*accent_color).into(),
                    },
                );
            }
            // The pilot is gone; their id will be handed to somebody else.
            ServerMessage::Disconnect { id } => {
                if let Some(id) = to_entity(&session, *id) {
                    remote.0.remove(&id);
                }
            }
            // A new room is a new set of ids. Nothing announced in the last one
            // describes anybody in this one.
            ServerMessage::Room { .. } => remote.0.clear(),
            _ => {}
        }
    }
}

/// A wire player id as an entity id. `net::IdSwap::apply`, from the outside.
fn to_entity(
    session: &NetSession,
    id: spaceships_protocol::PlayerId,
) -> Option<sim::world::EntityId> {
    let id = sim::world::EntityId::try_from(id).ok()?;
    let mine = session
        .you
        .and_then(|you| sim::world::EntityId::try_from(you).ok());
    Some(match mine {
        Some(mine) if id == mine => LOCAL_ID,
        Some(mine) if id == LOCAL_ID => mine,
        _ => id,
    })
}

/// Tells the room what colours this pilot is flying in.
///
/// The other half of [`read_remote_paint`], and just as necessary: a client that
/// applies everyone else's colours and never sends its own is a client whose
/// pilot is the only one who cannot be seen properly.
///
/// Sent on two occasions, which between them cover every way a peer can come to
/// need it — `main.js:778` sends it on exactly the second one:
///
/// - **The livery changed.** The room is already assembled and has to be told.
/// - **This client reached a room, or a match started.** Anyone already in the
///   room learns the colour of somebody who has just arrived, and anyone who
///   joins later gets it again when the match begins.
///
/// `flush_outbox` drops anything written while the socket is shut, so the phase
/// edge is not merely a convenience: it is the first moment a send can land.
fn announce_livery(
    livery: Res<Livery>,
    session: Res<NetSession>,
    mut outbox: MessageWriter<ToServer>,
    mut known: Local<Option<(Livery, Phase)>>,
) {
    let now = (*livery, session.phase);
    let seated = matches!(now.1, Phase::Room | Phase::Playing);
    match *known {
        Some(before) if before == now => return,
        // The first observation is only worth announcing if there is already
        // somebody to hear it.
        None if !seated => {
            *known = Some(now);
            return;
        }
        _ => *known = Some(now),
    }
    if !seated {
        return;
    }
    outbox.write(ToServer(ClientMessage::Colors {
        hull_color: pack_rgb(livery.hull),
        accent_color: pack_rgb(livery.accent),
    }));
}

/// Handles the sync systems need but must not reload every frame.
#[derive(Resource)]
struct SceneAssets {
    /// The glTF scene inside `spaceship.glb`.
    ship: Handle<WorldAsset>,
    /// Six deformed icospheres, matching `asteroids.js`'s six variants.
    rock_meshes: Vec<Handle<Mesh>>,
    /// One material for the whole field. See [`DamageFlash`].
    rock_material: Handle<RockMaterial>,
}

pub struct ScenePlugin;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Registry>()
            .init_resource::<RemotePaint>()
            // Not `init_resource`: the livery is whatever this installation last
            // saved, and `Livery::default()` is deliberately the authored pair
            // and nothing else. See [`Livery::saved`].
            .insert_resource(Livery::saved())
            .insert_resource(StoredLivery(Livery::saved()))
            // The asteroid field's material. One `MaterialPlugin` per material
            // *type*, not per material — the sixty rocks still share one asset.
            .add_plugins(MaterialPlugin::<RockMaterial>::default())
            .add_systems(Startup, setup)
            // `PreUpdate`, after the socket has published the frame's traffic:
            // a `colors` that arrives this frame is applied this frame, before
            // anything in `Update` or `FixedUpdate` reads the registry.
            .add_systems(PreUpdate, read_remote_paint.after(NetSet::Receive))
            // Sampling is per *tick*. `FixedUpdate`, after `SimSet`, is the
            // only place `prev` and `curr` are guaranteed to be consecutive
            // ticks — see the module docs on why sampling in `Update` freezes
            // on a frame that ran no tick and lurches on one that ran two.
            // `flash_ships` is chained after the samplers because it is driven
            // by `Changed<HitFlash>`, and `sample_ships` is what changes it —
            // and `repaint_ships` for the same reason, one component along.
            .add_systems(
                FixedUpdate,
                ((sample_ships, sample_rocks), (flash_ships, repaint_ships))
                    .chain()
                    .after(SimSet),
            )
            // Both are edge-triggered off `Livery`, and neither belongs on the
            // tick: picking a colour is something a person does between matches,
            // and `announce_livery` has to be able to write on a frame that ran
            // no tick at all.
            .add_systems(Update, (persist_livery, announce_livery))
            // Drawing is per *frame*. `AfterFixedMainLoop` is where Bevy's own
            // docs put this: the fixed loop has finished, so
            // `overstep_fraction` is the leftover accumulator and nothing else
            // will consume it, and it still lands before `Update` and
            // `PostUpdate` — which is to say before the chase camera reads the
            // scene and before `TransformSystems::Propagate`.
            .add_systems(
                RunFixedMainLoop,
                draw_interpolated.in_set(RunFixedMainLoopSystems::AfterFixedMainLoop),
            )
            // Per *frame*, not per tick: the moon is scenery, nothing reads its
            // transform, and `draw_interpolated` has no `Interp` on it to fight
            // over. `Update` also runs before `PostUpdate`'s propagation, so the
            // rotation lands in the same frame it is applied.
            .add_systems(Update, (report_batches, spin_bodies));
    }
}

// ---------------------------------------------------------------------------
// Draw-call instrumentation
// ---------------------------------------------------------------------------

/// `SPACESHIPS_BATCHES=1` logs the scene's batch keys once, four seconds in.
///
/// Bevy collapses the opaque phase by `(mesh, material)` — instances sharing a
/// pair are drawn as one indirect call — so the number of distinct pairs is
/// what a draw-call count is *counting*, and it is the number to watch when
/// adding a per-entity effect. If it tracks the entity count, the effect was
/// built the way `asteroids.js` builds its damage tint and the port has
/// reproduced the thing it exists to avoid.
///
/// Hand-rolled for the same reason as `main.rs`'s frame-time readout: a
/// diagnostic this cheap is not worth a plugin tree, and wgpu offers no draw
/// call counter to read instead.
fn report_batches(
    time: Res<Time>,
    mut done: Local<bool>,
    standard: Query<(&Mesh3d, &MeshMaterial3d<StandardMaterial>)>,
    rocks: Query<(&Mesh3d, &MeshMaterial3d<RockMaterial>)>,
) {
    if *done || time.elapsed_secs() < 4.0 || std::env::var_os("SPACESHIPS_BATCHES").is_none() {
        return;
    }
    *done = true;

    let mut keys = std::collections::BTreeSet::new();
    let mut n = 0usize;
    for (mesh, mat) in &standard {
        n += 1;
        keys.insert((format!("{:?}", mesh.id()), format!("{:?}", mat.id())));
    }
    let standard_keys = keys.len();
    for (mesh, mat) in &rocks {
        n += 1;
        keys.insert((format!("{:?}", mesh.id()), format!("{:?}", mat.id())));
    }
    info!(
        "batches: {n} mesh entities -> {} batch keys ({standard_keys} standard, {} rock)",
        keys.len(),
        keys.len() - standard_keys,
    );
}

// ---------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------

fn setup(
    mut commands: Commands,
    assets: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut rock_materials: ResMut<Assets<RockMaterial>>,
    mut shaders: ResMut<Assets<Shader>>,
) {
    let rules = sim::rules::Rules::DEFAULT;

    // The space rig, so the first frame is lit whatever the map turns out to
    // be. [`crate::terrain::apply_map`] owns the rig from the first `Update`
    // onward and swaps this out for the terrain sun when it has to — see
    // [`MapScenery`].
    install_space_lights(&mut commands, &rules);

    // -- The moon -------------------------------------------------------------
    // `World::new` already put the collision sphere at the origin; this is only
    // its mesh. Radius comes from the rules so the two can never disagree.
    let moon_r = rules.world.moon_radius as f32;
    commands.spawn((
        Mesh3d(meshes.add(moon_mesh(moon_r))),
        MeshMaterial3d(materials.add(StandardMaterial {
            // **Not** `moon.js`'s texture, and deliberately.
            //
            // The JS loads `sounds/Moon2.jpeg`, a 300x188 tiling crater photo on
            // `RepeatWrapping`. It has to: it maps onto an `IcosahedronGeometry`,
            // whose UVs are per-face scraps with no global layout, so the only
            // image that survives them is one with no layout to lose.
            //
            // This moon is a UV sphere, which has true equirectangular UVs — so
            // the asset that belongs on it is an equirectangular albedo, and
            // `public/moon Texture.jpg` is exactly that at 2048x1024. It gives
            // real maria and ray systems in the right places instead of the same
            // crater field tiled, at 37x the texel count. `Moon2.jpeg` on these
            // UVs would be one stretched, pole-pinched smear.
            //
            // The honest cost: at the ~150 px the moon covers from spawn, the
            // JS's crater photo reads *crisper*, because it is a high-contrast
            // detail shot repeated at high frequency while this is a soft albedo
            // map resolved down. The trade is deliberate — the moon sits at the
            // centre of the map and players fly around it, and at close range a
            // coherent 2048px map is the one that holds up.
            //
            // Nothing in `public/src/` references `moon Texture.jpg`; it is an
            // asset the JS shipped and never used.
            base_color_texture: Some(sharp_texture(&assets, "moon Texture.jpg")),
            // **A grey, where `moon.js:23` has `color: 0xffffff` — because the
            // two textures are not the same brightness.**
            //
            // `Moon2.jpeg` is a mid-grey photograph; `moon Texture.jpg` is a
            // near-white albedo map. Multiplied by the same white, the second is
            // 3.4x the radiance of the first — measured, by rendering the JS
            // moon with each map under Ultra and comparing the lit hemisphere.
            // At that brightness the fill light alone carries the shadow side
            // past the point where it reads as shadow, so the moon loses its
            // terminator and goes flat: a bright sticker rather than a body.
            //
            // 0.3 is the reciprocal of that measurement, so this moon sits at
            // the luminance the rig was balanced against. It is a correction for
            // the asset swap and nothing more: it is on this one material, and
            // no other object in the scene sees it.
            base_color: LinearRgba::rgb(0.3, 0.3, 0.3).into(),
            perceptual_roughness: 0.95,
            // `moon.js:25` is `metalness: 0.02`.
            metallic: 0.02,
            ..default()
        })),
        Transform::from_xyz(
            rules.world.moon_pos.x as f32,
            rules.world.moon_pos.y as f32,
            rules.world.moon_pos.z as f32,
        )
        // Stand the moon up. Bevy's UV sphere puts its poles on ±Z, and the
        // match runs down the Z axis — spawns at ∓540, motherships at ±600 — so
        // the pole pointed straight down the flight corridor and the face
        // everyone saw was the one part of an equirectangular map with nothing
        // on it. A quarter turn about X sends the pole to +Y and turns the
        // equator, where the maria are, toward the player.
        .with_rotation(Quat::from_rotation_x(-FRAC_PI_2)),
        // `moon.js:31`.
        Spin(Vec3::new(0.005, 0.012, 0.003)),
        // `graphics.js`: "Big background props stay out of the shadow pass."
        NotShadowCaster,
        // `main.js:182` — `isTerrainMap ? null : createMoon(..)`. The Sierras
        // have no moon, and `sim::world::World::new` agrees: its obstacle list
        // is empty on that map.
        SpaceScenery,
    ));

    // -- Motherships ----------------------------------------------------------
    install_motherships(&mut commands, &mut meshes, &mut materials, &rules);

    // -- Shared handles -------------------------------------------------------
    // The rock shader is compiled in rather than loaded; see
    // [`DAMAGE_FLASH_SHADER`] for why. Inserting it here rather than in
    // `Plugin::build` keeps every asset this module owns in one place, and is
    // still long before the first frame the pipeline is specialized for.
    shaders
        .insert(
            &DAMAGE_FLASH_SHADER,
            Shader::from_wgsl(DAMAGE_FLASH_WGSL, "spaceships/damage_flash.wgsl"),
        )
        .expect("a uuid handle has no generation to be stale");

    commands.insert_resource(SceneAssets {
        // 0.19: `GltfAssetLabel::Scene(0).from_asset(..)` is unchanged, but the
        // asset it resolves to is a `WorldAsset` (was `Scene`) and the
        // component that instantiates it is `WorldAssetRoot` (was `SceneRoot`).
        ship: assets.load(bevy::gltf::GltfAssetLabel::Scene(0).from_asset(ship_model())),
        rock_meshes: (0..rules.world.asteroid_field.variant_count)
            .map(|v| meshes.add(rock_mesh(v)))
            .collect(),
        rock_material: rock_materials.add(RockMaterial {
            base: StandardMaterial {
                // `asteroids.js` maps `sounds/asteroid.jpg` with a 1.5x repeat.
                base_color_texture: Some(sharp_texture(&assets, "sounds/asteroid.jpg")),
                perceptual_roughness: 0.95,
                metallic: 0.05,
                ..default()
            },
            extension: DamageFlash {
                flash: Vec4::new(ROCK_FLASH.red, ROCK_FLASH.green, ROCK_FLASH.blue, 1.0),
                scorch: Vec4::new(ROCK_SCORCH.red, ROCK_SCORCH.green, ROCK_SCORCH.blue, 0.0),
            },
        }),
    });
}

/// The two team hulls from `mothership.js`, at `±mothership_z`.
///
/// Every dimension is derived from [`WorldRules::mothership_half`], which is
/// the box `resolve_world_collisions` already bounces ships off — so the thing
/// you see and the thing you hit cannot drift apart. It happens to be exactly
/// the JS's `W = 90, H = 36, L = 70`, which is the point.
///
/// Meshes and materials are built once and shared by both ships, so the pair
/// costs the batch keys of one. The hangar mouth is at local `+z` and the far
/// mothership is turned to face the middle, matching `main.js:158`.
///
/// [`WorldRules::mothership_half`]: sim::rules::WorldRules::mothership_half
fn install_motherships(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    rules: &sim::rules::Rules,
) {
    let half = rules.world.mothership_half;
    let (w, h, l) = (
        half.x as f32 * 2.0,
        half.y as f32 * 2.0,
        half.z as f32 * 2.0,
    );

    // Straight off `mothership.js` — and deliberately *not* the ship's
    // `0.55 / 0.34`. Ultra's `sweepScene` only passes `metalness`/`roughness`
    // for the object literally named `Ship`; everything else keeps what it was
    // authored with and only picks up the anisotropy and env-intensity sweep.
    let hull = materials.add(StandardMaterial {
        base_color: hex(0x4a_5366).into(),
        perceptual_roughness: 0.5,
        metallic: 0.55,
        ..default()
    });
    let accent = materials.add(StandardMaterial {
        base_color: hex(0x22_2a36).into(),
        perceptual_roughness: 0.7,
        metallic: 0.45,
        ..default()
    });
    let recess = materials.add(StandardMaterial {
        base_color: hex(0x0a_1220).into(),
        perceptual_roughness: 0.9,
        ..default()
    });
    // The three `MeshBasicMaterial`s. See [`glow`].
    let engine_glow = materials.add(glow(hex(0xff_7733), GLOW_BOOST));
    let door_glow = materials.add(StandardMaterial {
        // The plane's `opacity: 0.65` folded into the colour, because the
        // additive blend has no use for an alpha channel and leaving it there
        // only invites a question about which of the two is doing the work.
        alpha_mode: AlphaMode::Add,
        ..glow(hex(0x66_ccff), GLOW_BOOST * 0.65)
    });
    let runway = materials.add(glow(hex(0xff_d97a), GLOW_BOOST));

    let hull_mesh = meshes.add(Cuboid::new(w, h, l));
    let stripe_mesh = meshes.add(Cuboid::new(w * 1.005, 1.6, l * 1.005));
    let ring_mesh = meshes.add(Cylinder::new(3.5, 2.0));
    let ring_glow_mesh = meshes.add(Circle::new(3.2));

    // The hangar mouth. `mothership.js:33` onwards, with the depths named so
    // the shield plane's own z can be *derived* from the two solids it has to
    // sit in front of rather than transcribed — see `door_z`.
    let (door_w, door_h) = (32.0f32, 18.0f32);
    let (frame_z, frame_d) = (l / 2.0 + 0.1, 1.2);
    let (inset_z, inset_d) = (l / 2.0 - 2.0, 6.0);
    let frame_mesh = meshes.add(Cuboid::new(door_w + 4.0, door_h + 4.0, frame_d));
    let inset_mesh = meshes.add(Cuboid::new(door_w, door_h, inset_d));
    let door_mesh = meshes.add(Rectangle::new(door_w - 1.0, door_h - 1.0));
    let lamp_mesh = meshes.add(Sphere::new(0.4).mesh().uv(6, 4));

    // **The one number here that is not the JS's.**
    //
    // `mothership.js:56` puts the shield plane at `L / 2 + 0.55`, which is
    // behind *both* solids at the mouth: the frame slab's front face is at
    // `L / 2 + 0.7` and the recess box's is at `L / 2 + 1.0`, so the plane is
    // depth-rejected and draws nothing. Confirmed on screen — the blue in the
    // JS hangar is its point light, and the additive plane it is supposedly
    // there for has never been visible.
    //
    // Derived rather than transcribed so it cannot come loose if either solid
    // is retuned, and called out because everything else about this hull is a
    // transcription: a silent 0.65 would be exactly the kind of drift
    // `rules.rs` exists to prevent.
    let door_z = (frame_z + frame_d / 2.0).max(inset_z + inset_d / 2.0) + 0.2;

    for z in [-rules.world.mothership_z, rules.world.mothership_z] {
        let facing = if z > 0.0 {
            Quat::from_rotation_y(PI)
        } else {
            Quat::IDENTITY
        };
        commands
            .spawn((
                Transform::from_xyz(0.0, 0.0, z as f32).with_rotation(facing),
                Visibility::default(),
                // `graphics.js`: big background props stay out of the shadow
                // pass. A 90-unit box 600 units out contributes nothing a
                // two-cascade map can resolve.
                NotShadowCaster,
                // `main.js:142` swaps these for `createAirfield` on the terrain
                // map. The collision boxes swap with them in
                // `sim::world::World::new`, so hiding the mesh is the whole of
                // the renderer's half.
                SpaceScenery,
            ))
            .with_children(|hull_of| {
                hull_of.spawn((Mesh3d(hull_mesh.clone()), MeshMaterial3d(hull.clone())));

                for y in [-10.0f32, 10.0] {
                    hull_of.spawn((
                        Mesh3d(stripe_mesh.clone()),
                        MeshMaterial3d(accent.clone()),
                        Transform::from_xyz(0.0, y, 0.0),
                    ));
                }

                // Three engine bells out the back, each with an additive disc
                // behind it. Bevy's `Cylinder` is Y-up like three's, so the
                // quarter-turn about X is the same correction.
                for x in [-22.0f32, 0.0, 22.0] {
                    hull_of.spawn((
                        Mesh3d(ring_mesh.clone()),
                        MeshMaterial3d(accent.clone()),
                        Transform::from_xyz(x, -2.0, -l / 2.0 - 0.5)
                            .with_rotation(Quat::from_rotation_x(FRAC_PI_2)),
                    ));
                    hull_of.spawn((
                        Mesh3d(ring_glow_mesh.clone()),
                        MeshMaterial3d(engine_glow.clone()),
                        Transform::from_xyz(x, -2.0, -l / 2.0 - 1.6)
                            .with_rotation(Quat::from_rotation_y(PI)),
                    ));
                }

                // The hangar mouth: a raised frame, a dark recess, and the
                // shield plane across it.
                hull_of.spawn((
                    Mesh3d(frame_mesh.clone()),
                    MeshMaterial3d(accent.clone()),
                    Transform::from_xyz(0.0, 0.0, frame_z),
                ));
                hull_of.spawn((
                    Mesh3d(inset_mesh.clone()),
                    MeshMaterial3d(recess.clone()),
                    Transform::from_xyz(0.0, 0.0, inset_z),
                ));
                hull_of.spawn((
                    Mesh3d(door_mesh.clone()),
                    MeshMaterial3d(door_glow.clone()),
                    Transform::from_xyz(0.0, 0.0, door_z),
                    NotShadowCaster,
                ));
                hull_of.spawn((
                    PointLight {
                        color: hex(0x66_ccff).into(),
                        intensity: 4.0e6,
                        range: 100.0,
                        // 0.19's name for `shadows_enabled`, same as on
                        // `DirectionalLight` above.
                        shadow_maps_enabled: false,
                        ..default()
                    },
                    Transform::from_xyz(0.0, 0.0, l / 2.0 + 6.0),
                ));

                // Approach lights along the lip, six a side.
                for i in [-3i32, -2, -1, 1, 2, 3] {
                    for y in [-10.0f32, 10.0] {
                        hull_of.spawn((
                            Mesh3d(lamp_mesh.clone()),
                            MeshMaterial3d(runway.clone()),
                            Transform::from_xyz(
                                (i as f32 / 3.0) * (w / 2.0 - 4.0),
                                y,
                                l / 2.0 + 0.3,
                            ),
                        ));
                    }
                }
            });
    }
}

/// A `MeshBasicMaterial` stand-in: unlit, with its colour pushed past 1.0.
///
/// Three.js "basic" is unlit and writes its colour straight out. Bevy's
/// spelling is `unlit`, which takes `base_color` and skips lighting — and
/// **skips `emissive` with it**, since emissive is added inside
/// `apply_pbr_lighting`. So the brightness has to live in `base_color` and
/// nowhere else; an `unlit` material with a bright emissive and a dim
/// base_color renders dim, silently, which is the trap this function exists to
/// close.
///
/// Which is also exactly what the JS does. `upgradeMaterials`
/// (`graphics.js:413`) reaches into every additive basic material and calls
/// `m.color.multiplyScalar(glowBoost)` — on the linear colour, three.js having
/// stored it that way since r152 — because "pushing the colour past 1.0 is what
/// makes the bloom pass actually bite".
pub(crate) fn glow(color: Srgba, scale: f32) -> StandardMaterial {
    let c = LinearRgba::from(color);
    StandardMaterial {
        base_color: LinearRgba::rgb(c.red * scale, c.green * scale, c.blue * scale).into(),
        unlit: true,
        ..default()
    }
}

/// Loads a texture with Ultra's 8x anisotropic filtering.
///
/// `upgradeMaterials` walks every loaded material and raises `anisotropy` on
/// `map`, `emissiveMap`, `roughnessMap`, `metalnessMap`, and `normalMap`. Bevy
/// has no equivalent post-hoc sweep, so the sampler is set at load time
/// instead — which only reaches textures this crate loads by name, not the ones
/// inside the glTF. See [`ultra_material_sweep`] for those.
///
/// **Caveat worth knowing:** anisotropic filtering only does anything on a
/// mipmapped texture, and Bevy 0.19's image loader does not generate mip chains
/// for ordinary PNG/JPEG assets the way `THREE.TextureLoader` does. Until the
/// assets carry their own mips (ktx2) or a mip generator runs at load, this is
/// the correct sampler on a texture that has nothing to filter between.
fn sharp_texture(assets: &AssetServer, path: &str) -> Handle<Image> {
    // 0.19 deprecated `load_with_settings` in favour of this builder.
    assets
        .load_builder()
        .with_settings(|s: &mut ImageLoaderSettings| {
            s.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                anisotropy_clamp: ANISOTROPY,
                ..ImageSamplerDescriptor::linear()
            });
        })
        .load(path.to_owned())
}

/// The three-point rig from `graphics.js`'s `installSpaceLights`.
///
/// Three.js `DirectionalLight` intensity is a unitless multiplier; Bevy's
/// `illuminance` is lux. Only the *ratios* carry over, so the key is anchored
/// at a plausible sun and the other two are scaled from it by the same factors
/// the JS uses (0.45 and 0.30 against 2.7).
///
/// A Three.js light at position `P` shines from `P` toward the origin; a Bevy
/// `DirectionalLight` shines along its transform's -Z. Hence
/// `from_translation(P).looking_at(ZERO)`.
///
/// Every light here is tagged [`MapScenery`], and so is the terrain sun. The
/// two rigs are mutually exclusive — `main.js:107` branches between them — so
/// [`crate::terrain::apply_map`] can install one by despawning the other with
/// no knowledge of what it is replacing.
pub(crate) fn install_space_lights(commands: &mut Commands, rules: &sim::rules::Rules) {
    const KEY_LUX: f32 = 9_000.0;

    // Key: warm, high, from the front-right.
    commands.spawn((
        DirectionalLight {
            color: Color::srgb_u8(0xff, 0xf2, 0xdd),
            illuminance: KEY_LUX,
            // 0.19 renamed this from `shadows_enabled`.
            //
            // Off, matching the JS: `installSpaceLights` sets
            // `renderer.shadowMap.enabled` but never sets `castShadow` on any
            // of the three lights, so Ultra's *space* map casts no shadows at
            // all. Only `upgradeTerrainSun` configures a shadow map, and that
            // is the terrain map's sun.
            //
            // Measured on an M5 at 1280x720, turning three cascades on and off
            // moved the frame time by less than the measurement noise, so this
            // is parity rather than a performance fix — it buys self-shadowing
            // that a directional light at this distance barely resolves.
            // `ShadowFilteringMethod::Gaussian` stays on the camera so the
            // PCF-soft path is already wired when the terrain sun lands.
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_xyz(200.0, 300.0, 100.0).looking_at(Vec3::ZERO, Vec3::Y),
        // Sized for the field rather than for a human-scale scene, so that
        // turning shadows on is a one-word change rather than a retune. The
        // default `maximum_distance` would put every cascade inside the first
        // 100 units and resolve nothing.
        CascadeShadowConfigBuilder {
            num_cascades: 2,
            maximum_distance: rules.world.asteroid_field.radius as f32,
            first_cascade_far_bound: 60.0,
            ..default()
        }
        .build(),
        MapScenery,
    ));

    // Fill: cool bounce from the opposite side. "Kept gentle — a strong blue
    // fill against the warm key turns grey rock violet."
    commands.spawn((
        DirectionalLight {
            color: Color::srgb_u8(0x7a, 0xa8, 0xe0),
            illuminance: KEY_LUX * (0.45 / 2.7),
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_xyz(-260.0, -120.0, -180.0).looking_at(Vec3::ZERO, Vec3::Y),
        MapScenery,
    ));

    // Rim: warm, from behind, for silhouette separation against the nebula.
    commands.spawn((
        DirectionalLight {
            color: Color::srgb_u8(0xff, 0xa0, 0x60),
            illuminance: KEY_LUX * (0.30 / 2.7),
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_xyz(-80.0, 60.0, -320.0).looking_at(Vec3::ZERO, Vec3::Y),
        MapScenery,
    ));

    // `HemisphereLight(0x9fb4d0, 0x2b2f3a, 0.55)`. Bevy has no hemisphere
    // light; the sky/ground split is what an environment map does, and
    // `skybox.rs` installs a `GeneratedEnvironmentMapLight` from the nebula
    // cubemap — which is exactly what `applyEnvironment`'s PMREM pass does in
    // the JS. This ambient is the neutral lift underneath it.
    //
    // **This was 120, and that is why everything unlit went black.** The other
    // three lights are all anchored to the key by the JS's own ratios; this one
    // was not, and it came out an order of magnitude short. Derived the same way
    // as the others:
    //
    // three's hemisphere light gives a surface `mix(ground, sky, 0.5 * n.y +
    // 0.5) * intensity`, so averaged over every normal on a sphere it is
    // `(sky + ground) / 2 * 0.55` = `(0.217, 0.245, 0.289)`. Against the key's
    // `0xfff2dd * 2.7` = `(2.70, 2.56, 2.34)` that is 8-12% of key. Bevy
    // multiplies `color * brightness` for its own irradiance, so matching that
    // fraction of [`KEY_LUX`] through `0x9fb4d0` wants `brightness` near 1160 —
    // and all three channels agree on it to within 2%.
    //
    // At 120 the fill and rim were carrying the entire shadow side on their own,
    // which is a tenth of what the JS gives it: the moon's dark limb vanished
    // into the sky rather than reading as a dark limb, and every hull turned
    // away from the key was a silhouette.
    commands.insert_resource(GlobalAmbientLight {
        color: Color::srgb_u8(0x9f, 0xb4, 0xd0),
        brightness: 1160.0,
        ..default()
    });
}

// ---------------------------------------------------------------------------
// Per-tick sampling
// ---------------------------------------------------------------------------

/// Whether the simulation announced that this ship was placed rather than
/// moved this tick.
///
/// `SimEvent::ShipRespawned` is the simulation saying so itself, which is the
/// signal to prefer: respawn and the campaign warp both go through it, and it
/// needs no threshold. It is a linear scan of a list that holds single digits,
/// against a ship list that holds ten, so there is nothing to index here.
fn was_placed(frame: &sim::world::Frame, id: sim::world::EntityId) -> bool {
    frame.events.iter().any(|e| {
        matches!(
            e,
            sim::world::SimEvent::ShipRespawned { id: respawned, .. } if *respawned == id
        )
    })
}

fn sample_ships(
    mut commands: Commands,
    frame: Res<SimFrame>,
    scene: Res<SceneAssets>,
    livery: Res<Livery>,
    remote: Res<RemotePaint>,
    mut reg: ResMut<Registry>,
    mut q: ShipSample,
) {
    for view in &frame.0.ships {
        // Boss hitboxes are never drawn — they exist so one damage path can
        // serve the capital ship too.
        if view.flags.contains(sim::world::ShipFlags::BOSS_HITBOX) {
            continue;
        }

        let pose = Pose::of_ship(view);
        // Recomputed every tick rather than only at spawn: a livery the pilot
        // has just changed, a `colors` that has just landed, and a team the
        // server has just assigned all change the answer under a ship that is
        // already flying. `set_if_neq` below is what keeps that from touching
        // ten materials a tick.
        let paint = paint_for(view, &livery, &remote);

        let entity = *reg.ships.entry(view.id).or_insert_with(|| {
            commands
                .spawn((
                    ShipRoot(view.id),
                    pose.transform(),
                    // First tick: no previous pose, so both ends are this one.
                    Interp::spawned(pose),
                    Visibility::default(),
                    HitFlash::default(),
                    paint,
                ))
                .with_children(|ship| {
                    ship.spawn((
                        WorldAssetRoot(scene.ship.clone()),
                        // `ship.js:45` does exactly this: the model's nose
                        // rests along +x (its `gun` node is at x = 3.81), and
                        // the simulation says the nose is local +z. Correcting
                        // it on a child keeps the root transform the
                        // simulation's own.
                        {
                            let (fit_scale, fit_offset) = model_fit(&ship_model());
                            Transform::from_rotation(Quat::from_rotation_y(-FRAC_PI_2))
                                .with_scale(Vec3::splat(fit_scale))
                                .with_translation(
                                    Quat::from_rotation_y(-FRAC_PI_2) * (fit_offset * fit_scale),
                                )
                        },
                    ))
                    // Paint and Ultra's material treatment, once the glTF
                    // hierarchy actually exists.
                    //
                    // On the *model* entity, not the ship root:
                    // `WorldInstanceReady` is triggered on the entity that
                    // holds the `WorldAssetRoot`, it does not propagate, and an
                    // observer on the parent therefore never runs. That is why
                    // this is `with_children` and not `with_child` — the latter
                    // hands back the parent's `EntityCommands`.
                    .observe(paint_and_upgrade);
                })
                .id()
        });

        // Miss on the tick the entity was spawned — `Interp::spawned` above
        // already holds this pose, and the commands have not been applied yet.
        if let Ok((mut interp, mut vis, mut flash, mut worn, marked)) = q.get_mut(entity) {
            if marked {
                commands.entity(entity).remove::<Snap>();
            }
            if marked || was_placed(&frame.0, view.id) {
                interp.snap(pose);
            } else {
                interp.advance(pose);
            }

            // Discrete, so it is set on the tick and never blended.
            *vis = if view.flags.contains(sim::world::ShipFlags::ALIVE) {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            };

            // `set_if_neq` and not a plain write: this is what keeps
            // `flash_ships` off the material assets of the nine ships nobody
            // shot this tick, and `repaint_ships` off the ten nobody recoloured.
            flash.set_if_neq(HitFlash(view.hit_flash));
            worn.set_if_neq(paint);
        }

        // TODO(trails): `BOOSTING`/`BRAKING` are already in `view.flags` and are
        // what `trails.js` emits from. See the module docs on batching before
        // adding a mesh per trail segment.
    }

    // A ship that left the frame left the match.
    reg.ships.retain(|id, entity| {
        let live = frame.0.ships.iter().any(|s| s.id == *id);
        if !live {
            commands.entity(*entity).despawn();
        }
        live
    });
}

/// `applyColorsToShip` and Ultra's `upgradeMaterials`, in one walk of one ship.
///
/// Fires on [`WorldInstanceReady`], because until the glTF has been
/// instantiated there is no hierarchy to walk. `ready.entity` is the entity
/// carrying [`WorldAssetRoot`] — the ship root's child — so the paint is read
/// back up through [`ChildOf`].
///
/// **Every material is cloned.** The glTF's own materials are shared by every
/// instance of the model, so painting one in place would paint all ten ships
/// the same colour — and the old in-place sweep had a quieter version of the
/// same bug, multiplying the shared `emissive` by `glowBoost` once *per ship*.
/// The clone is per ship, not per mesh entity: a model whose hull is four
/// meshes over one material still ends up with one hull material.
///
/// This is the exception the module docs allow, and it does not generalise —
/// see [`DamageFlash`] for the sixty-rock case, where cloning is exactly the
/// bottleneck this port exists to escape.
fn paint_and_upgrade(
    ready: On<WorldInstanceReady>,
    mut commands: Commands,
    children: Query<&Children>,
    parents: Query<&ChildOf>,
    paints: Query<&ShipPaint>,
    meshes: Query<&MeshMaterial3d<StandardMaterial>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let Ok(&ChildOf(root)) = parents.get(ready.entity) else {
        return;
    };
    let Ok(paint) = paints.get(root) else {
        return;
    };

    // Source material -> the clone this ship uses, so meshes sharing an
    // authored material keep sharing one here.
    let mut cloned: HashMap<AssetId<StandardMaterial>, Handle<StandardMaterial>> = HashMap::new();
    let mut skin = Vec::new();

    for descendant in children.iter_descendants(ready.entity) {
        let Ok(MeshMaterial3d(source)) = meshes.get(descendant) else {
            continue;
        };

        let handle = match cloned.get(&source.id()) {
            Some(handle) => handle.clone(),
            None => {
                let Some(mut mat) = materials.get(source).cloned() else {
                    continue;
                };

                // -- The paint ------------------------------------------------
                //
                // The split is read from the *authored* colour and then
                // remembered on the `SkinPart`, because this is the only moment
                // it can be read: from here on the material wears a livery, and
                // asking the same question of a painted material gives a
                // different answer. See [`SkinPart::accent`].
                let accent = is_accent(mat.base_color);
                let painted = if accent { paint.accent } else { paint.hull };
                // Alpha is the model's, not the palette's: a canopy authored
                // translucent must stay translucent.
                mat.base_color = painted.with_alpha(mat.base_color.alpha());

                // -- Ultra's `sweepScene` -------------------------------------
                // Ultra's `sweepScene` gives ships `metalness: 0.55,
                // roughness: 0.34`, and forcing that here overrode whatever the
                // model was authored with. Those numbers were tuned against
                // `spaceship.glb` -- six flat-shaded Blender primitives, where a
                // hard specular reads as sci-fi panelling. On a smooth aircraft
                // hull the same values read as polished chrome, which is not
                // what a painted airframe looks like.
                //
                // The model's own values now stand. `jet.glb` carries 0.0 / 0.72
                // for the hull and 0.1 / 0.22 for the canopy glass; the old ship
                // carries 0.0 / 0.5. Both are sane, and a future model that
                // wants a metal finish can simply say so.
                //
                // Everything else Ultra does -- anisotropy, the environment
                // intensity sweep, the emissive boost -- still applies below.
                // `emissiveIntensity *= glowBoost` (1.7) — pushing emissive
                // past 1.0 is what makes the bloom pass bite. Safe to apply
                // unconditionally now that the material is this ship's own.
                mat.emissive *= 1.7;

                // The anisotropy half of `upgradeMaterials`. Textures inside
                // the glTF were loaded by the gltf loader with its own sampler,
                // so they are patched here rather than at load time. The images
                // stay shared — only the materials are cloned.
                for tex in [
                    mat.base_color_texture.as_ref(),
                    mat.emissive_texture.as_ref(),
                    mat.metallic_roughness_texture.as_ref(),
                    mat.normal_map_texture.as_ref(),
                ]
                .into_iter()
                .flatten()
                {
                    if let Some(mut image) = images.get_mut(tex) {
                        image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                            anisotropy_clamp: ANISOTROPY,
                            ..ImageSamplerDescriptor::linear()
                        });
                    }
                }

                let emissive = mat.emissive;
                let handle = materials.add(mat);
                skin.push(SkinPart {
                    material: handle.clone(),
                    emissive,
                    accent,
                });
                cloned.insert(source.id(), handle.clone());
                handle
            }
        };

        commands
            .entity(descendant)
            .insert(MeshMaterial3d(handle.clone()));
    }

    commands.entity(root).insert(ShipSkin(skin));
}

/// Drives a struck ship's emissive from `view.hit_flash`.
///
/// The JS has no equivalent — `hitFlash` is asteroid-only there, and a player
/// being hit is communicated with a red screen vignette, which tells you
/// nothing about *someone else* taking fire. Under a pipeline with bloom, an
/// emissive pulse is the read: the hull flares for the quarter-second the
/// simulation's flash lasts and the bloom pass smears it, so a hit lands
/// visibly from across the field.
///
/// `Changed<HitFlash>` is doing real work. Writing a `StandardMaterial` marks it
/// modified, which re-prepares its bind group; without the filter this would do
/// that for every ship on every tick forever.
///
/// `Changed<ShipSkin>` is the other half, and it is not decoration. The skin is
/// inserted by [`paint_and_upgrade`] when the glTF finishes loading, which is
/// tens of frames after the ship entity exists — so on `HitFlash` alone, a
/// flash that arrived while the model was still loading would be seen by a
/// query that matched nothing, and then never seen again, because the next tick
/// the flash is *unchanged*. That is a stuck flash, not a missed one: whatever
/// value it was left at is what the ship wears until it is hit again. It showed
/// up as a hit that rendered on one run of the same build and not the next.
fn flash_ships(
    ships: Query<(&ShipSkin, &HitFlash), NeedsFlash>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (skin, &HitFlash(flash)) in &ships {
        for part in &skin.0 {
            let Some(mut mat) = materials.get_mut(&part.material) else {
                continue;
            };
            let base = part.emissive;
            // Added, not assigned: an engine bell that glows on its own has to
            // still glow once the flash has decayed to nothing.
            mat.emissive = LinearRgba::rgb(
                base.red + SHIP_FLASH.red * flash,
                base.green + SHIP_FLASH.green * flash,
                base.blue + SHIP_FLASH.blue * flash,
            );
        }
    }
}

/// Repaints a ship whose livery has changed under it.
///
/// [`paint_and_upgrade`] paints once, when the glTF lands, and that was the
/// whole story while a colour could only be chosen before launch. It cannot be
/// the whole story now: `ESC` opens the livery page over a running match, and a
/// pilot who changes their hull there is looking at their own aircraft while
/// they do it. Same shape as [`flash_ships`], one component along —
/// `Changed<ShipPaint>` for the colour moving, `Changed<ShipSkin>` for the
/// materials arriving after it (the glTF finishes loading tens of frames after
/// the entity exists, and a paint applied in between would be applied to nothing
/// and then never again).
///
/// Idempotent with the initial paint on purpose: the two agree, so a ship that
/// is spawned and never recoloured is written once by each and looks identical
/// either way.
fn repaint_ships(
    ships: Query<(&ShipPaint, &ShipSkin), NeedsPaint>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (paint, skin) in &ships {
        for part in &skin.0 {
            let Some(mut mat) = materials.get_mut(&part.material) else {
                continue;
            };
            let want = if part.accent {
                paint.accent
            } else {
                paint.hull
            };
            // Alpha is the model's, not the palette's: a canopy authored
            // translucent must stay translucent.
            mat.base_color = want.with_alpha(mat.base_color.alpha());
        }
    }
}

fn sample_rocks(
    mut commands: Commands,
    frame: Res<SimFrame>,
    scene: Res<SceneAssets>,
    mut reg: ResMut<Registry>,
    mut q: Query<(&mut Interp, &mut MeshTag, Has<Snap>), With<Rock>>,
) {
    for view in &frame.0.asteroids {
        let pose = Pose::of_rock(view);

        let entity = *reg.rocks.entry(view.id).or_insert_with(|| {
            // The variant is simulation state precisely so every client picks
            // the same mesh. `RockView` does not carry it, so it is rederived
            // from the id the same way — stable, and identical everywhere.
            let variant = view.id as usize % scene.rock_meshes.len();
            commands
                .spawn((
                    Rock(view.id),
                    Mesh3d(scene.rock_meshes[variant].clone()),
                    MeshMaterial3d(scene.rock_material.clone()),
                    // Was `Transform::default()`, which put every rock at the
                    // origin for the one frame between spawning it and the next
                    // tick's write.
                    pose.transform(),
                    Interp::spawned(pose),
                    // The damage tint, in full. Everything else about this rock
                    // is shared with the other fifty-nine.
                    rock_tag(view),
                ))
                .id()
        });

        if let Ok((mut interp, mut tag, marked)) = q.get_mut(entity) {
            if marked {
                commands.entity(entity).remove::<Snap>();
                interp.snap(pose);
            } else {
                interp.advance(pose);
            }

            // `asteroids.js:101`, but as a per-instance number rather than a
            // material clone. `set_if_neq` is what keeps a quiet field from
            // re-extracting sixty mesh uniforms every tick.
            tag.set_if_neq(rock_tag(view));
        }
    }

    // Destroyed rocks leave the frame.
    reg.rocks.retain(|id, entity| {
        let live = frame.0.asteroids.iter().any(|a| a.id == *id);
        if !live {
            commands.entity(*entity).despawn();
        }
        live
    });
}

// ---------------------------------------------------------------------------
// Per-frame draw
// ---------------------------------------------------------------------------

/// Draws every sampled entity between its last two ticks.
///
/// The whole of the judder fix, and the only system that writes a `Transform`
/// the simulation did not produce.
///
/// `alpha` comes from [`Time<Fixed>::overstep_fraction`] — Bevy 0.19's name for
/// the fixed accumulator's leftover, as a 0..1 fraction of one timestep. Read
/// in `RunFixedMainLoopSystems::AfterFixedMainLoop` it is exactly "how long ago
/// the last tick ran", which is what makes it the blend factor. The clamp is
/// belt and braces: the fixed loop leaves it below 1 by construction, but a
/// paused or rate-scaled `Time<Virtual>` is not this system's problem to
/// diagnose.
fn draw_interpolated(fixed: Res<Time<Fixed>>, mut q: Query<(&Interp, &mut Transform)>) {
    let alpha = fixed.overstep_fraction().clamp(0.0, 1.0);
    for (interp, mut tf) in &mut q {
        *tf = interp.at(alpha).transform();
    }
}

// ---------------------------------------------------------------------------
// Displaced meshes: the rocks and the moon
// ---------------------------------------------------------------------------

/// One deformed unit icosphere, matching `asteroids.js:29` (`buildVariants`).
///
/// Two octaves: a big lobe that makes the rock lumpy and a fine bump that
/// roughens the surface. Flat normals afterwards, for the faceted look the JS
/// gets from `flatShading: true`.
///
/// **Every number in the displacement comes from
/// [`sim::rules::AsteroidMeshRules`]**, and none of them is written here. The
/// simulation collides against a sphere of
/// [`sim::rules::AsteroidFieldRules::collision_radius_scale`], which *is* the
/// mean of this displacement — so the mesh and the hitbox are two readings of
/// one description rather than two numbers that happen to look similar. They did
/// not look similar: the sphere used to be the JS's `size * 0.95` while this
/// surface averages `size * 0.7332`, which is a quarter of a rock of hitbox that
/// nothing was drawn in.
///
/// The displacement runs *before* [`Mesh::duplicate_vertices`], on the indexed
/// icosphere — the same order as `asteroids.js:33`'s `mergeVertices` then
/// displace. It is what keeps the rock watertight: a shared vertex is moved
/// once, so the faces around it stay joined however far it travels.
///
/// Six of these are built once and shared by every rock, so the whole field is
/// six mesh handles and one material — see the batching note in
/// [`crate`]'s docs.
///
/// **The mesh is still not the hitbox.** A single sphere cannot follow a surface
/// that ranges from 0.36 to 1.19 of the nominal radius with hard facets; it can
/// only be centred on it, and `max_radius_scale` says how far a spike reaches
/// past it. A rock is a sphere no matter how it is drawn.
fn rock_mesh(variant: u32) -> Mesh {
    let shape = RULES.world.asteroid_field.mesh;
    let mut mesh = Sphere::new(1.0)
        .mesh()
        .ico(shape.subdivisions)
        .expect("the rules' subdivision level is within the icosphere limit");

    // `asteroids.js:37` seeds the lobe with the variant index and the bump with
    // `v + 7`, so the two octaves of one rock are uncorrelated.
    let seed = f64::from(variant);

    if let Some(bevy::mesh::VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
    {
        for p in positions.iter_mut() {
            let [x, y, z] = *p;
            let (x64, y64, z64) = (f64::from(x), f64::from(y), f64::from(z));
            let (lf, bf) = (shape.lobe_freq, shape.bump_freq);
            // The noise is sampled in `f64` (see `pseudo_noise`) and applied in
            // `f32`, which is what the vertex buffer holds.
            let lobe = shape.lobe_base as f32
                + shape.lobe_amp as f32 * pseudo_noise(x64 * lf, y64 * lf, z64 * lf, seed);
            let bump = shape.bump_base as f32
                + shape.bump_amp as f32
                    * pseudo_noise(x64 * bf, y64 * bf, z64 * bf, seed + shape.bump_seed_offset);
            let n = lobe * bump;
            *p = [x * n, y * n, z * n];
        }
    }

    // The icosphere is indexed and shares vertices, so `compute_normals` would
    // smooth them. Splitting first is what gives the faceted rock.
    mesh.duplicate_vertices();
    mesh.compute_flat_normals();

    // Drop the CPU copy after upload. It is only needed by something that
    // raycasts against the mesh, and the simulation does its own collision
    // against `Asteroid::radius` — a rock is a sphere no matter how it is
    // drawn. Set last, because the edits above need `MAIN_WORLD` access.
    mesh.asset_usage = RenderAssetUsages::RENDER_WORLD;
    mesh
}

/// How finely the moon is tessellated, in sectors and stacks.
///
/// **Chosen for the vertex spacing, not the smoothness.** The displacement below
/// is per-vertex white noise, so spacing *is* the lump size, and getting it
/// wrong changes the surface completely: the same ±3% radius at a quarter of the
/// spacing is four times the slope, which stops reading as a lumpy moon and
/// starts reading as sandpaper.
///
/// `moon.js:12` builds `IcosahedronGeometry(1, 4)`. Three.js's `detail` is edge
/// *segments minus one*, so that is 5 segments per icosahedron edge — 500
/// triangles and 252 vertices, not the 2562 a recursive reading suggests. Spread
/// over a sphere that is `sqrt(4*PI / 252) = 0.22` of a radius apart, or 17.9
/// units on an 80-unit moon. A 32-sector UV sphere is 15.7 units at the equator
/// and 15.7 between stacks at 16 — the closest near-uniform match, and inside
/// the range Bevy's own docs call a sensible default.
///
/// This was `uv(96, 48)`, which was chosen when the sphere was undisplaced and
/// only the silhouette mattered.
const MOON_SECTORS: u32 = 32;
const MOON_STACKS: u32 = 16;

/// The moon, lumpy. `moon.js:11` (`createMoon`).
///
/// This was `Sphere::new(moon_r).mesh().uv(96, 48)` — a mathematically perfect
/// sphere, which is the whole of why it read as "too round": a circle for a
/// silhouette and a terminator with nothing on it to catch light.
///
/// `moon.js` displaces every vertex by `0.985 + 0.03 * pseudoNoise(...)` and
/// recomputes **smooth** normals — not flat, unlike the rocks. The moon is meant
/// to be subtly lumpy, not faceted, so the ±3% radius (±2.4 units here) shows as
/// a knobbly limb and mottled shading rather than as visible triangles.
///
/// A UV sphere rather than the JS's icosphere, and that is not a compromise —
/// see the texture note at the call site. It does mean the seam and the poles
/// carry duplicate vertices, which is what [`weld_smooth_normals`] is for.
fn moon_mesh(radius: f32) -> Mesh {
    let mut mesh = Sphere::new(radius).mesh().uv(MOON_SECTORS, MOON_STACKS);

    // The JS displaces a *unit* sphere and scales the mesh afterwards, so the
    // noise is sampled on unit coordinates. Bevy's builder emits the final
    // radius directly, hence the divide: feeding it 80-unit coordinates would
    // sample a different — equally random, equally arbitrary — field.
    let unit = f64::from(radius).recip();

    if let Some(bevy::mesh::VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
    {
        for p in positions.iter_mut() {
            let [x, y, z] = *p;
            let n = 3.7 * unit;
            let bump = 0.985
                + 0.03 * pseudo_noise(f64::from(x) * n, f64::from(y) * n, f64::from(z) * n, 11.0);
            *p = [x * bump, y * bump, z * bump];
        }
    }

    weld_smooth_normals(&mut mesh);

    // Same reasoning as `rock_mesh`: nothing raycasts this, the simulation
    // collides against a sphere at the origin. Set last — the edits above need
    // `MAIN_WORLD` access.
    mesh.asset_usage = RenderAssetUsages::RENDER_WORLD;
    mesh
}

/// Constant angular velocity about the entity's own axes, in rad/s.
///
/// Only the moon has one. `moon.js:31` spins it on `(0.005, 0.012, 0.003)`,
/// which is slow enough to read as a body rather than a prop — a full turn takes
/// nine minutes — and is what stops the lumps above being a fixed pattern baked
/// into one face of the sky. Asteroid spin is not this: it is simulation state,
/// arrives in [`SimFrame`], and is interpolated like any other pose.
#[derive(Component)]
struct Spin(Vec3);

/// `moon.js:32`'s `update`, verbatim: Euler increments about the local axes.
fn spin_bodies(time: Res<Time>, mut q: Query<(&Spin, &mut Transform)>) {
    let dt = time.delta_secs();
    for (spin, mut tf) in &mut q {
        tf.rotate_local_x(spin.0.x * dt);
        tf.rotate_local_y(spin.0.y * dt);
        tf.rotate_local_z(spin.0.z * dt);
    }
}

/// `pseudoNoise` from `asteroids.js:202` and `moon.js:7`, digit for digit.
///
/// **The discontinuity is the feature.** This is a sine *hash*, not a noise
/// field: the multipliers are large and coprime enough that two vertices a
/// tenth of a unit apart land in unrelated parts of the sine, so the output is
/// effectively white noise per vertex. That is what makes a rock spiky. An
/// earlier port here summed three sine products instead — smooth, continuous,
/// interpolating — and the identical `0.78 + 0.34 * n` displacement came out as
/// rounded lobes: a pebble rather than a shard. Same formula around it, entirely
/// different silhouette, and it is the whole of why the rocks looked soft.
///
/// The determinism ban on `sin` (see CLAUDE.md) does **not** reach here. It
/// applies to `crates/sim`, which has no dependency on this crate and no path to
/// this function; six meshes built once at startup on the machine that draws
/// them can differ in the last bit between platforms without anything noticing.
///
/// `f64` because the JS is `f64`: `sin(..) * 43758.5453` is around 4e4, where an
/// `f32` has about 256 representable steps left inside one unit interval — the
/// fractional part would come out quantized to that ladder rather than uniform.
fn pseudo_noise(x: f64, y: f64, z: f64, seed: f64) -> f32 {
    let s = (x * 12.9898 + y * 78.233 + z * 37.719 + seed * 4.7).sin() * 43758.5453;
    ((s - s.floor()) * 2.0 - 1.0) as f32
}

/// Smooth normals that treat vertices sharing a **position** as one vertex.
///
/// [`Mesh::compute_smooth_normals`] averages by *index*, and the UV sphere has
/// two sets of indices at the same place: a full seam column, duplicated so the
/// texture can wrap from u=1 back to u=0, and a fan of coincident vertices at
/// each pole. Averaging those separately gives the two sides of the seam
/// different normals, which under smooth shading draws a lit line from pole to
/// pole — invisible on a true sphere, where both halves average to the same
/// radial normal, and glaring once [`moon_mesh`] displaces the surface with a
/// per-vertex hash and the two halves no longer agree.
///
/// This is `mergeVertices` + `computeVertexNormals` (`moon.js:13`, `:20`) with
/// the merge left implicit: positions are bucketed, each face normal is added to
/// every vertex in its corners' buckets, and Bevy normalizes at the end. The
/// buckets are keyed on the exact bits of the position, which is sound here
/// because the duplicates are *copies* — the mesh builder pushes the same
/// computed `[f32; 3]` twice — rather than two arrivals at the same point by
/// different arithmetic.
fn weld_smooth_normals(mesh: &mut Mesh) {
    // `group_of[vertex]` indexes `groups`, and `groups[g]` lists every vertex at
    // that one position. Built before the mutable borrow below.
    let (groups, group_of) = {
        let Some(positions) = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|p| p.as_float3())
        else {
            return;
        };
        let mut groups: Vec<Vec<usize>> = Vec::new();
        let mut index: HashMap<[u32; 3], usize> = HashMap::new();
        let mut group_of: Vec<usize> = Vec::with_capacity(positions.len());
        for (i, p) in positions.iter().enumerate() {
            let key = [p[0].to_bits(), p[1].to_bits(), p[2].to_bits()];
            let g = *index.entry(key).or_insert_with(|| {
                groups.push(Vec::new());
                groups.len() - 1
            });
            groups[g].push(i);
            group_of.push(g);
        }
        (groups, group_of)
    };

    mesh.compute_custom_smooth_normals(|[a, b, c], pos, normals| {
        let n = Vec3::from(bevy::mesh::triangle_normal(pos[a], pos[b], pos[c]));
        for corner in [a, b, c] {
            for &i in &groups[group_of[corner]] {
                normals[i] += n;
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The interpolation math only. Everything below is pure — [`Pose`] and
/// [`Interp`] were split out from the systems precisely so that the part with
/// the failure modes (long-way rotation, interpolating in from the origin,
/// streaking across a teleport) can be tested without standing up an `App`, a
/// window, or a render device.
#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// Tolerance on positions and scales.
    const EPS: f32 = 1e-4;

    /// Tolerance on *angles*, in radians. Two orders looser than [`EPS`] and it
    /// has to be: `Quat::angle_between` is `2 * acos(|dot|)`, and `acos` near 1
    /// is where a float loses half its bits — an f32 quaternion one ulp off
    /// unit already reads as 7e-4 rad of rotation. This is still 0.1 of a
    /// degree, which is far tighter than anything the eye resolves at 144 Hz.
    const ROT_EPS: f32 = 2e-3;

    fn deg(d: f32) -> f32 {
        d * PI / 180.0
    }

    /// Two poses are the same pose. Rotation is compared as a *rotation*, not
    /// as four numbers: `q` and `-q` are the same orientation, and slerp
    /// returns whichever of the two lay on the short arc.
    #[track_caller]
    fn assert_same(got: Pose, want: Pose) {
        assert!(
            got.translation.abs_diff_eq(want.translation, EPS),
            "translation {:?} != {:?}",
            got.translation,
            want.translation
        );
        assert!(
            got.rotation.angle_between(want.rotation) < ROT_EPS,
            "rotation {:?} != {:?}",
            got.rotation,
            want.rotation
        );
        assert!(
            got.scale.abs_diff_eq(want.scale, EPS),
            "scale {:?} != {:?}",
            got.scale,
            want.scale
        );
    }

    fn pose(x: f32, yaw_deg: f32) -> Pose {
        Pose {
            translation: Vec3::new(x, 0.0, 0.0),
            rotation: Quat::from_rotation_y(deg(yaw_deg)),
            scale: Vec3::ONE,
        }
    }

    // -- The ends of the interval -------------------------------------------

    #[test]
    fn alpha_zero_is_the_previous_tick() {
        let interp = Interp {
            prev: pose(10.0, 0.0),
            curr: pose(20.0, 90.0),
        };
        assert_same(interp.at(0.0), interp.prev);
    }

    #[test]
    fn alpha_one_is_the_current_tick() {
        let interp = Interp {
            prev: pose(10.0, 0.0),
            curr: pose(20.0, 90.0),
        };
        assert_same(interp.at(1.0), interp.curr);
    }

    #[test]
    fn alpha_half_is_halfway() {
        let interp = Interp {
            prev: pose(10.0, 0.0),
            curr: pose(20.0, 90.0),
        };
        assert_same(interp.at(0.5), pose(15.0, 45.0));
    }

    // -- Rotation -----------------------------------------------------------

    /// The reason this is slerp and not lerp.
    ///
    /// A quaternion lerp crosses the chord rather than the arc, so
    /// renormalizing it sweeps the angle fastest at the ends and slowest
    /// through the middle. On a ship holding one steady roll that is a visible
    /// speed-up and slow-down inside every tick. Slerp's four quarter-steps are
    /// equal; the same test run against a lerp is asserted to fail, so this
    /// cannot pass by accident if someone swaps the call.
    #[test]
    fn rotation_sweeps_at_a_constant_rate() {
        let interp = Interp {
            prev: pose(0.0, 0.0),
            curr: pose(0.0, 150.0),
        };

        let step = |a: f32, b: f32| interp.at(a).rotation.angle_between(interp.at(b).rotation);
        let quarter = deg(150.0) / 4.0;
        for (a, b) in [(0.0, 0.25), (0.25, 0.5), (0.5, 0.75), (0.75, 1.0)] {
            let got = step(a, b);
            assert!(
                (got - quarter).abs() < 1e-3,
                "slerp {a}..{b} swept {got} rad, expected {quarter}"
            );
        }

        // And the naive alternative does not have that property.
        let lerped =
            |a: f32| (interp.prev.rotation * (1.0 - a) + interp.curr.rotation * a).normalize();
        let first = lerped(0.0).angle_between(lerped(0.25));
        let middle = lerped(0.25).angle_between(lerped(0.5));
        assert!(
            (first - middle).abs() > 1e-2,
            "a lerp is supposed to sweep unevenly; got {first} then {middle}"
        );
    }

    /// A 10-degree turn written as +350 must interpolate 5 degrees the short
    /// way, not 175 the long way. This is the case where the two candidate
    /// quaternions `q` and `-q` differ, and getting it wrong spins a ship
    /// almost all the way round inside one tick.
    #[test]
    fn rotation_takes_the_short_way_around() {
        let interp = Interp {
            prev: pose(0.0, 0.0),
            curr: pose(0.0, 350.0),
        };

        let mid = interp.at(0.5).rotation;

        // Rotating +Z by theta about Y gives (sin theta, 0, cos theta). The
        // short way is theta = -5 degrees, so x is *negative*; the long way is
        // +175, which would put x near +0.09 and z near -1.
        let z = mid * Vec3::Z;
        assert!(
            z.abs_diff_eq(Vec3::new(deg(-5.0).sin(), 0.0, deg(-5.0).cos()), EPS),
            "went the long way: +Z ended up at {z:?}"
        );
        assert!(mid.angle_between(Quat::IDENTITY) < deg(6.0));
    }

    /// Asteroid attitude arrives as Euler angles, which wrap. A rock spinning
    /// through pi must keep spinning: interpolating the angles themselves would
    /// average +3.13 and -3.13 to zero and snap it back to its rest pose every
    /// time it came round.
    #[test]
    fn a_rock_spinning_through_pi_does_not_reverse() {
        let rock = |y: f32| sim::world::RockView {
            id: 7,
            hp: 5,
            pos: [0.0; 3],
            rot: [0.0, y, 0.0],
            size: 1.0,
            hit_flash: 0.0,
        };

        let mut interp = Interp::spawned(Pose::of_rock(&rock(3.13)));
        interp.advance(Pose::of_rock(&rock(-3.13)));

        // Continuous, so it interpolated rather than snapping...
        assert_ne!(interp.prev.rotation, interp.curr.rotation);
        // ...and halfway between them is half a turn, not no turn.
        let z = interp.at(0.5).rotation * Vec3::Z;
        assert!(z.z < -0.999, "halfway through the wrap +Z was {z:?}");
    }

    // -- Spawns -------------------------------------------------------------

    /// A rock or ship that appeared this tick has no previous pose. It must
    /// draw where the simulation put it, not slide in from the origin.
    #[test]
    fn a_spawn_does_not_interpolate_in_from_the_origin() {
        let at = Pose {
            translation: Vec3::new(-320.0, 40.0, 180.0),
            rotation: Quat::from_rotation_x(deg(30.0)),
            scale: Vec3::splat(12.0),
        };
        let interp = Interp::spawned(at);

        for alpha in [0.0, 0.25, 0.5, 0.75, 1.0] {
            assert_same(interp.at(alpha), at);
        }
    }

    // -- Teleports ----------------------------------------------------------

    /// Respawn and the campaign warp. Interpolating across one draws the ship
    /// as a streak the length of the map.
    #[test]
    fn a_snap_does_not_interpolate() {
        let mut interp = Interp {
            prev: pose(10.0, 0.0),
            curr: pose(20.0, 90.0),
        };

        let respawn = Pose {
            translation: Vec3::new(0.0, 0.0, -540.0),
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        };
        interp.snap(respawn);

        for alpha in [0.0, 0.5, 1.0] {
            assert_same(interp.at(alpha), respawn);
        }
    }

    /// The backstop, for a discontinuity that arrives without a
    /// `SimEvent::ShipRespawned` to announce it.
    #[test]
    fn an_impossible_jump_snaps_instead_of_streaking() {
        let mut interp = Interp::spawned(pose(0.0, 0.0));
        let elsewhere = pose(600.0, 0.0);
        interp.advance(elsewhere);

        assert_same(interp.at(0.0), elsewhere);
        assert_same(interp.at(0.5), elsewhere);
    }

    /// ...and an attitude reset in place, which moves nothing.
    #[test]
    fn an_impossible_turn_snaps_too() {
        let mut interp = Interp::spawned(pose(0.0, 0.0));
        let flipped = pose(0.0, 180.0);
        interp.advance(flipped);

        assert_same(interp.at(0.0), flipped);
    }

    // -- The hull/accent split ----------------------------------------------

    /// The contract the customizer's preview is built on, and the one thing
    /// about the ship model this file is allowed to assume. If a model swap
    /// changes which meshes come out accent, that is a re-authoring decision;
    /// if it changes *where the line is*, the customizer and the game disagree
    /// about what the player picked.
    #[test]
    fn the_accent_split_is_luma_below_zero_point_three_five() {
        // Straddling the threshold on a pure grey, where luma is the value.
        assert!(is_accent(Color::LinearRgba(LinearRgba::rgb(
            0.34, 0.34, 0.34
        ))));
        assert!(!is_accent(Color::LinearRgba(LinearRgba::rgb(
            0.36, 0.36, 0.36
        ))));

        // Rec. 709 and not an average: green carries most of the weight, blue
        // almost none. A saturated blue is an accent at a value a green of the
        // same value is nowhere near.
        assert!(is_accent(Color::LinearRgba(LinearRgba::rgb(0.0, 0.0, 1.0))));
        assert!(!is_accent(Color::LinearRgba(LinearRgba::rgb(
            0.0, 0.5, 0.0
        ))));
    }

    /// The two authored defaults have to land on opposite sides, or the
    /// customizer's two colours are one colour.
    #[test]
    fn the_default_hull_and_accent_land_on_opposite_sides() {
        assert!(!is_accent(DEFAULT_HULL.into()));
        assert!(is_accent(DEFAULT_ACCENT.into()));
    }

    /// **The split is not a fixed point, and that is why it is applied
    /// exactly once.**
    ///
    /// Team red (`#ff5566`) is a *dark* colour by Rec. 709 luma — linear
    /// (1.0, 0.07, 0.11) comes to 0.27 — so a hull painted red would classify
    /// as an accent if it were ever fed back through [`is_accent`]. Nothing
    /// does today: [`paint_and_upgrade`] runs on `WorldInstanceReady`, reads
    /// the *authored* colour, and never re-derives the split from a material it
    /// has already written. This pins the reason, so that a later "just re-run
    /// the sweep on team change" reads as the bug it would be — every red ship
    /// would come out with its hull and its accents swapped.
    #[test]
    fn painting_is_not_a_fixed_point_of_the_split() {
        assert!(
            !is_accent(TEAM_HULL[0].into()),
            "team blue is a bright colour"
        );
        assert!(is_accent(TEAM_HULL[1].into()), "team red is a dark one");
    }

    // -- Ship paint ----------------------------------------------------------

    fn ship(id: sim::world::EntityId, team: i32) -> sim::world::ShipView {
        sim::world::ShipView {
            id,
            team,
            ..sim::world::ShipView::default()
        }
    }

    fn bot(id: sim::world::EntityId, team: i32) -> sim::world::ShipView {
        sim::world::ShipView {
            flags: sim::world::ShipFlags::ALIVE.with(sim::world::ShipFlags::BOT),
            ..ship(id, team)
        }
    }

    /// The paint on a ship, with nothing saved and nothing announced.
    fn painted(view: &sim::world::ShipView) -> ShipPaint {
        paint_for(view, &Livery::default(), &RemotePaint::default())
    }

    /// The local pilot wears their own colours whatever team they are on —
    /// `main.js:781` reads `getSavedShipColor()` for the player and only ever
    /// consults the team for *other* people's markers.
    #[test]
    fn the_local_pilot_keeps_their_own_colours() {
        let paint = painted(&ship(LOCAL_ID, 0));
        assert_eq!(paint.hull, Color::from(DEFAULT_HULL));
        assert_eq!(paint.accent, Color::from(DEFAULT_ACCENT));

        // And the chosen pair, once there is one, on either team.
        let chosen = Livery {
            hull: hex(0xff_5a3c),
            accent: hex(0x2a_3138),
        };
        for team in [-1, 0, 1] {
            let paint = paint_for(&ship(LOCAL_ID, team), &chosen, &RemotePaint::default());
            assert_eq!(paint.hull, Color::from(chosen.hull), "team {team}");
            assert_eq!(paint.accent, Color::from(chosen.accent), "team {team}");
        }
    }

    #[test]
    fn a_teamed_stranger_wears_the_team_colour() {
        assert_eq!(painted(&ship(7, 0)).hull, Color::from(TEAM_HULL[0]));
        assert_eq!(painted(&ship(8, 1)).hull, Color::from(TEAM_HULL[1]));
    }

    /// `team == -1` is "unassigned", which is every ship before the host
    /// presses Launch. `createShip({ tint: PALETTE[id % PALETTE.length] })`.
    #[test]
    fn an_unassigned_stranger_falls_back_to_the_palette() {
        for id in 2..20 {
            let want = PALETTE[id as usize % PALETTE.len()];
            assert_eq!(painted(&ship(id, -1)).hull, Color::from(want));
        }
    }

    /// A pilot who has announced their colours wears them, and the team colour
    /// stops applying — that announcement is the whole point of the `colors`
    /// message, and it used to be dropped on the floor.
    #[test]
    fn an_announced_livery_beats_the_team_colour() {
        let mut remote = RemotePaint::default();
        let theirs = ShipPaint {
            hull: hex(0xc0_84fc).into(),
            accent: hex(0x2a_3138).into(),
        };
        remote.0.insert(7, theirs);

        assert_eq!(
            paint_for(&ship(7, 0), &Livery::default(), &remote),
            theirs,
            "the announcement wins over team blue"
        );
        // ...and says nothing about anybody else.
        assert_eq!(
            paint_for(&ship(8, 0), &Livery::default(), &remote).hull,
            Color::from(TEAM_HULL[0])
        );
    }

    /// Nobody else's announcement can repaint the local pilot's own aircraft.
    /// The server never echoes `colors` back to its sender, so an entry under
    /// [`LOCAL_ID`] would have to be a mistake, and the pilot's own choice is
    /// the one thing on screen they are entitled to be sure of.
    #[test]
    fn nothing_announced_can_overrule_the_local_pilot() {
        let mut remote = RemotePaint::default();
        remote.0.insert(
            LOCAL_ID,
            ShipPaint {
                hull: hex(0x00_ff00).into(),
                accent: hex(0x00_ff00).into(),
            },
        );
        let paint = paint_for(&ship(LOCAL_ID, 0), &Livery::default(), &remote);
        assert_eq!(paint.hull, Color::from(DEFAULT_HULL));
    }

    // -- The `colors` message ------------------------------------------------

    /// One app with the two resources [`read_remote_paint`] needs, and a way to
    /// post frames at it. Not a full `App` plugin set: this system reads one
    /// message stream and writes one map, and nothing about that needs a
    /// renderer.
    fn with_socket(you: Option<i64>) -> App {
        let mut app = App::new();
        app.add_message::<FromServer>();
        app.init_resource::<RemotePaint>();
        app.insert_resource(NetSession {
            you,
            ..NetSession::default()
        });
        app.add_systems(Update, read_remote_paint);
        app
    }

    fn post(app: &mut App, msg: ServerMessage) {
        app.world_mut()
            .resource_mut::<bevy::ecs::message::Messages<FromServer>>()
            .write(FromServer(msg));
        app.update();
    }

    fn seen(app: &App, id: sim::world::EntityId) -> Option<ShipPaint> {
        app.world().resource::<RemotePaint>().0.get(&id).copied()
    }

    /// The `colors` frame the crossplay work decoded and dropped. It has to
    /// reach the registry, and it has to land on the ship its *wire* id names.
    #[test]
    fn a_colors_frame_repaints_the_pilot_it_names() {
        // This connection is player 5, so the swap trades 5 and `LOCAL_ID`.
        let mut app = with_socket(Some(5));
        post(
            &mut app,
            ServerMessage::Colors {
                id: 7,
                hull_color: 0xc0_84fc,
                accent_color: 0x2a_3138,
            },
        );
        assert_eq!(
            seen(&app, 7),
            Some(ShipPaint {
                hull: hex(0xc0_84fc).into(),
                accent: hex(0x2a_3138).into(),
            })
        );

        // The pilot the server happens to call 1 is `LOCAL_ID` inside this
        // client, and would be painted over the local pilot's own aircraft if
        // the swap were skipped. It is *this* client that becomes `LOCAL_ID`,
        // and player 1 takes its number.
        post(
            &mut app,
            ServerMessage::Colors {
                id: 1,
                hull_color: 0x46_ff9b,
                accent_color: 0x00_0000,
            },
        );
        assert_eq!(seen(&app, LOCAL_ID), None, "nothing may land on the pilot");
        assert_eq!(seen(&app, 5).map(|p| p.hull), Some(hex(0x46_ff9b).into()));

        // A pilot who leaves takes their paint with them: the server hands ids
        // out again, and the next holder of 7 is somebody else.
        post(&mut app, ServerMessage::Disconnect { id: 7 });
        assert_eq!(seen(&app, 7), None);

        // ...and a new room is a new set of ids entirely.
        post(
            &mut app,
            ServerMessage::Room {
                code: "ABCD".to_owned(),
                host: false,
                you: 5,
                private: false,
            },
        );
        assert_eq!(seen(&app, 5), None, "the room reply clears the registry");
    }

    /// The other half: this pilot's own colours have to go *out*, or they are
    /// the only pilot in the room nobody can see properly.
    #[test]
    fn the_room_is_told_when_there_is_a_room_to_tell() {
        let mut app = App::new();
        app.add_message::<ToServer>();
        app.insert_resource(Livery::default());
        app.insert_resource(NetSession::default());
        app.add_systems(Update, announce_livery);

        let sent = |app: &mut App| -> Vec<ClientMessage> {
            app.world_mut()
                .resource_mut::<bevy::ecs::message::Messages<ToServer>>()
                .drain()
                .map(|ToServer(msg)| msg)
                .collect()
        };

        // Offline: `flush_outbox` would drop it anyway, and there is nobody to
        // tell.
        app.update();
        assert!(sent(&mut app).is_empty(), "nothing goes out with no socket");

        // Reaching a room is the first moment a send can land, so it is the
        // first moment one is made. `main.js:778` announces at exactly this
        // point.
        app.world_mut().resource_mut::<NetSession>().phase = Phase::Room;
        app.update();
        assert_eq!(
            sent(&mut app),
            vec![ClientMessage::Colors {
                hull_color: pack_rgb(DEFAULT_HULL),
                accent_color: pack_rgb(DEFAULT_ACCENT),
            }]
        );

        // Sitting there is not news.
        app.update();
        assert!(sent(&mut app).is_empty());

        // Changing the paint is.
        app.world_mut().resource_mut::<Livery>().hull = hex(0xff_5a3c);
        app.update();
        assert_eq!(
            sent(&mut app),
            vec![ClientMessage::Colors {
                hull_color: 0xff_5a3c,
                accent_color: pack_rgb(DEFAULT_ACCENT),
            }]
        );

        // And so is the match starting, which is when a client that joined
        // after the announcement finally has a ship to paint.
        app.world_mut().resource_mut::<NetSession>().phase = Phase::Playing;
        app.update();
        assert_eq!(sent(&mut app).len(), 1);
    }

    // -- Bot liveries --------------------------------------------------------

    /// The property the whole hashing scheme exists for: two clients watching
    /// the same fight must agree about what colour a bot is. Nothing here reads
    /// a clock, a seed, or a random number — the same id gives the same paint,
    /// every run, on every platform.
    #[test]
    fn a_bots_livery_is_a_function_of_its_id_alone() {
        for id in [-9, -1, 3, 42, 1_000] {
            let once = painted(&bot(id, 1));
            for _ in 0..8 {
                assert_eq!(painted(&bot(id, 1)), once, "bot {id}");
            }
            // The team it happens to be on does not enter into it either.
            assert_eq!(painted(&bot(id, 0)), once, "bot {id} on the other team");
        }
    }

    /// Negative ids are bots on the wire (`server/index.js` allocates them as
    /// `-(nextId++)`), so the index must be a table position and never a
    /// negative one — which is what `%` would give and `rem_euclid` does not.
    #[test]
    fn a_wire_bots_negative_id_still_lands_in_the_table() {
        for id in -20..0 {
            let hull = Srgba::from(painted(&bot(id, 1)).hull);
            assert!(
                BOT_LIVERY.contains(&hull),
                "bot {id} was painted something that is not in the box"
            );
        }
        // ...and it is not the same paint as the pilot of the same number.
        assert_ne!(painted(&bot(-7, 1)), painted(&bot(7, 1)));
    }

    /// **Nine bots, nine paints.** A skirmish is four allies and five enemies on
    /// consecutive ids, and a stride coprime with the table length is what makes
    /// that a guarantee rather than a probability — see [`LIVERY_STRIDE`] for
    /// what hashing gave instead.
    #[test]
    fn nine_bots_come_out_nine_colours() {
        // The ids `sim_bridge::skirmish` hands out: allies 2..=5, enemies 6..=10.
        let hulls: Vec<u32> = (2..=10)
            .map(|id| pack_rgb(Srgba::from(painted(&bot(id, 1)).hull)))
            .collect();
        let mut distinct = hulls.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            hulls.len(),
            "nine bots came out {} colours: {hulls:06x?}",
            distinct.len()
        );
    }

    /// The point of the muted set. A saturated primary reads as a toy on a grey
    /// airframe, and — more practically — cyan and red mean *side*, so a bot in
    /// one would be a bot pretending to be a team marker. Chroma is what
    /// separates them: both team hulls are over 0.55 and no paint reaches 0.30.
    #[test]
    fn every_bot_livery_is_a_paint_rather_than_a_colour() {
        fn chroma(c: Srgba) -> f32 {
            c.red.max(c.green).max(c.blue) - c.red.min(c.green).min(c.blue)
        }

        for (i, team) in TEAM_HULL.iter().enumerate() {
            assert!(
                chroma(*team) > 0.55,
                "TEAM_HULL[{i}] is the premise of this test and has stopped being saturated"
            );
        }
        for (i, paint) in BOT_LIVERY.iter().enumerate() {
            assert!(
                chroma(*paint) < 0.30,
                "BOT_LIVERY[{i}] has a chroma of {:.2}, which is a colour, not a paint",
                chroma(*paint)
            );
            let value = paint.red.max(paint.green).max(paint.blue);
            assert!(
                (0.15..0.75).contains(&value),
                "BOT_LIVERY[{i}] is {value:.2} at its brightest: too {} to read as paint",
                if value < 0.15 { "dark" } else { "bright" }
            );
        }
    }

    /// Every accent has to stay on the dark side of the split, or a bot's
    /// canopy and intakes come out the same shade as its wings.
    #[test]
    fn a_bot_accent_stays_below_the_split() {
        for (i, paint) in BOT_LIVERY.iter().enumerate() {
            assert!(
                is_accent(shade(*paint).into()),
                "the accent for BOT_LIVERY[{i}] is not dark enough to be one"
            );
        }
    }

    // -- The settings store ---------------------------------------------------

    /// A colour survives a round trip through the store's own spelling, which
    /// is `customization.js`'s: `#rrggbb`, lower case, no alpha. A pilot who
    /// picks a hull in this client and then opens the Three.js lobby has to see
    /// the same one.
    #[test]
    fn a_colour_round_trips_through_the_hex_the_js_writes() {
        for c in [DEFAULT_HULL, DEFAULT_ACCENT, hex(0xff_5a3c), hex(0x00_0000)] {
            let text = hex_string(c);
            assert_eq!(text.len(), 7, "{text} is not #rrggbb");
            assert_eq!(text, text.to_lowercase());
            let back = parse_hex(&text).expect("the store's own spelling must parse");
            assert_eq!(pack_rgb(back), pack_rgb(c), "{text}");
        }
    }

    /// The wire carries `0xRRGGBB` as an integer, and the far end reads it back
    /// with [`hex`]. These two are the same conversion in both directions and
    /// have to stay each other's inverse.
    #[test]
    fn the_wire_packing_is_the_inverse_of_the_unpacking() {
        for packed in [0x00_0000, 0xff_ffff, 0x9f_b6cc, 0xff_5a3c, 0x46_ff9b] {
            assert_eq!(pack_rgb(hex(packed)), packed);
        }
    }

    /// Reading a key back out of a file just written, and — the part that
    /// matters — leaving every other key in it alone. The livery is two keys and
    /// `api.rs` keeps its own file; a store that rewrote the document wholesale
    /// would lose a setting every time another one was saved.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn the_settings_store_keeps_the_keys_it_was_not_asked_about() {
        let dir = std::env::temp_dir().join(format!(
            "spaceships-settings-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = dir.join("settings.json");
        let _ = std::fs::remove_dir_all(&dir);

        // Nothing saved yet: not an error, just nothing.
        assert_eq!(settings::get_from(&path, HULL_KEY), None);

        settings::set_in(&path, HULL_KEY, "#ff5a3c").expect("the directory is created for us");
        settings::set_in(&path, ACCENT_KEY, "#2a3138").expect("second write");
        settings::set_in(&path, "spaceships:trailShape", "star").expect("third write");
        // ...and the first key again, with a different value.
        settings::set_in(&path, HULL_KEY, "#46ff9b").expect("overwrite");

        assert_eq!(
            settings::get_from(&path, HULL_KEY).as_deref(),
            Some("#46ff9b")
        );
        assert_eq!(
            settings::get_from(&path, ACCENT_KEY).as_deref(),
            Some("#2a3138")
        );
        assert_eq!(
            settings::get_from(&path, "spaceships:trailShape").as_deref(),
            Some("star")
        );

        // A file that is not JSON reads as "nothing saved" rather than as a
        // reason to fail to start.
        std::fs::write(&path, "{ this is not json").expect("write");
        assert_eq!(settings::get_from(&path, HULL_KEY), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- The asteroid damage tag ---------------------------------------------

    /// The two fields must not bleed into each other: this is one `u32` doing
    /// the work of two floats, and getting the shift wrong makes a rock at full
    /// health flash permanently.
    #[test]
    fn the_damage_tag_keeps_its_two_halves_apart() {
        assert_eq!(pack_damage(0.0, 0.0), 0);
        assert_eq!(pack_damage(1.0, 0.0), 0xffff_0000);
        assert_eq!(pack_damage(0.0, 1.0), 0x0000_ffff);
        assert_eq!(pack_damage(1.0, 1.0), u32::MAX);
    }

    /// Out-of-range input has to clamp, not wrap. An `as u32` cast of a
    /// negative float is 0 and of a huge one saturates, but neither is
    /// something to rely on when the alternative is one `clamp`.
    #[test]
    fn the_damage_tag_clamps() {
        assert_eq!(pack_damage(-1.0, 2.0), 0x0000_ffff);
        assert_eq!(pack_damage(2.0, -1.0), 0xffff_0000);
    }

    /// Round-trips through the shader's arithmetic to within a quantization
    /// step, which is what 16 bits buys and is three orders of magnitude finer
    /// than the eye resolves in a quarter-second flash.
    #[test]
    fn the_damage_tag_survives_the_round_trip() {
        for &flash in &[0.0, 0.25, 0.5, 0.75, 1.0] {
            for &hp in &[0.0, 0.1, 0.5, 1.0] {
                let tag = pack_damage(flash, hp);
                let got_flash = (tag >> 16) as f32 / 65535.0;
                let got_hp = (tag & 0xffff) as f32 / 65535.0;
                assert!(
                    (got_flash - flash).abs() < 1e-4,
                    "flash {flash} -> {got_flash}"
                );
                assert!((got_hp - hp).abs() < 1e-4, "hp {hp} -> {got_hp}");
            }
        }
    }

    /// `RockView` carries `hp` but not the tier that set it, so the fraction
    /// the shader wants is recovered from `size`. Every tier's own size range
    /// has to come back with that tier's HP, or a chipped huge rock reads as a
    /// nearly-dead small one.
    #[test]
    fn a_rocks_maximum_hp_comes_back_from_its_size() {
        for tier in sim::rules::ASTEROID_TIERS {
            for size in [
                tier.min_size,
                (tier.min_size + tier.max_size) / 2.0,
                tier.max_size,
            ] {
                assert_eq!(
                    rock_max_hp(size as f32),
                    tier.hp,
                    "size {size} should be the {} HP tier",
                    tier.hp
                );
            }
        }
    }

    /// Nothing generates one, but a rock larger than the table must not divide
    /// by zero or fall off the end.
    #[test]
    fn an_oversized_rock_takes_the_last_tier() {
        let last = sim::rules::ASTEROID_TIERS[sim::rules::ASTEROID_TIERS.len() - 1];
        assert_eq!(rock_max_hp(1_000.0), last.hp);
    }

    /// A full-health rock is undimmed and unlit; a destroyed one is fully
    /// scorched. The shader's `mix(scorch, 1, hp01)` depends on the *ends*
    /// being exactly these two.
    #[test]
    fn a_rocks_tag_tracks_its_damage() {
        let rock = |hp: i32, hit_flash: f32| sim::world::RockView {
            id: 3,
            hp,
            size: 20.0, // the 30 HP tier
            hit_flash,
            ..sim::world::RockView::default()
        };

        assert_eq!(rock_tag(&rock(30, 0.0)), MeshTag(pack_damage(0.0, 1.0)));
        assert_eq!(rock_tag(&rock(0, 1.0)), MeshTag(pack_damage(1.0, 0.0)));
        assert_eq!(rock_tag(&rock(15, 0.5)), MeshTag(pack_damage(0.5, 0.5)));
    }

    /// The whole reason the tag is quantized: an untouched field writes
    /// nothing, so `set_if_neq` never marks sixty mesh uniforms dirty.
    #[test]
    fn an_untouched_rock_produces_an_unchanged_tag() {
        let rock = sim::world::RockView {
            id: 1,
            hp: 5,
            size: 6.0,
            hit_flash: 0.0,
            ..sim::world::RockView::default()
        };
        assert_eq!(rock_tag(&rock), rock_tag(&rock));
        assert_eq!(rock_tag(&rock), MeshTag(pack_damage(0.0, 1.0)));
    }

    // -- Flash brightness ----------------------------------------------------

    /// Both halves of the brightness choice, and the second half was found on
    /// screen rather than reasoned to.
    ///
    /// A flash has to clear `camera.rs`'s prefilter or it is a colour change
    /// nobody reads as a hit — and it has to leave *one channel under it*, or
    /// ACES desaturates the lot and a hit reads as a white blowout with no hue
    /// at all. The first `SHIP_FLASH`, `(6.0, 1.2, 0.9)`, failed the second
    /// half: it looked like the ship had been overexposed, not shot.
    #[test]
    fn a_flash_blooms_without_going_white() {
        const KNEE: f32 = 0.9;
        for (what, flash) in [("ship", SHIP_FLASH), ("rock", ROCK_FLASH)] {
            let peak = flash.red.max(flash.green).max(flash.blue);
            let floor = flash.red.min(flash.green).min(flash.blue);
            assert!(
                peak > KNEE,
                "the {what} flash peaks at {peak}, under the knee"
            );
            assert!(
                floor < KNEE,
                "every channel of the {what} flash is over the knee; ACES will \
                 desaturate it to white"
            );
        }
    }

    /// `asteroids.js:103` is `setRGB(f, f * 0.6, f * 0.3)`. The magnitude is
    /// this pipeline's business — the JS's peaks at 1.0 and this one has an HDR
    /// target to fill — but the *hue* is authored, and scaling the whole ramp
    /// is how it stays the same orange.
    #[test]
    fn the_rock_flash_keeps_the_js_ramp() {
        assert!((ROCK_FLASH.green / ROCK_FLASH.red - 0.6).abs() < 1e-5);
        assert!((ROCK_FLASH.blue / ROCK_FLASH.red - 0.3).abs() < 1e-5);
    }

    /// The other half of the threshold, and the one that would silently ruin
    /// the fix: real motion at the fastest the flight model allows must still
    /// interpolate. If someone tightens [`TELEPORT_DIST_SQ`], a boosting ship
    /// snaps every tick and the judder is back.
    #[test]
    fn top_speed_is_still_motion() {
        let step = (TOP_SPEED * sim::world::TICK_DT) as f32;
        let mut interp = Interp::spawned(pose(0.0, 0.0));
        interp.advance(pose(step, 0.0));

        assert_same(interp.at(0.0), pose(0.0, 0.0));
        assert_same(interp.at(1.0), pose(step, 0.0));
    }

    /// And so must the fastest authored rotation.
    #[test]
    fn the_fastest_roll_is_still_motion() {
        let per_tick = (RULES.ship.roll_rate * sim::world::TICK_DT) as f32;
        let mut interp = Interp::spawned(pose(0.0, 0.0));
        interp.advance(pose(0.0, per_tick * 180.0 / PI));

        assert!(
            interp.prev.rotation.angle_between(interp.curr.rotation) > 0.0,
            "a roll at the authored rate was mistaken for a teleport"
        );
    }
}
