//! Projectiles, beams, and effects — **one mesh, one material, one draw call**.
//!
//! [`crate`]'s module docs name this file's constraint directly: the JS client
//! spends 477 draw calls at p99 because it allocates a mesh per entity, and
//! `ProjView`/`FlareView` are small fixed-stride records with hundreds in
//! flight. A mesh per bolt reproduces that bottleneck exactly.
//!
//! # The shape that does not
//!
//! There is exactly **one** rendered *effect* entity in this module —
//! [`EffectSurface`] — carrying one [`Mesh`] and one [`StandardMaterial`]. Every
//! frame, [`build_surface`] clears that mesh and rewrites it from the current
//! [`SimFrame`] slices plus this module's own effect state. Bullets, bullet
//! halos, missile exhaust and nozzle glow, flare cores and glows, beams,
//! explosion shells, muzzle flashes, and engine trails all land in the same
//! vertex buffer.
//!
//! The one thing that is *not* an effect is the missile body, which is a solid
//! object and is drawn as one — see [the section below](#the-missile-body).
//!
//! The cost is therefore **O(1) in draw calls and O(n) in vertices**, which is
//! the trade the GPU wants. Six hundred live effects is one draw call and about
//! 5 000 triangles; six hundred entities would be six hundred draw calls and
//! the same 5 000 triangles.
//!
//! ## Why a rebuilt mesh and not a storage buffer
//!
//! `main.rs` offers two shapes: "a material extension plus a storage buffer
//! indexed by instance, or a single mesh rebuilt each frame from the `Frame`
//! slices". This is the second, and the deciding vote is the browser.
//! `Cargo.toml` names `webgl2` as the wasm render backend, and **WebGL2 has no
//! storage buffers at all** — `@group(1) @binding(n) var<storage>` does not
//! compile there. A storage-buffer instancer would be a native-only renderer
//! with a second, different implementation for the web, which is the exact
//! duplication the Rust port exists to delete. A rebuilt vertex buffer is one
//! implementation that runs unchanged on Metal and WebGL2.
//!
//! The second vote is that these are *effects*: no two bolts share a transform,
//! a colour, or an age, so the per-instance payload is nearly as large as the
//! vertices it would drive. Instancing pays when the per-instance data is small
//! relative to the mesh. Four vertices per quad is not that case.
//!
//! ## The brush atlas
//!
//! One material means one texture, so [`build_glow_atlas`] bakes two brushes
//! into a single 128×64 image: a soft radial falloff for halos and puffs, and a
//! near-solid disc for bolt cores and beams. A quad picks one by its UVs
//! ([`Brush::CORE`], [`Brush::GLOW`]). Both brushes reach zero alpha at their
//! cell edge, so linear filtering cannot bleed one into the other.
//!
//! Colour rides in [`Mesh::ATTRIBUTE_COLOR`], which the PBR shader multiplies
//! into `base_color` *before* the texture
//! (`bevy_pbr/src/render/pbr_fragment.wgsl:55`, then `:101`, then `:194`), so
//! with `unlit` the fragment is exactly `vertex_colour × texture`. Vertex
//! colours are `Float32x4` and nothing clamps them, which is what lets a bolt
//! core sit at 5.0 and clear the camera's bloom prefilter threshold of 0.9 —
//! the Ultra idiom from `graphics.js`, where additive materials are multiplied
//! by `glowBoost = 1.7` for the same reason.
//!
//! `AlphaMode::Add` maps to a premultiplied-alpha blend state whose shader half
//! zeroes the destination alpha, so the pass is order-independent. That matters
//! here: a single mesh cannot sort its own triangles, and additive blending is
//! the family of effects that does not need it to.
//!
//! ## The particle model, and why it is not `trails.js`'s
//!
//! A `trails.js` particle is a position, a scale and an opacity: `emit` stamps
//! it and `update` only ever shrinks and dims it. Ported straight across, every
//! effect in this module inherited three problems that a screenshot makes
//! obvious and a code review does not:
//!
//! - **Nothing moves.** A mote is abandoned in world space the instant it is
//!   born, so at cruise the engine plume is deposited on the chase camera's own
//!   axis — hidden by the hull at the near end, already past the lens at the far
//!   end — and is invisible in between. On afterburner it is a row of separated
//!   beads, because the ship covers several units between one particle and the
//!   next and each particle is a fifth of a unit across.
//! - **Nothing cools.** One colour for a whole life. Real exhaust, fire and
//!   fireballs shift hue as they lose energy, and holding one shade while
//!   dimming is the clearest tell that an effect is a texture on a quad.
//! - **Nothing has structure.** `bullets.spawnExplosion` is a single expanding
//!   sphere, so a ship dying is a symmetrical blob that grows and fades.
//!
//! [`Mote`] therefore carries a velocity, a drag, a second colour, and a
//! motion smear ([`MOTE_SMEAR`]); [`Shell`] carries a second colour and an
//! easing; and [`spawn_explosion`] throws sparks out of the same mote pool the
//! trails use. None of it changes the shape of the module: it is still one
//! entity, one material, one mesh, and one draw call, and the extra fields are
//! CPU-side state that never reaches the GPU.
//!
//! ## Battle damage
//!
//! [`emit_damage`] is the one effect here with no JS to port: smoke that
//! thickens as a hull drops and fire at the wing root when it is nearly gone,
//! on **every** ship the frame carries rather than only the local one. Watching
//! a bandit start to stream is the whole point, and nothing in the old client
//! said anything about anybody else's hull.
//!
//! It costs one more `Vec<Mote>` and one more quad per particle, on its own
//! fixed cap so it cannot evict the engine trails. See [`SMOKE_AT`] for why the
//! smoke is a light haze rather than dark smoke — the answer is that this
//! module has exactly one material and it is additive.
//!
//! # The missile body
//!
//! A missile is the one projectile here that is a *thing* rather than a light,
//! and the additive surface above cannot draw a thing. It got a
//! [`MeshBuild::streak`] like everything else — a camera-facing quad on the hard
//! brush — and at range that reads as a glowing lozenge and close up as a flat
//! white pill. `missiles.js:143` (`makeMissileMesh`) builds five solid parts in
//! three flat colours, and losing them is what the report "rockets don't look
//! like rockets" is about.
//!
//! So it is solid geometry, and it is the one place in this module that spawns
//! entities. The shape that keeps that inside the budget:
//!
//! - **One mesh, built once.** [`missile_mesh`] is a static `Handle<Mesh>` built
//!   at [`setup`] and never touched again. Nothing here rebuilds geometry per
//!   frame, so the fixed-capacity padding [`build_surface`] needs does not apply
//!   — a mesh that is never rewritten cannot make the slab allocator free and
//!   reallocate under the render world.
//! - **One material, and three colours anyway.** The body, the fins and the bell
//!   are three hexes in the JS and three materials with them; here they are
//!   [`Mesh::ATTRIBUTE_COLOR`] on one `StandardMaterial`, which the PBR shader
//!   multiplies into `base_color` exactly as it does for the effect surface.
//!   Bevy's batch key is `(mesh, material, pipeline)`, so every missile in the
//!   air is one batch however many there are.
//! - **A fixed pool, hidden and moved.** [`MISSILE_BODY_POOL`] entities are
//!   spawned at startup and are never spawned or despawned again;
//!   [`place_missile_bodies`] writes a `Transform` and a `Visibility` on the
//!   ones in use and hides the rest. This is `hud.rs`'s target-marker pool, in
//!   3D. Despawning and respawning per shot would churn the render world's
//!   caches for no reason and is the habit this port exists to break.
//!
//! Cost, measured: a scene with forty missile bodies in it is **one** more draw
//! call than the same scene with none.
//!
//! # Whose shot is whose
//!
//! `bullets.js` keeps three material pairs — `self`/`ally`/`enemy` — and a
//! bolt's colour is the clearest read a player gets on whether it is about to
//! hurt them. `ProjView` used to carry no owner and no team, so everything here
//! drew the `self` palette and incoming fire looked exactly like outgoing.
//!
//! It now carries [`Allegiance`], resolved in the simulation off the same team
//! comparison that decides whether the round can damage you. See that type for
//! why the signal is a friend/foe verdict rather than a raw owner id: the
//! renderer would otherwise have to re-derive a rule the simulation already
//! owns, which is the client/server duplication the port exists to delete.
//!
//! It costs **no** extra draw calls. The colour is three floats in
//! [`Mesh::ATTRIBUTE_COLOR`], in the same vertex buffer, in the same single
//! mesh — [`bolt_palette`] resolves the three palettes once per frame and every
//! bolt indexes it. Bullets, beams, muzzle flashes, and the halo around a
//! missile body all take it; the missile *body* stays three flat greys on its
//! shared mesh, because that mesh is what keeps forty rockets down to one batch.
//!
//! # What is still missing from `Frame`
//!
//! - **`FlareView` has no owner.** A decoy is not a weapon and nothing is
//!   currently misled by a yellow flare, but the field is the same one-liner if
//!   a use turns up.
//! - **Beams are events, not state.** `SimEvent::Fired { weapon: Beam }` gives
//!   the segment once; `beams.js` keeps it on screen for `BEAM_LIFE`. That
//!   0.18 s lifetime therefore lives in [`Effects::beams`] on this side, and a
//!   second client watching the same tick stream would have to reimplement it.
//! - **`FlareView::life` is remaining burn, `age` is elapsed** — both present,
//!   which is what the flicker and fade need. Nothing missing.
//!
//! # Where the numbers come from
//!
//! Geometry, colours, lifetimes, and emission rates are ported from
//! `public/src/bullets.js`, `missiles.js`, `beams.js`, and `trails.js`, with
//! the `graphics.js` Ultra multiplier applied. Sizes that describe *simulation*
//! behaviour — speeds, ranges, lifetimes the sim also knows — are read from
//! [`sim::rules`] instead of copied, per the crate rule that a game constant
//! exists exactly once.
//!
//! # Measuring
//!
//! `SPACESHIPS_DRAWCALLS=1` installs [`report_draw_calls`] into the render
//! world, which counts the *batched* items in the three main 3D phases once a
//! second. See that function for what each number is.
//!
//! `SPACESHIPS_FX_DEMO=<n>` fills the effect lists with `n` synthetic
//! projectiles so the count can be taken against a busy scene — far more than a
//! real match puts in the air, which is the point of a stress harness.
//!
//! `SPACESHIPS_FX_SCENE=<effect>` and `SPACESHIPS_FX_TRAIL=<mode>` are the
//! opposite: they hold **one** effect still, in front of the camera, so a
//! before-and-after pair is a pair of the same thing. See [`fx_scene`] and
//! [`forced_trail`].
//!
//! Two values of the first are not effects and do not take an age:
//! `SPACESHIPS_FX_SCENE=rocket@<units>` parks a flight of missile *bodies* at a
//! range, because a body does not age and the question about it is how it reads
//! at 10 units against how it reads at 60 ([`stage_rockets`]); and
//! `SPACESHIPS_FX_SCENE=allegiance@<units>` parks one volley of each side
//! abreast, because "can you tell them apart" is a question about three things
//! next to each other ([`stage_allegiance`]).

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::image::{ImageSampler, ImageSamplerDescriptor};
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use sim::world::{Allegiance, ExplosionKind, FlareView, ProjView, ShipFlags, SimEvent, WeaponKind};
use spaceships_sim as sim;

use crate::sim_bridge::{pos as to_vec3, rot, SimFrame, SimSet};

/// The rules, so speeds and lifetimes are *derived* rather than guessed at.
/// Same reasoning as `scene.rs`: `rules.rs` is where a game constant is allowed
/// to live and re-deriving it here is how it cannot drift.
const RULES: sim::rules::Rules = sim::rules::Rules::DEFAULT;

// ---------------------------------------------------------------------------
// Tuning, ported from public/src/
// ---------------------------------------------------------------------------

/// `graphics.js` multiplies every additive material's colour by this when Ultra
/// is on (`glowBoost`, `graphics.js:377`). It is the whole reason the effect
/// palette clears the bloom threshold.
const ULTRA_GLOW: f32 = 1.7;

/// `bullets.js`: `LASER_LEN`.
const BOLT_LEN: f32 = 5.0;
/// `bullets.js` core cylinder radius is 0.09 and the halo's is 0.32. A
/// soft-edged billboard reads thinner than a lit cylinder of the same radius,
/// so the core is widened to keep a bolt visible at range.
const BOLT_CORE_HALF_W: f32 = 0.14;
/// The halo, sized to be **read** rather than to match a radius.
///
/// `bullets.js`'s halo cylinder is 0.32, and at that width the bolt has no
/// legible colour: `camera.rs` grades with [`Tonemapping::AcesFitted`], which
/// desaturates highlights hard, so the 7.5-nit core blows to white and a
/// two-pixel rim around it goes with it. Since the halo is now the thing that
/// says *whose shot this is* ([`BoltInk`]), it is widened until the coloured
/// area survives the grade at combat range — which is the same reasoning that
/// widened the core, applied to the half of the bolt that carries information.
///
/// The JS could afford a thin one: it had a material per team and no ACES.
const BOLT_HALO_HALF_W: f32 = 0.85;
/// `bullets.js`: halo cylinder is `LASER_LEN * 1.15`, lengthened with the
/// widening above so the glow stays a lozenge rather than becoming a bead.
const BOLT_HALO_LEN: f32 = BOLT_LEN * 1.32;
/// Opacity of the halo pass. `bullets.js` uses 0.55 against an unbloomed,
/// ungraded frame; see [`BOLT_HALO_HALF_W`] for why this one has to work harder.
const BOLT_HALO_ALPHA: f32 = 0.85;

/// `beams.js`: cylinder radius 0.5.
const BEAM_HALF_W: f32 = 0.5;
/// `beams.js`: `LIFE`.
const BEAM_LIFE: f32 = 0.18;
/// `beams.js`: base opacity, faded linearly over `BEAM_LIFE`.
const BEAM_OPACITY: f32 = 0.9;

/// `missiles.js:17`: `BODY_LEN`. The fuselage runs from `-BODY_LEN / 2` to
/// `+BODY_LEN / 2` about the missile's own origin, and every other length below
/// is measured off that.
const MISSILE_BODY_LEN: f32 = 3.5;
/// `missiles.js:18`: `BODY_RAD`, the fuselage radius at the nose end.
const MISSILE_BODY_RAD: f32 = 0.28;
/// `missiles.js:32`: the fuselage is `CylinderGeometry(BODY_RAD, BODY_RAD +
/// 0.04, ..)`, so it is a hair fatter at the tail. Four hundredths of a unit is
/// almost nothing and it is exactly what stops the barrel reading as a pipe.
const MISSILE_BODY_RAD_AFT: f32 = MISSILE_BODY_RAD + 0.04;
/// `missiles.js:19`: `NOSE_LEN`, ahead of the fuselage.
const MISSILE_NOSE_LEN: f32 = 1.8;
/// `missiles.js:20`, `:21`, `:22`: the fin box, span across, thickness through,
/// depth along the body.
const MISSILE_FIN_SPAN: f32 = 2.0;
const MISSILE_FIN_THICK: f32 = 0.07;
const MISSILE_FIN_DEPTH: f32 = 1.1;
/// `missiles.js:23`: `FIN_Z`, which sets the fins a tenth of a unit forward of
/// the fuselage's aft face rather than flush with it.
const MISSILE_FIN_Z: f32 = -(MISSILE_BODY_LEN / 2.0 - MISSILE_FIN_DEPTH / 2.0 - 0.1);
/// `missiles.js:47`: the nozzle bell, `ConeGeometry(0.38, 0.55)` — mouth radius
/// and length.
const MISSILE_BELL_R: f32 = 0.38;
const MISSILE_BELL_LEN: f32 = 0.55;
/// The bell's throat, where it meets the fuselage's aft face.
///
/// The one dimension with no JS to copy, because the JS bell is a *cone* and has
/// no throat — see [`missile_geometry`] on why this is a frustum instead. Set
/// below [`MISSILE_BODY_RAD_AFT`] so the tail visibly necks down and then flares,
/// which is the silhouette that says "nozzle" rather than "the body stopped".
const MISSILE_BELL_THROAT_R: f32 = 0.24;
/// The bell's mouth plane: where the exhaust leaves and where the glow that
/// stands in for the flame sits.
///
/// This is `missiles.js:24`'s `NOZZLE_Z = -(BODY_LEN / 2 + 0.18)` — that is
/// -1.93 — pushed back to -2.30, and the reason is that the JS number is
/// *inside* the body. Its bell spans z -1.755 to -2.305 and it hangs both the
/// glow sphere and the exhaust emitter at -1.93, halfway up the inside of the
/// cone; three.js depth-tests them against it and eats the first frames of the
/// plume. A billboard quad centred there does worse, because it is flat and
/// half of it is behind the bell wall at any angle.
///
/// Putting it on the mouth plane is what makes the plume come *out of the
/// nozzle* rather than out of the middle of the tail, which is the whole reason
/// the body and the exhaust have to agree about where the bell is.
const MISSILE_FLAME_Z: f32 = -(MISSILE_BODY_LEN / 2.0 + MISSILE_BELL_LEN);
/// `missiles.js:26`, `:32`, `:47`: every round part is 10 radial segments.
const MISSILE_SEGMENTS: u32 = 10;
/// `missiles.js:66`, `:67`, `:68`: fuselage and nose, fins, bell. Three flat
/// `MeshBasicMaterial` colours there; three vertex colours on one lit material
/// here.
const MISSILE_HULL_HEX: u32 = 0xd4dce8;
const MISSILE_FIN_HEX: u32 = 0x7a8fa8;
const MISSILE_BELL_HEX: u32 = 0x445566;
/// How many missile bodies can be on screen at once.
///
/// [`sim::rules::WeaponRules::missile_max`] per ship against a lobby that tops
/// out around ten, plus the ones already in the air when a full salvo goes out.
/// Overflow is missiles drawn without a body rather than a panic or a
/// reallocation, and the pool is allocated once at startup either way.
const MISSILE_BODY_POOL: usize = RULES.weapons.missile_max as usize * 12;
/// `missiles.js:62`: `TRAIL_INTERVAL`, the exhaust emission period.
const MISSILE_EXHAUST_INTERVAL: f32 = 0.028;

/// `missiles.js`: flare core sphere radius.
const FLARE_CORE_R: f32 = 0.30;
/// `missiles.js`: flare glow sphere radius.
const FLARE_GLOW_R: f32 = 1.10;

/// The particle pool shared by engine trails, missile exhaust, and explosion
/// sparks.
///
/// `trails.js`'s `MAX_PARTICLES` is 250 and this was ported at 320. That is a
/// budget from a client that spent a *draw call* per particle, and it is the
/// reason a boosting ship's plume was a row of eight beads with gaps between
/// them: at the rates below, one ship on afterburner alone wants about 120
/// live particles before it reads as a continuous plume rather than a necklace.
/// Here a particle is four vertices in a buffer that is padded to
/// [`MESH_QUAD_CAPACITY`] every frame whatever happens, so the *upload* cost of
/// raising this is exactly zero and what it actually buys is spent on the CPU
/// loop that fills it. See [`MESH_QUAD_CAPACITY`] for the arithmetic that keeps
/// every pool inside the buffer.
const MAX_MOTES: usize = 900;

/// Cap on battle-damage particles, kept **separate** from [`MAX_MOTES`].
///
/// One shared pool would let a squadron of burning wrecks evict every engine
/// trail in the match, and vice versa — the two effects would silently fight
/// over the same slots. Two pools mean each is bounded on its own and neither
/// can starve the other; the mesh budget below covers both.
const MAX_DAMAGE_MOTES: usize = 320;

/// Cap on EMP wavefront particles, and a **third** pool for the same reason
/// there is a second.
///
/// This one was found by looking rather than reasoned out in advance. The front
/// went into [`MAX_MOTES`] first, and the wave was invisible: a ten-ship
/// skirmish emits engine trail at about 450 particles a second against a
/// 900-slot pool, so the pool is saturated within seconds of a match starting
/// and every new trail mote evicts the oldest thing in it — which, for the two
/// hundred milliseconds after a pulse, is the pulse. Two thirds of the
/// wavefront was deleted before it had travelled sixty units.
///
/// Sized at [`EMP_MOTES`] × 2 so two overlapping pulses both survive intact,
/// which is a real case: friendly blinding is on, so a furball can eat two.
const MAX_PULSE_MOTES: usize = EMP_MOTES * 2;

/// Cap on live explosion shells, and on beams.
///
/// Neither list had one. A shell is short-lived so the count self-limits in
/// practice, but "in practice" is not what the fixed vertex budget below is
/// asserting, and a missile volley into a cluster of asteroids is a case that
/// pushes many at once. Cheap insurance against silently truncating the mesh.
const MAX_SHELLS: usize = 128;
const MAX_BEAMS: usize = 32;

/// Fixed vertex and index budget for the effects mesh.
///
/// The mesh is rebuilt every frame, and it **must not change size** doing so --
/// see the padding in `rebuild`. Sized for the worst case the caps above allow,
/// with headroom: every quad is 4 vertices and 6 indices.
///
/// The worst case, counted: 900 motes + 320 damage + 400 pulse + 128 shells +
/// 32 beams, plus the projectiles a ten-ship match can have in flight — 400
/// bolts at two quads each (a 0.05 s gun cooldown against a 2 s bolt life), 30
/// flares at two, and 40 missiles at **one**, their bodies having moved off this
/// mesh onto their own. That is 2 680 quads against 4 096.
const MESH_QUAD_CAPACITY: usize = 4096;
const MESH_VERTEX_CAPACITY: usize = MESH_QUAD_CAPACITY * 4;
const MESH_INDEX_CAPACITY: usize = MESH_QUAD_CAPACITY * 6;

/// `main.js` `TRAIL_OFFSETS`: the two engine nozzles, in ship-local space.
///
/// These are the *old* model's, and they sit at x = ±2.2 on a hull about 8
/// units across — which is out at the wings. That was right for `spaceship.glb`
/// (six Blender primitives with engines on the outboard sections) and is wrong
/// for anything shaped like an aircraft, where the exhaust is a pair of nozzles
/// close to the centreline at the tail.
const TRAIL_OFFSETS_LEGACY: [Vec3; 2] = [Vec3::new(-2.2, -0.05, -1.8), Vec3::new(2.2, -0.05, -1.8)];

/// Nozzle positions for the F-22 airframe (`jet.glb`).
///
/// Twin nozzles inboard at the tail, in **ship** space — so they have to follow
/// `scene::model_fit`, which scales the jet up and shifts it nose-ward so it
/// frames like the model it replaces. The first values here were derived
/// against the unfitted mesh and left the plumes too wide, too high and hanging
/// well behind the aircraft.
const TRAIL_OFFSETS_JET: [Vec3; 2] = [
    Vec3::new(-0.45, -0.06, -1.45),
    Vec3::new(0.45, -0.06, -1.45),
];

/// Exhaust origins for whichever hull is flying.
///
/// Keyed off [`jet_hull`], which asks `scene::ship_model()` — the same function
/// that decides which glb is actually loaded — so the trails follow the model
/// rather than making a second, independent decision about it.
///
/// Multiplied by [`crate::scene::SHIP_SCALE`] because these are consumed in
/// **world** space — `emit` computes `ship.pos + quat * offset` rather than
/// parenting the motes to the hull — so unlike the mesh they do not inherit the
/// ship's scale and have to be given it. Both constants are authored in the
/// same pre-scale space the JS authors `TRAIL_OFFSETS` in, where they are
/// children of a group `main.js:219` scales.
/// How much of a ship's velocity its exhaust keeps, split along the nozzle
/// axis.
///
/// [`EmitMode::inherit`] exists so the plume *falls behind* the ship rather
/// than travelling with it: a mote keeping 70% of the ship's velocity drifts
/// aft at the remaining 30%, which is what puts a visible stream at the tail
/// instead of a cloud around the hull.
///
/// Applied to the whole velocity vector, though, that residual points along the
/// ship's **travel**, not along its nozzles. Fly straight and the two coincide,
/// so it looks right; drift, strafe, or slide through a hard turn and 30% of a
/// sideways velocity walks the plume off the side of the aircraft. That is the
/// trails "not travelling the right way if you go sideways", and they were not.
///
/// Only the component along the exhaust axis may be shed. Everything lateral is
/// inherited whole, so the plume stays on the nozzles however the airframe is
/// moving, and the aft drift is exactly as tuned in the case it was tuned for —
/// `back` parallel to `vel`, where this reduces to the old expression.
///
/// `back` must be a unit vector: the ship's -Z, which is where the nozzles
/// point.
fn carried_momentum(vel: Vec3, back: Vec3, inherit: f32) -> Vec3 {
    let along = vel.dot(back);
    let lateral = vel - back * along;
    lateral + back * (along * inherit)
}

fn trail_offsets() -> [Vec3; 2] {
    if jet_hull() {
        return TRAIL_OFFSETS_JET.map(|o| o * crate::scene::SHIP_SCALE);
    }
    TRAIL_OFFSETS_LEGACY.map(|o| o * crate::scene::SHIP_SCALE)
}

/// One row of `main.js`'s `EMIT_CONFIG`, plus the four fields the JS has no
/// concept of: a plume's motion, its spread, its growth, and its colour.
///
/// # What the JS is missing, and why it beads
///
/// `trails.js` particles are **static in world space**. `emit` copies a
/// position into a mesh and `update` only ever touches scale and opacity, so a
/// mote is stamped at the nozzle and abandoned there while the aircraft flies
/// out from in front of it. Two things follow, and both are visible in a
/// screenshot:
///
/// - At cruise the plume is *behind* the ship on the chase camera's own axis.
///   The hull hides the near end, the far end has already swept past the lens,
///   and the trail is invisible in between. It was.
/// - At the emission rates above, the ship covers 2 to 4 units between one
///   particle and the next, and each particle is a fifth of a unit across. That
///   is a string of separated beads, not a plume — which is precisely what the
///   afterburner looked like.
///
/// [`EmitMode::inherit`] is the fix for both: exhaust leaves the nozzle carrying
/// most of the ship's momentum and only *slowly* falls behind, so it stays at
/// the tail where it is visible and where consecutive particles overlap.
struct EmitMode {
    /// Particles per second, per nozzle.
    rate: f32,
    /// Birth scale range; the JS sphere has radius 0.5, so world half-extent is
    /// `0.5 * scale`.
    scale: (f32, f32),
    /// Lifetime range, seconds.
    life: (f32, f32),
    /// Position jitter, ± per axis.
    jitter: f32,
    /// Fraction of the ship's velocity a new particle keeps. Below 1, so the
    /// plume recedes; well above 0, so it recedes *slowly* and stays in shot.
    inherit: f32,
    /// Speed the exhaust leaves the nozzle at, along the ship's own -Z. What
    /// gives a parked ship a plume at all.
    eject: f32,
    /// Random sideways spread speed, which is what turns a line into a cone.
    spread: f32,
    /// Exponential drag, per second. Bleeds the ejection off so the cone opens
    /// and then stalls rather than running away.
    drag: f32,
    /// Growth over the life, as `half * (1 + t * grow)`.
    grow: f32,
    /// Colour at birth and at death, as `(hex, intensity)`. Real exhaust cools;
    /// this is the whole reason a mote is not one flat colour for its life.
    ///
    /// The intensities are deliberately modest — around 2, not around 5. A
    /// plume is *dozens of overlapping additive quads*, so the brightness that
    /// reaches the screen is the sum and not the sample, and an intensity
    /// picked to look right on one particle clips to flat white the moment
    /// fifteen of them stack. Everything the tone mapper clips is also hue
    /// stripped, which is how an afterburner authored blue arrives white.
    hot: (u32, f32),
    cool: (u32, f32),
    /// Per-particle alpha, for the same reason: it is the *density* that
    /// carries a plume, and alpha is what stops density becoming a white bar.
    alpha: f32,
}

/// `EMIT_CONFIG.move` — the idle cruise trail.
///
/// White-hot at the nozzle, cooling to the `0x66ddff` the JS uses flat.
const EMIT_MOVE: EmitMode = EmitMode {
    rate: 70.0,
    scale: (0.34, 0.58),
    // Short, because the plume has to *stay in frame*. The chase camera sits 11
    // units behind the ship, so a particle receding at 30 units a second is past
    // the lens in a third of a second and everything after that is life spent
    // off screen — which is what the old 0.30 s at a full 80 u/s of recession
    // was: a trail drawn almost entirely behind the viewer.
    life: (0.24, 0.40),
    jitter: 0.06,
    inherit: 0.70,
    eject: 7.0,
    spread: 1.4,
    drag: 2.2,
    grow: 2.0,
    hot: (0xe6f6ff, 1.25),
    cool: (0x2f7dff, 0.55),
    alpha: 0.55,
};
/// `EMIT_CONFIG.boost`.
const EMIT_BOOST: EmitMode = EmitMode {
    rate: 150.0,
    scale: (0.42, 0.76),
    life: (0.28, 0.46),
    jitter: 0.14,
    inherit: 0.72,
    eject: 12.0,
    spread: 1.7,
    drag: 2.6,
    grow: 2.4,
    hot: (0xd6f2ff, 2.0),
    cool: (0x1e6cff, 0.85),
    alpha: 0.55,
};
/// `EMIT_CONFIG.brake`.
const EMIT_BRAKE: EmitMode = EmitMode {
    rate: 120.0,
    scale: (0.36, 0.66),
    life: (0.30, 0.48),
    jitter: 0.12,
    // Retro-thrust blows *forward*, so it keeps less of the ship's momentum and
    // leaves faster — the plume overtakes the aircraft, which is the read.
    inherit: 0.35,
    eject: 20.0,
    spread: 2.2,
    drag: 3.0,
    grow: 2.2,
    hot: (0xffe9a8, 1.9),
    cool: (0xff4400, 0.70),
    alpha: 0.55,
};

/// How much of a particle's own velocity is smeared into its quad, in seconds.
///
/// A particle in a plume moves several of its own diameters per frame, so a
/// round billboard reads as a bead on a string no matter how many there are.
/// Drawing it stretched along its velocity instead — the same trick a motion
/// blur is — bridges the gap between one particle and the next for no extra
/// particles and no extra vertices, and it is what makes the plume a *plume*.
///
/// 14 ms, so a mote moving at 40 u/s is stretched by half a unit. Small enough
/// that a slow particle stays a puff, which is why the branch in `build_surface`
/// only takes the streak when the smear is worth more than the radius.
const MOTE_SMEAR: f32 = 0.014;

/// `main.js:1143`: below this speed a coasting ship emits nothing.
const TRAIL_MIN_SPEED: f32 = 5.0;

// ---------------------------------------------------------------------------
// Battle damage
// ---------------------------------------------------------------------------
//
// Nothing in the JS client to port: a damaged ship there is a health bar and a
// red flash on the hull and nothing else, so from outside there is no way to
// tell a fresh enemy from one that is one burst from dead. That is the gap this
// fills, and it is worth more on *other* people's ships than on your own —
// watching a bandit start to stream is the read the game was missing.
//
// # Why the smoke is light and not dark
//
// This module has exactly one material and it is `AlphaMode::Add` — see the
// header. Additive blending can only ever *add* light, so genuinely dark smoke
// is not available here at any alpha: a black puff over space contributes
// nothing at all. The plume is therefore a dim, desaturated haze, deliberately
// held **below** the camera's 0.9 bloom prefilter threshold so it reads as
// matter catching the light rather than as something glowing. The fire above it
// is the opposite and is authored well over that threshold.
//
// Buying real dark smoke means a second mesh on `AlphaMode::Blend`, which is a
// second draw call *and* a sorted pass that a single unsorted buffer cannot
// serve correctly — the exact trade the module header rejects. Not worth it for
// one effect.

/// Hull fraction at or below which a ship begins to stream.
const SMOKE_AT: f32 = 0.6;
/// Hull fraction at or below which fire takes at the wing root.
const FIRE_AT: f32 = 0.28;
/// Smoke particles a second at zero hull, ramping up from nothing at
/// [`SMOKE_AT`].
const SMOKE_RATE: f32 = 64.0;
/// Fire particles a second at zero hull, from nothing at [`FIRE_AT`].
const FIRE_RATE: f32 = 46.0;

/// Anchors for the damage plume, in **unfitted ship** units — the space
/// `cockpit.rs`'s profiles are authored in and the space
/// [`crate::scene::ship_fit`] maps *from*.
///
/// This is the piece that has to go through the fit rather than round it. The
/// hull is drawn at `q' = scale * (q + offset)` and an anchor that skips that
/// stays where `spaceship.glb` put it while the aircraft moves out from under
/// it — which for `jet.glb`, fitted at 1.62 and shifted nose-ward, is a plume
/// hanging in the air behind the tailfins. `cockpit.rs` hit exactly this and
/// solved it the same way; `trail_offsets` below is the older, per-model form
/// of the same idea.
///
/// Mid-wing, and **just aft of the trailing edge** rather than on it. Ships pick
/// one anchor or the other by id (see [`emit_damage`]), so the damage reads as
/// damage to a *place* rather than as an aura around the whole aircraft.
///
/// The clearance behind the wing is not cosmetic. These quads are transparent
/// and still depth-test against the hull, so an anchor *on* the skin buries
/// every puff inside solid geometry and the plume is rejected wholesale —
/// which is exactly what the first pass looked like on screen: a ship at 12
/// hit points with nothing coming off it. The trailing edge is also where a
/// plume physically is, since a particle does not move once released and the
/// aircraft flies out from in front of it.
const DAMAGE_ANCHORS_LEGACY: [Vec3; 2] =
    [Vec3::new(-2.90, 0.00, -1.80), Vec3::new(2.90, 0.00, -1.80)];

/// The same two points on the F-22 airframe.
///
/// Its unfitted geometry is the one `JET_PROFILE` measures against — the canopy
/// is x ±0.268, z 0.78..1.97 — and inverting [`TRAIL_OFFSETS_JET`] back through
/// the fit puts the nozzles at x ±0.28, z -2.87, which pins the tail. So ±1.15
/// is a little over half way out the span and -2.875 is the nozzles' own plane
/// — outboard of the exhaust, on the wing's trailing edge, in the same clear
/// air the engine trails already draw into without being cut by the hull.
///
/// Further aft than this and the plume visibly detaches: the chase camera looks
/// slightly *down* at the ship, so a point behind it projects low on the screen
/// and reads as a blob hanging under the aircraft rather than as coming off it.
const DAMAGE_ANCHORS_JET: [Vec3; 2] = [
    Vec3::new(-1.15, -0.14, -2.875),
    Vec3::new(1.15, -0.14, -2.875),
];

/// The damage anchors for whichever hull is flying, in ship space.
///
/// Unlike [`trail_offsets`], which multiplies its per-model constants by
/// [`crate::scene::SHIP_SCALE`] alone, this runs the anchors through the *whole*
/// fit — `ship_fit` folds `SHIP_SCALE` in, so the two agree for
/// `spaceship.glb`, where the model fit is the identity, and only this one is
/// right for a hull the fit actually moves.
fn damage_offsets() -> [Vec3; 2] {
    let (scale, offset) = crate::scene::ship_fit();
    let anchors = if jet_hull() {
        DAMAGE_ANCHORS_JET
    } else {
        DAMAGE_ANCHORS_LEGACY
    };
    anchors.map(|q| (q + offset) * scale)
}

/// Whether `jet.glb` is the hull in the air.
///
/// Asks `scene::ship_model()` rather than reading `SPACESHIPS_SHIP_MODEL`
/// itself. Those are not the same question and the difference was a live bug:
/// when the jet became the *default* model, an environment variable nobody had
/// set answered "no", so the nozzles fell back to `spaceship.glb`'s — out at
/// x = +-2.2, on the wings — while the jet was on screen. One function decides
/// which hull is flying; everything anchored to that hull asks it.
fn jet_hull() -> bool {
    crate::scene::ship_model().contains("jet")
}

/// `SPACESHIPS_HULL=0.4`: pins every ship's hull fraction.
///
/// A screenshot hook, for the same reason `SPACESHIPS_COCKPIT` and
/// `SPACESHIPS_SCREENSHOT` exist — a visual check of what a burning ship looks
/// like should not need somebody to sit and be shot down first, and the states
/// worth looking at are exactly the ones you cannot hold still in.
///
/// `pub(crate)` and cached in a [`OnceLock`] because `hud.rs` reads it too: the
/// hull tape and the plume have to agree about how hurt the ship is, and one
/// definition is how they cannot disagree. Native only; there is no environment
/// on the web, and the whole hook compiles out there.
pub(crate) fn forced_hull() -> Option<f32> {
    #[cfg(target_arch = "wasm32")]
    return None;

    #[cfg(not(target_arch = "wasm32"))]
    {
        static FORCED: std::sync::OnceLock<Option<f32>> = std::sync::OnceLock::new();
        *FORCED.get_or_init(|| {
            std::env::var("SPACESHIPS_HULL")
                .ok()
                .and_then(|v| v.parse::<f32>().ok())
                .map(|v| v.clamp(0.0, 1.0))
        })
    }
}

/// `SPACESHIPS_FX_TRAIL=move|boost|brake`: pins the engine-trail emitter's mode.
///
/// The same kind of hook as [`forced_hull`] and for the same reason. A trail is
/// the one effect a still of a *parked* ship cannot show — the emitter is silent
/// below [`TRAIL_MIN_SPEED`] and the vertical slice spawns you stationary — so
/// without this there is no way to photograph a cruise plume or a boost plume,
/// let alone photograph the same one twice.
///
/// It pins the *mode*, not the motion: the ship still sits still, which is the
/// harder case for the emitter and therefore the more useful one to look at.
fn forced_trail() -> Option<&'static EmitMode> {
    #[cfg(target_arch = "wasm32")]
    return None;

    #[cfg(not(target_arch = "wasm32"))]
    {
        static FORCED: std::sync::OnceLock<Option<&'static EmitMode>> = std::sync::OnceLock::new();
        *FORCED.get_or_init(|| {
            match std::env::var("SPACESHIPS_FX_TRAIL")
                .ok()?
                .trim()
                .to_ascii_lowercase()
                .as_str()
            {
                "move" | "cruise" => Some(&EMIT_MOVE),
                "boost" => Some(&EMIT_BOOST),
                "brake" => Some(&EMIT_BRAKE),
                _ => None,
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Draws every projectile and effect into a single mesh.
pub struct WeaponsPlugin;

impl Plugin for WeaponsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Effects>()
            .add_systems(Startup, setup)
            // Effect *sources* are tick-rate state: an event happens on a tick
            // and a trail samples a pose on a tick. Emitting from `Update`
            // would emit twice on a frame that ran two ticks and not at all on
            // a frame that ran none, which is the same class of bug
            // `scene.rs`'s interpolation exists to avoid.
            .add_systems(
                FixedUpdate,
                (consume_events, emit_trails, emit_damage).after(SimSet),
            )
            // Ageing and the rebuild are per *frame*: they are what makes the
            // effects smooth on a display that is not the tick rate.
            // `stage_scene` runs before `place_missile_bodies` and after the
            // demo so the effect it holds still is the one the frame draws:
            // ageing would move it, the demo would bury it, and the bodies have
            // to see whatever it staged.
            .add_systems(
                Update,
                (run_demo, age_effects, stage_scene, place_missile_bodies).chain(),
            )
            // After transform propagation so the billboards face where the
            // camera actually ended up this frame rather than where it was
            // last frame. `camera::follow` writes the chase pose in
            // `PostUpdate` before `Propagate`.
            .add_systems(PostUpdate, build_surface.after(TransformSystems::Propagate));
    }

    /// The draw-call probe lives in the render world, which `RenderPlugin`
    /// creates during its own `build`. `finish` runs after every plugin's
    /// `build`, so the sub-app is certainly there by now.
    fn finish(&self, app: &mut App) {
        if std::env::var_os("SPACESHIPS_DRAWCALLS").is_some() {
            install_draw_call_probe(app);
        }
    }
}

// ---------------------------------------------------------------------------
// The brush atlas
// ---------------------------------------------------------------------------

/// A horizontal slice of the glow atlas. One material means one texture, so the
/// two shapes this module needs share an image and are selected by UV.
#[derive(Clone, Copy)]
struct Brush {
    u0: f32,
    u1: f32,
}

impl Brush {
    /// A near-solid disc with a soft rim: bolt cores, beams, explosion
    /// fragments — anything that should read as a hard edge rather than a haze.
    const CORE: Brush = Brush { u0: 0.5, u1: 1.0 };
    /// A soft radial falloff: halos, puffs, explosion shells, trail motes.
    const GLOW: Brush = Brush { u0: 0.0, u1: 0.5 };
}

/// Atlas cell size. 64 is plenty — every brush is a smooth radial ramp, and the
/// texture is magnified far past 1:1 in every use.
const ATLAS_CELL: u32 = 64;

/// Bakes the two brushes into one 128×64 RGBA image.
///
/// The shape rides entirely in **alpha**, with RGB left at white. `AlphaMode::Add`
/// premultiplies in the shader, so the fragment's contribution is
/// `vertex_colour.rgb × vertex_colour.a × texture.a` — colour and brightness
/// come from the vertex, silhouette from the texture. Alpha is never sRGB
/// encoded, so an `Rgba8UnormSrgb` texture still carries a linear ramp there.
fn build_glow_atlas() -> Image {
    let w = ATLAS_CELL * 2;
    let h = ATLAS_CELL;
    let mut data = vec![0u8; (w * h * 4) as usize];

    for y in 0..h {
        for x in 0..w {
            let cell = x / ATLAS_CELL;
            let cx = (x % ATLAS_CELL) as f32 / (ATLAS_CELL - 1) as f32 * 2.0 - 1.0;
            let cy = y as f32 / (h - 1) as f32 * 2.0 - 1.0;
            let r = (cx * cx + cy * cy).sqrt().min(1.0);

            let a = if cell == 0 {
                // Soft falloff. The exponent sets how much of the quad is
                // core versus haze; 2.2 leaves a bright centre that bloom
                // catches and a long tail that reads as glow.
                (1.0 - r).powf(2.2)
            } else {
                // Solid to 72% of the radius, then a smoothstep rim. Anti-
                // aliases the silhouette without the shape going soft.
                let t = ((1.0 - r) / 0.28).clamp(0.0, 1.0);
                t * t * (3.0 - 2.0 * t)
            };

            let i = ((y * w + x) * 4) as usize;
            data[i] = 255;
            data[i + 1] = 255;
            data[i + 2] = 255;
            data[i + 3] = (a * 255.0).round() as u8;
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
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    // Clamp, not repeat: a quad's UVs sit inside one cell and must not wrap
    // into its neighbour.
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: bevy::image::ImageAddressMode::ClampToEdge,
        address_mode_v: bevy::image::ImageAddressMode::ClampToEdge,
        ..ImageSamplerDescriptor::linear()
    });
    image
}

// ---------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------

/// Handles for the one mesh and the one material.
#[derive(Resource)]
struct EffectAssets {
    mesh: Handle<Mesh>,
}

/// Marks the single entity every effect is drawn through.
#[derive(Component)]
struct EffectSurface;

/// An expanding additive shell. `bullets.spawnExplosion` is one of these;
/// `missiles.spawnExplosion` and the flare burst are three.
struct Shell {
    pos: Vec3,
    age: f32,
    life: f32,
    /// Radius at birth and at death.
    from: f32,
    to: f32,
    /// Colour at birth and at death.
    ///
    /// The JS holds one colour for the whole life, and a fireball that is the
    /// same shade of orange from ignition to burnout is the single clearest
    /// tell that an explosion is a texture on a growing quad. A real one starts
    /// near-white and cools through yellow and orange into a dull red.
    color: LinearRgba,
    cool: LinearRgba,
    opacity: f32,
    /// Radius easing, as `t^ease`. Below 1 is fast-then-slow, which is what a
    /// blast wave does — it dumps its energy immediately and then coasts. The
    /// JS lerps linearly, so its fireball expands at a constant rate and reads
    /// like an inflating balloon.
    ease: f32,
    /// Alpha falloff, as `(1 - t)^fade`. Above 1 holds the shell bright and
    /// then drops it, instead of the linear ramp's long grey tail.
    fade: f32,
}

impl Default for Shell {
    fn default() -> Shell {
        Shell {
            pos: Vec3::ZERO,
            age: 0.0,
            life: 0.5,
            from: 1.0,
            to: 2.0,
            color: LinearRgba::WHITE,
            cool: LinearRgba::BLACK,
            opacity: 1.0,
            ease: 0.5,
            fade: 1.4,
        }
    }
}

/// A sustained hitscan beam, kept alive for [`BEAM_LIFE`] because
/// `SimEvent::Fired` reports it exactly once.
struct BeamFx {
    start: Vec3,
    end: Vec3,
    age: f32,
    color: LinearRgba,
}

/// One trail, exhaust, smoke, or spark particle.
///
/// `trails.js` calls these particles and then does not move them; everything
/// from [`Mote::vel`] down is what this module adds, and between them they are
/// the difference between a plume and a row of beads. See [`EmitMode`].
struct Mote {
    pos: Vec3,
    age: f32,
    life: f32,
    /// Half-extent at birth, in world units.
    half: f32,
    /// Per-second growth factor applied over the life, as
    /// `half * (1 + t * grow)`.
    grow: f32,
    /// Shrink factor, as `half * (1 - t * shrink)`. `trails.js` uses 0.45.
    shrink: f32,
    /// Colour at birth and at death, lerped over the life.
    color: LinearRgba,
    cool: LinearRgba,
    opacity: f32,

    /// World velocity, integrated every frame. Zero is `trails.js`'s behaviour.
    vel: Vec3,
    /// Exponential drag per second, as `vel /= 1 + drag * dt`.
    drag: f32,
    /// Seconds of the particle's own motion to smear its quad along. See
    /// [`MOTE_SMEAR`]; zero draws a round billboard whatever the speed.
    smear: f32,
    /// Which brush to draw with. Sparks want the hard-edged one — a spark is a
    /// glowing fragment, not a haze — and everything else wants the soft one.
    brush: Brush,
}

impl Default for Mote {
    fn default() -> Mote {
        Mote {
            pos: Vec3::ZERO,
            age: 0.0,
            life: 1.0,
            half: 0.5,
            grow: 0.0,
            shrink: 0.0,
            color: LinearRgba::WHITE,
            cool: LinearRgba::BLACK,
            opacity: 1.0,
            vel: Vec3::ZERO,
            drag: 0.0,
            smear: 0.0,
            brush: Brush::GLOW,
        }
    }
}

/// Linear interpolation in the working (linear-light) colour space, which is
/// where a lerp between two emitter colours is physically the average of the
/// two lights rather than a guess at one.
fn mix(a: LinearRgba, b: LinearRgba, t: f32) -> LinearRgba {
    LinearRgba::rgb(
        a.red + (b.red - a.red) * t,
        a.green + (b.green - a.green) * t,
        a.blue + (b.blue - a.blue) * t,
    )
}

/// Everything this module owns that is not in [`SimFrame`].
#[derive(Resource, Default)]
struct Effects {
    shells: Vec<Shell>,
    beams: Vec<BeamFx>,
    motes: Vec<Mote>,
    /// Battle-damage smoke and fire. Its own pool, on its own cap — see
    /// [`MAX_DAMAGE_MOTES`].
    damage: Vec<Mote>,
    /// EMP wavefronts. Its own pool for the same reason, and see
    /// [`MAX_PULSE_MOTES`] for the specific way sharing one went wrong.
    pulse: Vec<Mote>,
    /// Trail emission accumulators, keyed by ship id. A ship emitting at 45 Hz
    /// against a 60 Hz tick owes a fractional particle each tick.
    trail_debt: HashMap<i32, f32>,
    /// Missile exhaust accumulators, keyed by `ProjView::key`.
    exhaust_debt: HashMap<u64, f32>,
    /// Damage emission accumulators, keyed by ship id. Same fractional-particle
    /// problem [`Effects::trail_debt`] solves, and two emitters per ship — one
    /// for smoke and one for fire — so the entry is a pair.
    damage_debt: HashMap<i32, [f32; 2]>,
    rng: Rng,
    /// `SPACESHIPS_FX_DEMO` state; empty unless the variable is set.
    demo: Demo,
}

/// Cosmetic randomness. Deliberately local and deliberately *not* `sim`'s RNG:
/// nothing here feeds the simulation, so nothing here has to be deterministic
/// across machines.
struct Rng(u64);

impl Default for Rng {
    fn default() -> Rng {
        Rng(0x9E37_79B9_7F4A_7C15)
    }
}

impl Rng {
    fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    fn unit(&mut self) -> f32 {
        self.next_u32() as f32 / u32::MAX as f32
    }

    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }

    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[(self.next_u32() as usize) % xs.len()]
    }

    /// A direction drawn uniformly over the sphere.
    ///
    /// Sampling `z` uniformly and the azimuth uniformly is the exact
    /// area-preserving parameterisation — the naive "random angles" version
    /// clumps at the poles, and a spark burst that clumps at two opposite poles
    /// reads as two jets rather than as a burst.
    fn direction(&mut self) -> Vec3 {
        let z = self.range(-1.0, 1.0);
        let a = self.range(0.0, std::f32::consts::TAU);
        let r = (1.0 - z * z).max(0.0).sqrt();
        Vec3::new(r * a.cos(), r * a.sin(), z)
    }
}

/// An sRGB hex from the JS, in the linear space the shader works in.
fn hex(h: u32) -> LinearRgba {
    LinearRgba::from(Color::srgb_u8(
        ((h >> 16) & 0xff) as u8,
        ((h >> 8) & 0xff) as u8,
        (h & 0xff) as u8,
    ))
}

/// `hex`, scaled past 1.0 so the bloom prefilter (threshold 0.9, see
/// `camera.rs`) has something to find. This is `graphics.js`'s
/// `color.multiplyScalar(glowBoost)` with the per-effect intensity folded in.
fn glow(h: u32, intensity: f32) -> LinearRgba {
    let c = hex(h);
    LinearRgba::rgb(
        c.red * intensity * ULTRA_GLOW,
        c.green * intensity * ULTRA_GLOW,
        c.blue * intensity * ULTRA_GLOW,
    )
}

// ---------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    // `RenderAssetUsages::default()` is both worlds. The main-world copy has to
    // survive, because `build_surface` rewrites it every frame; the default of
    // dropping it after upload is for meshes that never change.
    let mesh = meshes.add(Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    ));

    let material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: Some(images.add(build_glow_atlas())),
        // No lighting: these are emitters. `unlit` also short-circuits the
        // whole PBR block, so `base_color` reaches the framebuffer untouched
        // and a vertex colour of 5.0 stays 5.0.
        unlit: true,
        // Premultiplied-alpha blend with the destination alpha zeroed, which
        // is additive and therefore order-independent — the one blend mode a
        // single unsorted mesh can use correctly.
        alpha_mode: AlphaMode::Add,
        // Billboards are built facing the camera, but a beam or a bolt is
        // aligned to its own direction and can present either face.
        cull_mode: None,
        double_sided: true,
        ..default()
    });

    commands.spawn((
        Mesh3d(mesh.clone()),
        MeshMaterial3d(material),
        // Bevy computes an `Aabb` when a mesh is *added*, not when its contents
        // change under it. Without this the effects would be culled against
        // whatever the buffer happened to hold on frame one — usually empty,
        // so they would never draw at all.
        NoFrustumCulling,
        NotShadowCaster,
        EffectSurface,
    ));

    commands.insert_resource(EffectAssets { mesh });

    // -- The missile bodies ---------------------------------------------------
    //
    // One mesh and one material, shared by the whole pool, so every missile in
    // the air batches into a single draw call. Unlike the surface above, this
    // mesh is built here and never written again.
    let body_mesh = meshes.add(missile_mesh());
    let body_material = materials.add(StandardMaterial {
        // White, because the paint rides in `ATTRIBUTE_COLOR`: the PBR shader
        // multiplies the vertex colour into `base_color`, which is what lets
        // one material carry the JS's three.
        base_color: Color::WHITE,
        // **Lit, where `missiles.js:66` is `MeshBasicMaterial` and therefore
        // is not.** Three flat greys read as a paper cutout in a scene where
        // everything else — the hull, the rocks, the moon — is shaded, and a
        // rocket that does not catch the key light is the same silhouette
        // problem the billboard had, one step less bad. The hexes are the JS's
        // exactly; only the shading model changed.
        perceptual_roughness: 0.55,
        metallic: 0.10,
        ..default()
    });
    let slots: Vec<Entity> = (0..MISSILE_BODY_POOL)
        .map(|_| {
            commands
                .spawn((
                    MissileBody,
                    Mesh3d(body_mesh.clone()),
                    MeshMaterial3d(body_material.clone()),
                    Transform::default(),
                    // Nothing is in the air on frame one, and a slot that is
                    // never used is never extracted.
                    Visibility::Hidden,
                    // `graphics.js`: small props stay out of the shadow pass. A
                    // 3.5-unit body 600 units from the key light contributes a
                    // shadow nobody can see, at the cost of a second batch.
                    NotShadowCaster,
                ))
                .id()
        })
        .collect();
    commands.insert_resource(MissileBodies { slots });
}

// ---------------------------------------------------------------------------
// The missile body
// ---------------------------------------------------------------------------

/// One slot of the missile-body pool. Never spawned or despawned after
/// [`setup`]; see the module docs.
#[derive(Component)]
struct MissileBody;

/// The pool, in a stable order.
///
/// A `Vec<Entity>` rather than relying on query iteration order, for the same
/// reason `scene.rs` keeps its own id-to-entity `Registry`: an archetype's
/// iteration order is an implementation detail, and a slot has to be the *same*
/// slot from one frame to the next or the pool is just a bag.
#[derive(Resource)]
struct MissileBodies {
    slots: Vec<Entity>,
}

/// Positions, normals, vertex colours and indices for a solid mesh.
///
/// Deliberately not [`MeshBuild`]: that one is cleared and rewritten every
/// frame, carries UVs for the brush atlas, and only ever emits camera-facing
/// quads. This one is filled once, has no texture to address, and emits
/// triangles whose winding matters.
#[derive(Default)]
struct SolidBuild {
    pos: Vec<[f32; 3]>,
    normal: Vec<[f32; 3]>,
    color: Vec<[f32; 4]>,
    index: Vec<u32>,
}

impl SolidBuild {
    /// Pushes a vertex and hands back its index.
    fn vertex(&mut self, p: Vec3, n: Vec3, c: LinearRgba) -> u32 {
        let i = self.pos.len() as u32;
        self.pos.push(p.to_array());
        self.normal.push(n.to_array());
        self.color.push([c.red, c.green, c.blue, 1.0]);
        i
    }

    /// One triangle, wound **counter-clockwise seen from outside**.
    ///
    /// That is Bevy's convention (`FrontFace::Ccw` with back-face culling), and
    /// it is equivalent to saying `(b - a) × (c - a)` points the same way as the
    /// vertex normals — which is what `every_face_of_the_body_winds_outward`
    /// asserts below, so a sign slip here fails a test rather than producing a
    /// missile that is inside out on screen and nowhere else.
    fn tri(&mut self, a: u32, b: u32, c: u32) {
        self.index.extend_from_slice(&[a, b, c]);
    }

    /// A surface of revolution about the local Z axis: the ring of radius `r0`
    /// at `z0` joined to the ring of radius `r1` at `z1`, with `z1 > z0`.
    ///
    /// Every round part of the missile is one of these. A cylinder is the case
    /// where the radii match, a cone is the case where one of them is zero, and
    /// the nozzle bell is neither.
    ///
    /// Normals are per-vertex and follow the slope, so the barrel shades
    /// smoothly around its circumference rather than faceting — a ten-sided
    /// prism with flat normals is what a low-poly missile looks like, and it is
    /// not what this is meant to look like.
    fn frustum(&mut self, z0: f32, r0: f32, z1: f32, r1: f32, seg: u32, c: LinearRgba) {
        let dz = z1 - z0;
        for i in 0..seg {
            let (t0, t1) = (
                i as f32 / seg as f32 * std::f32::consts::TAU,
                (i + 1) as f32 / seg as f32 * std::f32::consts::TAU,
            );
            let (da, db) = (
                Vec3::new(t0.cos(), t0.sin(), 0.0),
                Vec3::new(t1.cos(), t1.sin(), 0.0),
            );
            // The outward normal of a cone's flank: radial, tilted along the
            // axis by the taper. Reduces to purely radial when `r0 == r1`.
            let (na, nb) = (
                (da * dz + Vec3::Z * (r0 - r1)).normalize(),
                (db * dz + Vec3::Z * (r0 - r1)).normalize(),
            );
            let a0 = self.vertex(da * r0 + Vec3::Z * z0, na, c);
            let b0 = self.vertex(db * r0 + Vec3::Z * z0, nb, c);
            let a1 = self.vertex(da * r1 + Vec3::Z * z1, na, c);
            let b1 = self.vertex(db * r1 + Vec3::Z * z1, nb, c);
            // A ring of radius zero collapses to a point, and the triangle that
            // touches it twice is degenerate. Skipping it is cheaper than
            // rasterising nothing, and it keeps the winding test honest — a
            // zero-area triangle has no winding to check.
            if r0 > 0.0 {
                self.tri(a0, b0, b1);
            }
            if r1 > 0.0 {
                self.tri(a0, b1, a1);
            }
        }
    }

    /// A flat disc in the plane `z`, facing `+Z` when `facing` is positive and
    /// `-Z` when it is negative. What closes a frustum off.
    fn disc(&mut self, z: f32, r: f32, facing: f32, seg: u32, c: LinearRgba) {
        let n = Vec3::Z * facing.signum();
        for i in 0..seg {
            let (t0, t1) = (
                i as f32 / seg as f32 * std::f32::consts::TAU,
                (i + 1) as f32 / seg as f32 * std::f32::consts::TAU,
            );
            let centre = self.vertex(Vec3::Z * z, n, c);
            let a = self.vertex(Vec3::new(t0.cos() * r, t0.sin() * r, z), n, c);
            let b = self.vertex(Vec3::new(t1.cos() * r, t1.sin() * r, z), n, c);
            if facing >= 0.0 {
                self.tri(centre, a, b);
            } else {
                self.tri(centre, b, a);
            }
        }
    }

    /// One flat face, as two triangles. Corners counter-clockwise seen from
    /// outside.
    fn face(&mut self, corners: [Vec3; 4], n: Vec3, c: LinearRgba) {
        let [a, b, cc, d] = corners.map(|p| self.vertex(p, n, c));
        self.tri(a, b, cc);
        self.tri(a, cc, d);
    }

    /// An axis-aligned box. `missiles.js` builds both fins out of one of these.
    fn slab(&mut self, centre: Vec3, half: Vec3, c: LinearRgba) {
        let (x, y, z) = (half.x, half.y, half.z);
        let p = |sx: f32, sy: f32, sz: f32| centre + Vec3::new(x * sx, y * sy, z * sz);
        for (corners, n) in [
            (
                [
                    p(-1.0, -1.0, 1.0),
                    p(1.0, -1.0, 1.0),
                    p(1.0, 1.0, 1.0),
                    p(-1.0, 1.0, 1.0),
                ],
                Vec3::Z,
            ),
            (
                [
                    p(1.0, -1.0, -1.0),
                    p(-1.0, -1.0, -1.0),
                    p(-1.0, 1.0, -1.0),
                    p(1.0, 1.0, -1.0),
                ],
                Vec3::NEG_Z,
            ),
            (
                [
                    p(1.0, -1.0, 1.0),
                    p(1.0, -1.0, -1.0),
                    p(1.0, 1.0, -1.0),
                    p(1.0, 1.0, 1.0),
                ],
                Vec3::X,
            ),
            (
                [
                    p(-1.0, -1.0, -1.0),
                    p(-1.0, -1.0, 1.0),
                    p(-1.0, 1.0, 1.0),
                    p(-1.0, 1.0, -1.0),
                ],
                Vec3::NEG_X,
            ),
            (
                [
                    p(1.0, 1.0, -1.0),
                    p(-1.0, 1.0, -1.0),
                    p(-1.0, 1.0, 1.0),
                    p(1.0, 1.0, 1.0),
                ],
                Vec3::Y,
            ),
            (
                [
                    p(-1.0, -1.0, -1.0),
                    p(1.0, -1.0, -1.0),
                    p(1.0, -1.0, 1.0),
                    p(-1.0, -1.0, 1.0),
                ],
                Vec3::NEG_Y,
            ),
        ] {
            self.face(corners, n, c);
        }
    }
}

/// `makeMissileMesh` (`missiles.js:143`), as vertices.
///
/// Five parts, three colours, nose along local `+Z` — the same axis
/// `missiles.js:291` points its root down with
/// `setFromUnitVectors((0, 0, 1), dir)`, which is why
/// [`place_missile_bodies`] can rotate `Vec3::Z` onto `ProjView::dir` and be
/// done.
///
/// # Where this deliberately departs from the JS
///
/// **Both of the JS's cones point the wrong way, and the nose is the one that
/// shows.** `missiles.js:27` builds `ConeGeometry(BODY_RAD, NOSE_LEN)` — apex at
/// `+y` — and then rotates it with `rotateX(-PI/2)`, which sends `+y` to `-z`.
/// Evaluated, the nose spans z 1.75 to 3.55 with **radius 0 at the back and 0.28
/// at the front**: a funnel that flares forward into a flat disc, not a nose
/// cone. `missiles.js:48` does the same thing to the nozzle with
/// `rotateX(PI/2)` followed by `rotateX(PI)`, and the tail comes to a spike.
///
/// Reproducing that faithfully would be reproducing a sign error, and it is
/// precisely the part a player looks at. So the nose tapers to a point at the
/// front, and the bell flares aft from a throat ([`MISSILE_BELL_THROAT_R`]) to
/// the JS's own mouth radius over the JS's own length. Every *proportion* —
/// `BODY_LEN`, `BODY_RAD`, `NOSE_LEN`, the fin box, `NOZZLE_Z` — is the JS's
/// unchanged, and so are all three colours.
fn missile_geometry() -> SolidBuild {
    let hull = hex(MISSILE_HULL_HEX);
    let fin = hex(MISSILE_FIN_HEX);
    let bell = hex(MISSILE_BELL_HEX);
    let seg = MISSILE_SEGMENTS;
    let (aft, fore) = (-MISSILE_BODY_LEN / 2.0, MISSILE_BODY_LEN / 2.0);

    let mut b = SolidBuild::default();

    // Fuselage: fatter at the tail, tapering forward. `missiles.js:32`.
    b.frustum(aft, MISSILE_BODY_RAD_AFT, fore, MISSILE_BODY_RAD, seg, hull);
    // Nose: the fuselage's forward radius drawn to a point `NOSE_LEN` ahead. No
    // cap between them — the two rings coincide, so the surface is continuous.
    b.frustum(
        fore,
        MISSILE_BODY_RAD,
        fore + MISSILE_NOSE_LEN,
        0.0,
        seg,
        hull,
    );
    // The aft face, as a base plate. Only its outer annulus is ever visible,
    // around the bell throat, and it is what a missile's tail actually looks
    // like from behind.
    b.disc(aft, MISSILE_BODY_RAD_AFT, -1.0, seg, hull);
    // The bell: necked down at the fuselage and flared to the mouth.
    b.frustum(
        aft - MISSILE_BELL_LEN,
        MISSILE_BELL_R,
        aft,
        MISSILE_BELL_THROAT_R,
        seg,
        bell,
    );
    // Closed at the mouth, in the bell's own dark grey. An open cone would show
    // the skybox through the back of the missile, since back faces are culled
    // and there is no interior.
    b.disc(aft - MISSILE_BELL_LEN, MISSILE_BELL_R, -1.0, seg, bell);

    // The two fins. `missiles.js:37` and `:42` are the same box turned ninety
    // degrees, which is what makes the cross.
    let depth = MISSILE_FIN_DEPTH / 2.0;
    let (span, thick) = (MISSILE_FIN_SPAN / 2.0, MISSILE_FIN_THICK / 2.0);
    let at = Vec3::new(0.0, 0.0, MISSILE_FIN_Z);
    b.slab(at, Vec3::new(span, thick, depth), fin);
    b.slab(at, Vec3::new(thick, span, depth), fin);

    b
}

/// [`missile_geometry`] as a [`Mesh`].
///
/// No UVs: the material has no texture, so `pbr.wgsl`'s `VERTEX_UVS` block is
/// not compiled in and an attribute nothing samples would be bytes uploaded for
/// nothing.
fn missile_mesh() -> Mesh {
    let b = missile_geometry();
    Mesh::new(
        PrimitiveTopology::TriangleList,
        // Both worlds, and not because anything here rewrites it: Bevy's
        // `calculate_bounds` reads the **main**-world copy to fit the entity's
        // `Aabb`, and a mesh dropped after upload gets no bounds and therefore
        // no frustum culling. Two hundred vertices held once is not a memory
        // decision worth making.
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, b.pos)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, b.normal)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, b.color)
    .with_inserted_indices(Indices::U32(b.index))
}

/// Points every live missile's body where it is going, and hides the rest of the
/// pool.
///
/// The whole per-frame cost of the body: one `Transform` and one `Visibility`
/// per missile in the air. Nothing is spawned, despawned, re-parented, or
/// rebuilt.
///
/// The extrapolation is [`build_surface`]'s, for the same reason — [`SimFrame`]
/// is the last completed *tick* and a missile covers 2.7 units in one, so
/// drawing it at the tick pose would staircase against the exhaust, which is
/// emitted on a finer interval and is therefore already smooth.
fn place_missile_bodies(
    frame: Res<SimFrame>,
    fx: Res<Effects>,
    fixed: Res<Time<Fixed>>,
    bodies: Res<MissileBodies>,
    mut q: Query<(&mut Transform, &mut Visibility), With<MissileBody>>,
) {
    let lead =
        RULES.weapons.missile_speed as f32 * sim::world::TICK_DT as f32 * fixed.overstep_fraction();

    let mut slots = bodies.slots.iter();
    for m in frame.0.missiles.iter().chain(fx.demo.missiles.iter()) {
        // Overflow drops the *body*, not the missile: the nozzle glow and the
        // exhaust still draw, so a shot beyond the pool is dimmer rather than
        // invisible. See `MISSILE_BODY_POOL` on how far off that is.
        let Some(&slot) = slots.next() else {
            break;
        };
        let Ok((mut tf, mut vis)) = q.get_mut(slot) else {
            continue;
        };
        // `try_normalize` and not `normalize`: a zero direction would give NaNs,
        // and a NaN transform takes the entity's bounding sphere with it.
        let dir = to_vec3(m.dir).try_normalize().unwrap_or(Vec3::Z);
        tf.translation = to_vec3(m.pos) + dir * lead;
        // The mesh is built nose-along-`+Z`, so this is the whole of the
        // orientation. `from_rotation_arc` handles the antiparallel case by
        // picking an arbitrary perpendicular axis, which is correct here: a body
        // of revolution has no roll to get wrong.
        tf.rotation = Quat::from_rotation_arc(Vec3::Z, dir);
        vis.set_if_neq(Visibility::Inherited);
    }

    // Everything the frame did not need. `set_if_neq` so a pool that is mostly
    // idle — which it is, most of the time — writes nothing at all.
    for &slot in slots {
        if let Ok((_, mut vis)) = q.get_mut(slot) {
            vis.set_if_neq(Visibility::Hidden);
        }
    }
}

// ---------------------------------------------------------------------------
// Effect sources
// ---------------------------------------------------------------------------

fn consume_events(frame: Res<SimFrame>, mut fx: ResMut<Effects>) {
    for event in &frame.0.events {
        match *event {
            SimEvent::Fired {
                weapon,
                origin,
                dir,
                allegiance,
                ..
            } => match weapon {
                // For a beam `dir` is the *endpoint*, not a direction —
                // `SimEvent::Fired` says so, and it is the one field in this
                // enum whose meaning changes with a sibling.
                //
                // A beam is a shot like any other, so it takes the shooter's
                // halo. Its core stays the same white-hot channel: see
                // `BoltInk` on why the hue lives in the halo.
                WeaponKind::Beam => push_beam(
                    &mut fx.beams,
                    BeamFx {
                        start: to_vec3([origin.x as f32, origin.y as f32, origin.z as f32]),
                        end: to_vec3([dir.x as f32, dir.y as f32, dir.z as f32]),
                        age: 0.0,
                        color: bolt_ink(allegiance).halo,
                    },
                ),
                // A muzzle flash. `bullets.js` has none — the bolt spawns at
                // the muzzle and that reads as one — but the gun fires at
                // 20 Hz and a single frame of light at the barrel is what
                // sells it at this brightness.
                WeaponKind::Bullet | WeaponKind::Missile => {
                    let p = to_vec3([origin.x as f32, origin.y as f32, origin.z as f32]);
                    let d = to_vec3([dir.x as f32, dir.y as f32, dir.z as f32]);
                    muzzle_flash(&mut fx, p, d, allegiance);
                }
            },

            SimEvent::FlareBurst { origin, .. } => {
                let p = to_vec3([origin.x as f32, origin.y as f32, origin.z as f32]);
                spawn_explosion(&mut fx, ExplosionKind::FlareBurst, p, 1.0);
            }

            SimEvent::Explosion { pos, scale, kind } => {
                let p = to_vec3([pos.x as f32, pos.y as f32, pos.z as f32]);
                spawn_explosion(&mut fx, kind, p, scale as f32);
            }

            SimEvent::EmpBurst { origin, radius, .. } => {
                let p = to_vec3([origin.x as f32, origin.y as f32, origin.z as f32]);
                spawn_emp(&mut fx, p, radius as f32);
            }

            _ => {}
        }
    }
}

/// A muzzle flash, as a flash and not as a dot.
///
/// `bullets.js` has none — the bolt spawns at the muzzle and that reads as one —
/// but the gun fires at 20 Hz and a single frame of light at the barrel is what
/// sells it at this brightness.
///
/// It was one soft disc, which is what a *point* light looks like and not what
/// a gun looks like. A gun throws its flash forward along the barrel, so this is
/// a short hot streak on the firing axis with a small bloom at its root: two
/// quads, alive for four frames.
///
/// It cools to the shooter's own halo, which is the cheapest possible way to
/// answer "who just opened up on me" — the flash is at the muzzle, so its colour
/// names the ship as well as the shot.
fn muzzle_flash(fx: &mut Effects, p: Vec3, dir: Vec3, allegiance: Allegiance) {
    // The same two hexes the bolt uses, at the flash's own intensities — which
    // are the ones this effect already shipped for the local palette.
    let (core, halo) = BOLT_HEX[allegiance as usize];
    push_shell(
        &mut fx.shells,
        Shell {
            pos: p + dir * 1.1,
            life: 0.07,
            from: 0.55,
            to: 1.5,
            color: glow(core, 3.0),
            cool: glow(halo, 0.6),
            opacity: 0.9,
            ease: 0.55,
            fade: 1.2,
            ..default()
        },
    );
    // The forward lance. A `Mote` rather than a `Shell` because only a mote can
    // carry a direction, and `smear` against a fixed velocity is exactly the
    // aligned streak this wants.
    push_mote(
        &mut fx.motes,
        Mote {
            pos: p + dir * 2.0,
            life: 0.06,
            half: 0.30,
            shrink: 0.7,
            color: glow(core, 4.0),
            cool: glow(halo, 0.8),
            opacity: 0.95,
            vel: dir * 90.0,
            smear: 0.030,
            brush: Brush::CORE,
            ..default()
        },
    );
}

/// Every explosion in the game, in one place.
///
/// Factored out of [`consume_events`] so the screenshot harness below can stage
/// the *same* effect the simulation raises rather than a hand-copied likeness of
/// it — a before/after pair taken against a second implementation would be
/// comparing the harness, not the effect.
///
/// # Why this is no longer one quad
///
/// `bullets.spawnExplosion` is a single additive sphere lerping from `s * 0.4`
/// to `s * 2.6` over 0.55 s in one flat orange. Ported faithfully, a ship dying
/// was a symmetrical beige blob that grew and faded — an out-of-focus lamp, with
/// nothing in it to read as *matter coming apart*. The three things it was
/// missing, in order of how much each buys:
///
/// 1. **Sparks.** A burst throws fragments outward. Radial streaks are what the
///    eye reads as an explosion; a disc is what it reads as a light. They come
///    out of the same [`Effects::motes`] pool as the trails, so they cost no new
///    machinery — only [`Mote::vel`], which the trails needed anyway.
/// 2. **A flash.** One frame of something much brighter and much smaller than
///    the fireball, well over the bloom threshold, so the moment of detonation
///    is distinct from the burning that follows.
/// 3. **Cooling.** A fireball that goes white → yellow → orange → dull red over
///    its life, rather than holding one colour and dimming.
fn spawn_explosion(fx: &mut Effects, kind: ExplosionKind, p: Vec3, s: f32) {
    let spec = match kind {
        ExplosionKind::MissileHit => &MISSILE_HIT,
        ExplosionKind::FlareBurst => &FLARE_BURST,
        ExplosionKind::AsteroidBreak => &ASTEROID_BREAK,
        ExplosionKind::ShipDeath => &SHIP_DEATH,
        ExplosionKind::Impact => &IMPACT,
    };
    let scale = s.max(spec.min_scale);
    push_shells(&mut fx.shells, p, scale, spec.shells);
    push_sparks(fx, p, scale, spec);
}

/// One explosion recipe: the shells it draws, and the sparks it throws.
struct Burst {
    /// Shells, from the innermost flash outward.
    shells: &'static [ShellSpec],
    /// Floor on the caller's scale, so a hit reported at a hair's width is
    /// still visible.
    min_scale: f32,
    /// How many fragments to throw, and how fast in units per second per unit
    /// of scale.
    sparks: usize,
    spark_speed: f32,
    /// Fragment colours, hot then burnt out.
    spark_hot: (u32, f32),
    spark_cool: (u32, f32),
    /// Fragment lifetime range, in seconds.
    spark_life: (f32, f32),
    /// Fragment half-width, in **world units and not units of scale**.
    ///
    /// A chip of hull is a chip of hull whether it came off a bullet strike or
    /// off a ship coming apart; what the blast size changes is how many there
    /// are and how hard they are thrown, not how big each one is. Scaling the
    /// width with the burst gave a ship death fragments a metre and a half
    /// across, which read as a ring of white petals rather than as debris.
    spark_half: (f32, f32),
}

/// One shell of a [`Burst`]: `(hot, hot intensity, cool, cool intensity, from,
/// to, life, opacity)`, with the radii in units of the burst's scale.
struct ShellSpec {
    hot: (u32, f32),
    cool: (u32, f32),
    from: f32,
    to: f32,
    life: f32,
    opacity: f32,
}

/// `bullets.spawnExplosion` at a bullet's scale: a bolt striking a hull or a
/// rock. Small, brief, and mostly sparks — a strike throws chips, it does not
/// make a fireball.
const IMPACT: Burst = Burst {
    shells: &[
        ShellSpec {
            hot: (0xffffff, 2.4),
            cool: (0xffcc66, 0.9),
            from: 0.35,
            to: 1.6,
            life: 0.10,
            opacity: 1.0,
        },
        ShellSpec {
            hot: (0xffbb66, 1.15),
            cool: (0x881a00, 0.14),
            from: 0.5,
            to: 3.6,
            life: 0.32,
            opacity: 0.8,
        },
    ],
    min_scale: 0.8,
    sparks: 18,
    spark_speed: 30.0,
    spark_hot: (0xffe6b0, 2.2),
    spark_cool: (0xff3b00, 0.22),
    spark_life: (0.14, 0.36),
    spark_half: (0.09, 0.19),
};

/// A ship coming apart. The caller's scale is 6, so the fireball reaches about
/// 20 units and the fragments carry 90.
const SHIP_DEATH: Burst = Burst {
    shells: &[
        ShellSpec {
            hot: (0xffffff, 3.0),
            cool: (0xffddaa, 1.0),
            from: 0.25,
            to: 1.1,
            life: 0.12,
            opacity: 1.0,
        },
        ShellSpec {
            hot: (0xffd28a, 1.55),
            cool: (0xff3a00, 0.28),
            from: 0.35,
            to: 2.4,
            life: 0.45,
            opacity: 0.85,
        },
        ShellSpec {
            hot: (0xff6a1e, 0.60),
            cool: (0x2a0800, 0.04),
            from: 0.5,
            to: 4.0,
            life: 0.95,
            opacity: 0.45,
        },
    ],
    min_scale: 1.0,
    sparks: 46,
    spark_speed: 16.0,
    spark_hot: (0xffe0a8, 2.4),
    spark_cool: (0xff2000, 0.20),
    spark_life: (0.30, 1.00),
    spark_half: (0.09, 0.24),
};

/// A rock breaking. Dimmer and redder than a ship — rock does not burn — and
/// the fragments outlive the flash because that is the part that reads as
/// debris.
const ASTEROID_BREAK: Burst = Burst {
    shells: &[
        ShellSpec {
            hot: (0xffddaa, 1.6),
            cool: (0xaa4400, 0.4),
            from: 0.10,
            to: 0.55,
            life: 0.14,
            opacity: 0.9,
        },
        ShellSpec {
            hot: (0xd98a4a, 0.75),
            cool: (0x3a1400, 0.05),
            from: 0.16,
            to: 1.5,
            life: 0.70,
            opacity: 0.5,
        },
    ],
    min_scale: 1.0,
    sparks: 26,
    spark_speed: 5.0,
    spark_hot: (0xffc07a, 1.5),
    spark_cool: (0x501800, 0.08),
    spark_life: (0.45, 1.20),
    spark_half: (0.10, 0.30),
};

/// `missiles.spawnExplosion` — flash, fire, smoke.
const MISSILE_HIT: Burst = Burst {
    shells: &[
        ShellSpec {
            hot: (0xffffff, 3.0),
            cool: (0xffdd99, 1.0),
            from: 0.8,
            to: 4.2,
            life: 0.14,
            opacity: 1.0,
        },
        ShellSpec {
            hot: (0xffc866, 1.5),
            cool: (0xff3800, 0.26),
            from: 1.2,
            to: 10.0,
            life: 0.46,
            opacity: 0.85,
        },
        ShellSpec {
            hot: (0xff5511, 0.55),
            cool: (0x330800, 0.04),
            from: 1.8,
            to: 16.0,
            life: 0.80,
            opacity: 0.45,
        },
    ],
    min_scale: 0.6,
    sparks: 34,
    spark_speed: 48.0,
    spark_hot: (0xffe0a0, 2.4),
    spark_cool: (0xff2600, 0.18),
    spark_life: (0.28, 0.72),
    spark_half: (0.10, 0.26),
};

/// `spawnFlareBurst` — the same shape, sharper and smaller, and it stays
/// yellow-white because a decoy is burning magnesium rather than fuel.
const FLARE_BURST: Burst = Burst {
    shells: &[
        ShellSpec {
            hot: (0xffffff, 3.2),
            cool: (0xffee88, 1.2),
            from: 0.15,
            to: 3.2,
            life: 0.16,
            opacity: 1.0,
        },
        ShellSpec {
            hot: (0xffee44, 1.5),
            cool: (0xff7700, 0.30),
            from: 0.40,
            to: 6.5,
            life: 0.28,
            opacity: 0.85,
        },
        ShellSpec {
            hot: (0xff8800, 0.60),
            cool: (0x441000, 0.05),
            from: 0.70,
            to: 10.0,
            life: 0.40,
            opacity: 0.45,
        },
    ],
    min_scale: 0.6,
    sparks: 26,
    spark_speed: 36.0,
    spark_hot: (0xffffcc, 2.6),
    spark_cool: (0xff9900, 0.35),
    spark_life: (0.18, 0.52),
    spark_half: (0.07, 0.17),
};

/// Motes in the EMP front. Enough to read as a continuous shell at 300 units,
/// cheap enough that two overlapping pulses cannot evict the engine trails —
/// see [`MAX_MOTES`].
const EMP_MOTES: usize = 200;

/// How long the front takes to reach [`sim::rules::EmpRules::radius`].
///
/// The blackout is four seconds and this is well under one, deliberately: the
/// wave is the *announcement*, not the effect. By the time a pilot has
/// registered that their panel is dark, the thing that did it should already be
/// leaving.
///
/// Tuned by looking. At half this it is over before the eye finds it — three
/// hundred units in under half a second is faster than a bullet, and from the
/// centre it reads as one frame of glare and nothing else. This is slow enough
/// that the wall is a wall for a moment.
const EMP_FRONT_SECS: f32 = 0.9;

/// The EMP pulse: a cold front that runs out to the edge of the weapon's radius
/// and stops.
///
/// Not a [`Burst`], and not routed through [`spawn_explosion`], because it is
/// the one effect in this module that is not an explosion — nothing burns,
/// nothing comes apart, and there is nothing to throw fragments of.
///
/// # Why the wave is motes and not a [`Shell`]
///
/// It was three nested shells, which is what every other blast here is, and it
/// was wrong for one specific reason: **a `Shell` is a camera-facing billboard,
/// so it only reads as a sphere from outside**. Every other shell in this file
/// is a handful of units across and is looked at from tens of units away, so the
/// distinction never comes up. This one is three hundred units across and is
/// centred on the pilot who fired it — the camera is *inside* it — and a
/// billboard drawn from the inside is a quad across the whole viewport. The
/// screenshot was a white-out with a ship silhouetted in it.
///
/// A front of radial motes has no inside and no outside: it is a shell of
/// particles in world space, so it reads as an expanding wall from any camera,
/// and a camera at the centre watches it *leave* rather than being painted over
/// by it. The cost is a hundred quads for half a second, which is an eighth of
/// the mote budget.
///
/// # The rest of the recipe
///
/// - **Blue-white, cooling to a dead navy.** Every fireball here goes white →
///   yellow → red because that is what burning matter does; this is the one
///   effect that must not read as fire.
/// - **The front stops at the weapon's radius**, straight off
///   [`sim::world::SimEvent::EmpBurst`], so what you see is exactly who was
///   caught. Watching the wall pass over an aircraft and then watching that
///   aircraft go dark is the whole read, and a wave sized by taste would break
///   it.
/// - **Uniform speed, not the squared draw [`push_sparks`] uses.** Sparks want
///   to fill a volume; a wavefront wants to be a surface, and scattering the
///   speeds is precisely what would turn it back into a cloud.
/// - **One small, very bright flash at the origin**, well over the camera's
///   bloom threshold, so the moment of detonation has something to mark it. It
///   is deliberately smaller than the chase camera's own standoff, which is what
///   keeps *this* billboard outside the lens.
fn spawn_emp(fx: &mut Effects, pos: Vec3, radius: f32) {
    push_shell(
        &mut fx.shells,
        Shell {
            pos,
            life: 0.12,
            from: 0.4,
            to: 7.0,
            color: glow(0xffffff, 5.0),
            cool: glow(0x88ddff, 1.4),
            opacity: 1.0,
            ease: 0.5,
            fade: 1.4,
            ..default()
        },
    );

    let speed = radius / EMP_FRONT_SECS;
    let hot = glow(0xdff2ff, 2.6);
    let cool = glow(0x1b4d8a, 0.05);
    for _ in 0..EMP_MOTES {
        let dir = fx.rng.direction();
        // A little jitter on the speed only — enough that the wall has some
        // thickness and does not look like a wireframe sphere, not enough to
        // stop it being a wall.
        let speed = speed * fx.rng.range(0.93, 1.0);
        push_pulse(
            &mut fx.pulse,
            Mote {
                pos: pos + dir * 3.0,
                life: EMP_FRONT_SECS + 0.12,
                // Big, and growing, because the far side of this front is three
                // hundred units away: a trail-sized mote is a single pixel
                // there, and the wall would dissolve into static exactly as it
                // reached the aircraft it is about to blind.
                half: 3.0,
                grow: 2.4,
                color: hot,
                cool,
                opacity: 0.85,
                vel: dir * speed,
                // No drag: a wavefront that slowed down would bunch up short of
                // the radius it is supposed to describe.
                drag: 0.0,
                // Smeared along its own travel, so each mote is a radial dash
                // rather than a dot and the front reads as motion.
                smear: 0.014,
                brush: Brush::GLOW,
                ..default()
            },
        );
    }
}

fn push_shells(out: &mut Vec<Shell>, pos: Vec3, scale: f32, spec: &[ShellSpec]) {
    for s in spec {
        push_shell(
            out,
            Shell {
                pos,
                life: s.life,
                from: s.from * scale,
                to: s.to * scale,
                color: glow(s.hot.0, s.hot.1),
                cool: glow(s.cool.0, s.cool.1),
                opacity: s.opacity,
                ..default()
            },
        );
    }
}

/// Throws a [`Burst`]'s fragments.
///
/// Speed is randomised over a wide range on purpose: a burst where every
/// fragment leaves at the same speed is a hollow expanding shell of dots, which
/// is a worse artefact than the disc it replaced. Scattering the speeds fills
/// the volume.
fn push_sparks(fx: &mut Effects, pos: Vec3, scale: f32, spec: &Burst) {
    let hot = glow(spec.spark_hot.0, spec.spark_hot.1);
    let cool = glow(spec.spark_cool.0, spec.spark_cool.1);
    for _ in 0..spec.sparks {
        let dir = fx.rng.direction();
        // Squared, so most fragments stay in the fireball and a few outrun it.
        // A uniform draw puts them all in one band and the burst reads as a
        // ring of petals, which is what the first pass looked like.
        let u = fx.rng.range(0.16, 1.0);
        let speed = spec.spark_speed * scale * u * u;
        let life = fx.rng.range(spec.spark_life.0, spec.spark_life.1);
        let half = fx.rng.range(spec.spark_half.0, spec.spark_half.1);
        push_mote(
            &mut fx.motes,
            Mote {
                pos: pos + dir * (scale * 0.3),
                life,
                half,
                shrink: 0.8,
                color: hot,
                cool,
                opacity: 0.9,
                vel: dir * speed,
                // Space has no air, but a fragment that flies dead straight for
                // a second at a constant speed reads as a bug. The drag is a
                // cheat and it is the one that makes the burst look like it
                // happened rather than like it is still happening.
                drag: 1.5,
                // Well above `MOTE_SMEAR`: a fragment is a *streak*, and a
                // long thin one is the shape that reads as debris where a
                // round one reads as a bubble.
                smear: 0.038,
                brush: Brush::CORE,
                ..default()
            },
        );
    }
}

/// Engine trails. `scene.rs` leaves a `TODO(trails)` pointing at exactly this:
/// `BOOSTING`/`BRAKING` are already in `ShipView::flags`.
///
/// Emission is rate-based over time, as in `main.js`'s `EMIT_CONFIG`, not
/// distance-based — a hovering ship still smokes. The debt accumulator is what
/// lets a 45 Hz emitter run on a 60 Hz tick without beating against it.
fn emit_trails(frame: Res<SimFrame>, mut fx: ResMut<Effects>) {
    let dt = sim::world::TICK_DT as f32;

    // Ships that vanished this tick should not keep a debt entry forever.
    let live: Vec<i32> = frame.0.ships.iter().map(|s| s.id).collect();
    fx.trail_debt.retain(|id, _| live.contains(id));

    for ship in &frame.0.ships {
        if !ship.flags.contains(ShipFlags::ALIVE) || ship.flags.contains(ShipFlags::BOSS_HITBOX) {
            continue;
        }

        let speed = to_vec3(ship.vel).length();
        let mode = if let Some(forced) = forced_trail() {
            forced
        } else if ship.flags.contains(ShipFlags::BRAKING) {
            &EMIT_BRAKE
        } else if ship.flags.contains(ShipFlags::BOOSTING) {
            &EMIT_BOOST
        } else if speed > TRAIL_MIN_SPEED {
            &EMIT_MOVE
        } else {
            fx.trail_debt.remove(&ship.id);
            continue;
        };

        // Two nozzles, so the per-nozzle rate is the config rate.
        let debt = fx.trail_debt.entry(ship.id).or_insert(0.0);
        *debt += mode.rate * dt;
        let n = debt.floor();
        *debt -= n;
        let n = n as u32;
        if n == 0 {
            continue;
        }

        let quat = rot(ship.quat);
        let base = to_vec3(ship.pos);
        let vel = to_vec3(ship.vel);
        let back = quat * Vec3::NEG_Z;
        let right = quat * Vec3::X;
        let up = quat * Vec3::Y;

        let carried = carried_momentum(vel, back, mode.inherit);

        let hot = glow(mode.hot.0, mode.hot.1);
        let cool = glow(mode.cool.0, mode.cool.1);

        for offset in trail_offsets() {
            let nozzle = base + quat * offset;
            for i in 0..n {
                // Emission is a *rate*, but the pose is only sampled once a
                // tick, so every particle a tick owes would otherwise be
                // stamped at the same point — the plume comes out in clumps of
                // one, two, one, two as the debt accumulator beats against the
                // tick. Walking each particle back along the ship's own motion
                // to where the ship was when it was due, and starting it that
                // much aged, spaces them evenly instead. At 105 particles a
                // second on a 60 Hz tick that is the difference between pairs
                // and a stream.
                let slice = (i as f32 + 0.5) / n as f32;
                let back_dt = dt * (1.0 - slice);

                let j = mode.jitter;
                let jitter = Vec3::new(
                    fx.rng.range(-j, j),
                    fx.rng.range(-j, j),
                    fx.rng.range(-j, j),
                );
                let scale = fx.rng.range(mode.scale.0, mode.scale.1);
                let life = fx.rng.range(mode.life.0, mode.life.1);
                // Sideways only: a spread along the exhaust axis would just be
                // noise on the ejection speed, where across it opens the cone.
                let spread = right * fx.rng.range(-1.0, 1.0) * mode.spread
                    + up * fx.rng.range(-1.0, 1.0) * mode.spread;

                push_mote(
                    &mut fx.motes,
                    Mote {
                        pos: nozzle + jitter - vel * back_dt,
                        age: back_dt,
                        life,
                        // `trails.js` geometry is a radius-0.5 sphere scaled by
                        // `scale`, so the world half-extent is half of it.
                        half: scale * 0.5,
                        grow: mode.grow,
                        shrink: 0.45,
                        color: hot,
                        cool,
                        opacity: mode.alpha,
                        vel: carried + back * mode.eject + spread,
                        drag: mode.drag,
                        smear: MOTE_SMEAR,
                        ..default()
                    },
                );
            }
        }
    }
}

/// Battle damage: smoke that thickens as the hull goes, then fire.
///
/// Runs on **every** ship the frame carries, not just the local one. That is
/// most of the point — the health bar already tells you about your own hull, and
/// nothing at all used to tell you about anybody else's.
///
/// Shape is deliberately [`emit_trails`]'s: a rate that comes out of state, a
/// debt accumulator so a fractional particle per tick still emits at the right
/// average, and a fixed-capacity pool that drops its oldest. Nothing here
/// allocates a mesh, spawns an entity or touches the scene graph.
fn emit_damage(frame: Res<SimFrame>, mut fx: ResMut<Effects>) {
    let dt = sim::world::TICK_DT as f32;
    let max_hp = RULES.ship.max_hp as f32;
    let anchors = damage_offsets();
    let forced = forced_hull();

    let live: Vec<i32> = frame.0.ships.iter().map(|s| s.id).collect();
    fx.damage_debt.retain(|id, _| live.contains(id));

    for ship in &frame.0.ships {
        // A wreck stops smoking: the death explosion is the effect for that,
        // and a corpse trailing fire is a ship the player will keep shooting.
        // Boss hitboxes are twenty bodies wearing one hull and would light
        // twenty plumes.
        if !ship.flags.contains(ShipFlags::ALIVE) || ship.flags.contains(ShipFlags::BOSS_HITBOX) {
            fx.damage_debt.remove(&ship.id);
            continue;
        }

        let hull = forced.unwrap_or((ship.hp as f32 / max_hp).clamp(0.0, 1.0));
        // Linear in how far past each threshold the hull has fallen, so the
        // plume thickens continuously rather than switching on in stages: a
        // ship at 55% barely wisps and one at 10% is streaming.
        let smoke = ramp(hull, SMOKE_AT) * SMOKE_RATE;
        let fire = ramp(hull, FIRE_AT) * FIRE_RATE;
        if smoke <= 0.0 && fire <= 0.0 {
            fx.damage_debt.remove(&ship.id);
            continue;
        }

        let owed = {
            let debt = fx.damage_debt.entry(ship.id).or_insert([0.0; 2]);
            debt[0] += smoke * dt;
            debt[1] += fire * dt;
            let n = [debt[0].floor(), debt[1].floor()];
            debt[0] -= n[0];
            debt[1] -= n[1];
            [n[0] as u32, n[1] as u32]
        };

        // One wing, picked off the id and therefore stable for the life of the
        // ship. Both wings at once reads as an aura; one reads as a hit.
        let anchor = anchors[(ship.id.unsigned_abs() % 2) as usize];
        let quat = rot(ship.quat);
        let nozzle = to_vec3(ship.pos) + quat * anchor;
        let vel = to_vec3(ship.vel);
        let back = quat * Vec3::NEG_Z;
        // Outboard, away from the fuselage, so the plume clears the wing it is
        // anchored under instead of being drawn inside it.
        let out = quat * Vec3::X * anchor.x.signum();

        for _ in 0..owed[0] {
            let j = SMOKE_JITTER;
            let jitter = Vec3::new(
                fx.rng.range(-j, j),
                fx.rng.range(-j, j),
                fx.rng.range(-j, j),
            );
            let scale = fx.rng.range(1.1, 2.3);
            let life = fx.rng.range(0.9, 1.6);
            // Drawn before the push, for the borrow reason the flame loop below
            // documents: `fx.rng` and `fx.damage` are siblings and the checker
            // sees one `&mut fx` through the call.
            // A ship that is not moving still has to *stream*, or a damaged
            // hull holding station is a warm smudge welded to one wing. The
            // outboard component is what carries the plume clear of the wing it
            // is anchored under rather than leaving it drawn inside the skin.
            let drift = back * fx.rng.range(3.0, 9.0) + out * fx.rng.range(0.8, 3.2);
            push_damage(
                &mut fx.damage,
                Mote {
                    pos: nozzle + jitter,
                    life,
                    half: scale * 0.5,
                    // Grows and does not shrink: a puff of smoke expands as it
                    // is left behind, which is what turns a string of particles
                    // into a widening plume rather than a dotted line.
                    grow: 2.8,
                    shrink: 0.0,
                    // Lit at birth by the fire it came out of and dead by the
                    // end, which is the only handle additive blending gives on
                    // "this is matter, not a lamp": the *near* end of the plume
                    // glows and the far end does not.
                    color: SMOKE_HOT,
                    cool: SMOKE_COLD,
                    // Deliberately low, and it is the *count* that carries the
                    // effect rather than this number. Additive blending means a
                    // single puff contributes almost nothing over an unlit sky,
                    // so a plume is made of overlap — and pushing alpha instead
                    // of density is what turned an early pass into a small sun
                    // the moment three of them landed on the same pixel.
                    opacity: 0.13 + 0.13 * ramp(hull, SMOKE_AT),
                    // Smoke keeps most of the ship's momentum and bleeds it —
                    // it hangs at the wing, slides aft, and stalls in place,
                    // which is what draws the plume out into a streak behind a
                    // moving ship instead of dumping it all on one point.
                    vel: vel * 0.86 + drift,
                    drag: 0.8,
                    ..default()
                },
            );
        }

        for _ in 0..owed[1] {
            let j = FIRE_JITTER;
            let jitter = Vec3::new(
                fx.rng.range(-j, j),
                fx.rng.range(-j, j),
                fx.rng.range(-j, j),
            );
            let scale = fx.rng.range(0.5, 1.15);
            // Short-lived, so the flame stays *on* the wing while the smoke
            // trails away from it. The two lifetimes are what separates them.
            let life = fx.rng.range(0.13, 0.26);
            // Drawn before the push: `fx.rng` and `fx.damage` are sibling
            // fields, and the borrow checker sees one `&mut fx` through the
            // call rather than two disjoint borrows — the same shape
            // `age_effects` documents for the exhaust debt.
            let color = fx.rng.pick(&FIRE_COLORS);
            let flick = fx.rng.range(0.6, 1.0);
            let stream = back * fx.rng.range(4.0, 12.0) + out * fx.rng.range(0.0, 1.2);
            push_damage(
                &mut fx.damage,
                Mote {
                    pos: nozzle + jitter,
                    life,
                    half: scale * 0.5,
                    grow: 1.1,
                    shrink: 0.45,
                    color,
                    // Every flame ends the same deep red however it started, so
                    // the burning wing has a hot root and a dull tail rather
                    // than being one uniform orange smear.
                    cool: FIRE_EMBER,
                    opacity: 0.85 * flick,
                    // Nearly all of it: a flame is anchored to the airframe and
                    // only streams a little, which is what separates it from
                    // the smoke coming off the same point.
                    vel: vel * 0.95 + stream,
                    drag: 1.2,
                    smear: MOTE_SMEAR,
                    ..default()
                },
            );
        }
    }
}

/// How far below `at` a hull fraction has fallen, as `0..1`.
fn ramp(hull: f32, at: f32) -> f32 {
    ((at - hull) / at).clamp(0.0, 1.0)
}

/// Position jitter on a smoke puff, and on a flame. The flame is much tighter
/// because it is attached to a place on the airframe and the smoke is not.
///
/// The smoke's used to be much larger — 1.2 units — for a reason that no longer
/// holds: a particle did not move after release, so a *stationary* damaged ship
/// emitted every puff into the same cubic metre, and thirty additive quads
/// stacked on one point is not a plume but a small sun. Scattering them was the
/// only volume available. Now that a puff carries the ship's momentum and drifts
/// aft under drag, the plume gets its volume from the motion, and a jitter this
/// wide only made the source of the damage vague.
const SMOKE_JITTER: f32 = 0.45;
const FIRE_JITTER: f32 = 0.26;

/// Pushes a damage particle, dropping the oldest when its own cap is reached.
fn push_damage(damage: &mut Vec<Mote>, mote: Mote) {
    if damage.len() >= MAX_DAMAGE_MOTES {
        damage.remove(0);
    }
    damage.push(mote);
}

/// Pushes an EMP wavefront particle, dropping the oldest when its own cap is
/// reached.
fn push_pulse(pulse: &mut Vec<Mote>, mote: Mote) {
    if pulse.len() >= MAX_PULSE_MOTES {
        pulse.remove(0);
    }
    pulse.push(mote);
}

/// Pushes a particle, dropping the oldest when the global cap is reached.
///
/// `trails.js` does `list.shift()` on overflow, which is the same policy. The
/// cap is what keeps the per-frame vertex rebuild bounded no matter how many
/// ships are boosting.
fn push_mote(motes: &mut Vec<Mote>, mote: Mote) {
    if motes.len() >= MAX_MOTES {
        motes.remove(0);
    }
    motes.push(mote);
}

/// Pushes a shell, dropping the oldest when the cap is reached.
fn push_shell(shells: &mut Vec<Shell>, shell: Shell) {
    if shells.len() >= MAX_SHELLS {
        shells.remove(0);
    }
    shells.push(shell);
}

/// Pushes a beam, dropping the oldest when the cap is reached.
fn push_beam(beams: &mut Vec<BeamFx>, beam: BeamFx) {
    if beams.len() >= MAX_BEAMS {
        beams.remove(0);
    }
    beams.push(beam);
}

/// Ages every particle by `dt`, moves it, and drops it when its life runs out.
///
/// The integration is semi-implicit — drag first, then the step — because a
/// spark leaves an explosion at 90 units a second and an explicit step at a
/// 60 Hz frame would carry it a unit and a half before the drag it is supposed
/// to have felt is applied at all.
fn advance(motes: &mut Vec<Mote>, dt: f32) {
    for m in motes.iter_mut() {
        step(m, dt);
    }
    motes.retain(|m| m.age < m.life);
}

/// One particle, one step. Split out so the screenshot harness can wind a
/// single particle forward to a chosen point in its life without also ageing
/// the frame around it.
fn step(m: &mut Mote, dt: f32) {
    m.age += dt;
    if m.drag > 0.0 {
        m.vel /= 1.0 + m.drag * dt;
    }
    m.pos += m.vel * dt;
}

/// Ages every effect and drops the dead ones. Per *frame*, on real time, which
/// is what makes a 0.18 s beam fade smoothly on a 144 Hz display.
fn age_effects(time: Res<Time>, frame: Res<SimFrame>, mut fx: ResMut<Effects>) {
    let dt = time.delta_secs();

    for s in &mut fx.shells {
        s.age += dt;
    }
    fx.shells.retain(|s| s.age < s.life);

    for b in &mut fx.beams {
        b.age += dt;
    }
    fx.beams.retain(|b| b.age < BEAM_LIFE);

    advance(&mut fx.motes, dt);
    advance(&mut fx.damage, dt);
    advance(&mut fx.pulse, dt);

    // Missile exhaust. Emitted here rather than in `emit_trails` because the
    // 0.028 s interval is finer than a 16.7 ms tick and the missiles it hangs
    // off are read straight from the frame.
    let missiles: Vec<ProjView> = frame
        .0
        .missiles
        .iter()
        .copied()
        .chain(fx.demo.missiles.iter().copied())
        .collect();

    let live: Vec<u64> = missiles.iter().map(|m| m.key).collect();
    fx.exhaust_debt.retain(|k, _| live.contains(k));

    for m in &missiles {
        // The debt borrow has to end before the emit loop: `fx.rng` and
        // `fx.motes` are siblings of `fx.exhaust_debt`, and the borrow checker
        // sees one `&mut fx`, not three disjoint fields, through a map entry.
        let puffs = {
            let debt = fx.exhaust_debt.entry(m.key).or_insert(0.0);
            *debt += dt;
            let n = (*debt / MISSILE_EXHAUST_INTERVAL).floor();
            *debt -= n * MISSILE_EXHAUST_INTERVAL;
            n as u32
        };
        for i in 0..puffs {
            let dir = to_vec3(m.dir);
            let nozzle = to_vec3(m.pos) + dir * MISSILE_FLAME_Z;
            // Same sub-frame spread the engine trails use: a missile at
            // `missile_speed` covers a couple of units between puffs, and
            // stamping every puff a frame owes on one point beads the exhaust.
            let back = dt * (1.0 - (i as f32 + 0.5) / puffs as f32);
            let scale = fx.rng.range(0.45, 1.10);
            let life = fx.rng.range(0.30, 0.46);
            let speed = RULES.weapons.missile_speed as f32;
            push_mote(
                &mut fx.motes,
                Mote {
                    pos: nozzle - dir * speed * back,
                    age: back,
                    life,
                    half: scale * 0.5,
                    // `missiles.js`: `initScale * (1 + t * 2.8)`.
                    grow: 2.8,
                    shrink: 0.0,
                    // A rocket plume is white at the throat and soot-red by the
                    // time it is a body-length behind; one flat orange for the
                    // whole life is the flag that says "billboard".
                    color: glow(0xffd9a0, 2.6),
                    cool: glow(0xcc2200, 0.30),
                    opacity: 0.72,
                    // Most of the missile's own momentum, so the plume hangs at
                    // the nozzle rather than being abandoned a body-length back
                    // the instant it is born.
                    vel: dir * speed * 0.55,
                    drag: 2.4,
                    smear: MOTE_SMEAR,
                    ..default()
                },
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The mesh rebuild
// ---------------------------------------------------------------------------

/// Scratch vertex buffers, kept in a `Local` so the rebuild allocates only
/// until each vector reaches its high-water mark — the same trick
/// `Frame::clear` uses on the simulation side.
#[derive(Default)]
struct MeshBuild {
    pos: Vec<[f32; 3]>,
    normal: Vec<[f32; 3]>,
    uv: Vec<[f32; 2]>,
    color: Vec<[f32; 4]>,
    index: Vec<u32>,
}

impl MeshBuild {
    fn clear(&mut self) {
        self.pos.clear();
        self.normal.clear();
        self.uv.clear();
        self.color.clear();
        self.index.clear();
    }

    /// One quad, as two triangles. `ax`/`ay` are the *half*-extent vectors, so
    /// the corners are `center ± ax ± ay`; a screen-facing puff passes scaled
    /// camera right/up, and an aligned bolt passes its own direction and a
    /// camera-perpendicular side vector.
    ///
    /// `facing` is written into the normal attribute. Nothing reads it — the
    /// material is `unlit` — but the PBR vertex shader's layout expects the
    /// attribute to exist, and a plausible value costs the same as a zero one.
    fn quad(
        &mut self,
        center: Vec3,
        ax: Vec3,
        ay: Vec3,
        facing: Vec3,
        brush: Brush,
        color: LinearRgba,
        alpha: f32,
    ) {
        let base = self.pos.len() as u32;
        let c = [color.red, color.green, color.blue, alpha];
        let n = facing.to_array();

        // Half a texel of inset, so linear filtering at the cell seam cannot
        // reach into the neighbouring brush.
        let eps = 0.5 / (ATLAS_CELL * 2) as f32;
        let (u0, u1) = (brush.u0 + eps, brush.u1 - eps);

        for (corner, uv) in [
            (center - ax - ay, [u0, 1.0]),
            (center + ax - ay, [u1, 1.0]),
            (center + ax + ay, [u1, 0.0]),
            (center - ax + ay, [u0, 0.0]),
        ] {
            self.pos.push(corner.to_array());
            self.normal.push(n);
            self.uv.push(uv);
            self.color.push(c);
        }

        self.index
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    /// A screen-facing square of half-extent `r`.
    #[allow(clippy::too_many_arguments)]
    fn puff(
        &mut self,
        center: Vec3,
        r: f32,
        cam_right: Vec3,
        cam_up: Vec3,
        cam_fwd: Vec3,
        brush: Brush,
        color: LinearRgba,
        alpha: f32,
    ) {
        self.quad(
            center,
            cam_right * r,
            cam_up * r,
            -cam_fwd,
            brush,
            color,
            alpha,
        );
    }

    /// A quad aligned to a world-space segment and rolled to face the camera —
    /// the billboard a cylinder degrades to. Bolts, beams, and trail ribbons
    /// are all this. The missile body was, and is now real geometry instead.
    #[allow(clippy::too_many_arguments)]
    fn streak(
        &mut self,
        center: Vec3,
        along: Vec3,
        half_len: f32,
        half_width: f32,
        cam_fwd: Vec3,
        brush: Brush,
        color: LinearRgba,
        alpha: f32,
    ) {
        // The side vector has to be perpendicular to both the segment and the
        // view, or the quad turns edge-on and vanishes. When the segment points
        // straight at the camera the cross product degenerates, and any
        // perpendicular will do because the quad is a dot at that angle.
        let side = along.cross(cam_fwd);
        let side = if side.length_squared() > 1e-8 {
            side.normalize()
        } else {
            along.any_orthonormal_vector()
        };
        self.quad(
            center,
            along * half_len,
            side * half_width,
            -cam_fwd,
            brush,
            color,
            alpha,
        );
    }
}

/// Rewrites the whole effect mesh from this frame's state.
///
/// This is the function the module exists for. Note what it does *not* do:
/// spawn, despawn, or touch a single entity. The scene graph is static at one
/// entity no matter how much is in flight.
fn build_surface(
    mut meshes: ResMut<Assets<Mesh>>,
    assets: Res<EffectAssets>,
    fx: Res<Effects>,
    frame: Res<SimFrame>,
    fixed: Res<Time<Fixed>>,
    cameras: Query<&GlobalTransform, With<Camera3d>>,
    mut build: Local<MeshBuild>,
) {
    let Some(cam) = cameras.iter().next() else {
        return;
    };
    let Some(mut mesh) = meshes.get_mut(&assets.mesh) else {
        return;
    };

    let cam_right = cam.right().as_vec3();
    let cam_up = cam.up().as_vec3();
    let cam_fwd = cam.forward().as_vec3();

    // The viewer's own velocity, for the smear below.
    //
    // The chase camera rides the local ship, so the ship's velocity is the
    // camera's to within the damping — near enough for a streak length, and far
    // cheaper than differencing the camera transform between frames (which
    // would also smear the whole field for one frame after every respawn and
    // every teleport).
    let viewer_vel = frame
        .0
        .ships
        .iter()
        .find(|s| s.flags.contains(sim::world::ShipFlags::LOCAL))
        .map_or(Vec3::ZERO, |s| Vec3::from_array(s.vel));

    build.clear();

    // `SimFrame` is the last completed *tick*, and a bullet covers 13 units in
    // one at `bullet_speed`. Drawing it at the tick pose would staircase
    // badly, so each projectile is advanced along its own `dir` by however far
    // through the current tick the display is. This is the same correction
    // `scene.rs` applies to ships, in the one form a projectile allows: a bolt
    // travels in a straight line at a known speed, so the *next* pose can be
    // extrapolated rather than interpolated from two samples.
    let overstep = fixed.overstep_fraction();
    let bullet_lead = RULES.weapons.bullet_speed as f32 * sim::world::TICK_DT as f32 * overstep;
    let missile_lead = RULES.weapons.missile_speed as f32 * sim::world::TICK_DT as f32 * overstep;

    // The three palettes, resolved once for the whole frame rather than once
    // per bolt: `glow` is a `powf` per channel and there can be hundreds of
    // bolts. Indexing this array is the entire per-projectile cost of knowing
    // whose shot it is — no branch, no lookup, and not one extra draw call,
    // because the colour rides in the vertex buffer that is rebuilt anyway.
    let ink = bolt_palette();

    // ── bullets ──────────────────────────────────────────────────────────
    //
    // `bullets.js` draws a core cylinder inside a wider, dimmer halo cylinder,
    // both additive. Two quads per bolt, both in this buffer.
    for b in frame.0.bullets.iter().chain(fx.demo.bullets.iter()) {
        let dir = to_vec3(b.dir);
        let center = to_vec3(b.pos) + dir * bullet_lead;
        let ink = ink[b.allegiance as usize];
        build.streak(
            center,
            dir,
            BOLT_HALO_LEN * 0.5,
            BOLT_HALO_HALF_W,
            cam_fwd,
            Brush::GLOW,
            ink.halo,
            BOLT_HALO_ALPHA,
        );
        build.streak(
            center,
            dir,
            BOLT_LEN * 0.5,
            BOLT_CORE_HALF_W,
            cam_fwd,
            Brush::CORE,
            ink.core,
            0.95,
        );
    }

    // ── missiles ─────────────────────────────────────────────────────────
    //
    // Only the flame. The body is solid geometry on its own pooled entities —
    // see the module docs — and `place_missile_bodies` puts it here, using the
    // same `missile_lead` extrapolation so the two cannot separate.
    let t = frame.0.time as f32;
    for m in frame.0.missiles.iter().chain(fx.demo.missiles.iter()) {
        let dir = to_vec3(m.dir);
        let center = to_vec3(m.pos) + dir * missile_lead;
        // `missiles.js`: `pulse = 0.75 + 0.45 * |sin(age * 19)|`. Phase is
        // offset per missile off the key so a salvo does not throb in unison.
        let phase = (m.key % 997) as f32 * 0.0063;
        let pulse = 0.75 + 0.45 * (t * 19.0 + phase).sin().abs();
        build.puff(
            center + dir * MISSILE_FLAME_Z,
            0.55 * pulse,
            cam_right,
            cam_up,
            cam_fwd,
            Brush::GLOW,
            MISSILE_NOZZLE,
            0.70 + 0.25 * pulse,
        );
        // Whose rocket it is, in the one place a rocket can say so without
        // giving up its shared mesh. See `MISSILE_HALO_ALPHA`.
        build.streak(
            center,
            dir,
            MISSILE_BODY_LEN * 0.75,
            MISSILE_HALO_HALF_W,
            cam_fwd,
            Brush::GLOW,
            ink[m.allegiance as usize].halo,
            MISSILE_HALO_ALPHA,
        );
    }

    // ── flares ───────────────────────────────────────────────────────────
    //
    // `missiles.js` gives each decoy a white core and a yellow glow, both
    // flickering on their own frequency and both shrinking as they burn out.
    for f in frame.0.flares.iter().chain(fx.demo.flares.iter()) {
        let p = to_vec3(f.pos);
        // `tLife = 1 - life / FLARE_LIFE`: 0 at release, 1 at burnout.
        let t_life = 1.0 - (f.life / RULES.weapons.flare_life as f32).clamp(0.0, 1.0);
        let phase = (f.key % 1009) as f32 * 1.7;

        let c_flick = 0.70 + 0.30 * (f.age * 34.0 + phase).sin().abs();
        let g_flick = 0.55 + 0.45 * (f.age * 19.0 + phase * 1.8).sin().abs();

        let glow_r = (2.2 + (0.8 - 2.2) * t_life) * (0.9 + 0.1 * g_flick);
        build.puff(
            p,
            FLARE_GLOW_R * glow_r.max(0.1),
            cam_right,
            cam_up,
            cam_fwd,
            Brush::GLOW,
            FLARE_GLOW,
            (1.0 - t_life * 0.75) * g_flick * 0.70,
        );

        let core_r = (1.4 + (0.20 - 1.4) * t_life) * c_flick;
        build.puff(
            p,
            FLARE_CORE_R * core_r.max(0.05),
            cam_right,
            cam_up,
            cam_fwd,
            Brush::CORE,
            FLARE_CORE,
            ((1.0 - t_life * 0.45) * c_flick).min(1.0),
        );
    }

    // ── beams ────────────────────────────────────────────────────────────
    //
    // Core inside halo, the same two-quad idiom as a bolt. One quad on its own
    // is a flat ribbon of colour with a hard edge; the wider, dimmer pass under
    // it is what gives the beam a glow to sit in, and it is what `beams.js`'s
    // lit cylinder gets for free from its own shading.
    for b in &fx.beams {
        let seg = b.end - b.start;
        let len = seg.length();
        if len < 1e-3 {
            continue;
        }
        let fade = 1.0 - b.age / BEAM_LIFE;
        // The bolt fades by *thinning* as well as dimming, which reads as the
        // channel closing rather than as the image being turned down.
        let taper = 0.35 + 0.65 * fade;
        build.streak(
            b.start + seg * 0.5,
            seg / len,
            len * 0.5,
            BEAM_HALF_W * 3.2 * taper,
            cam_fwd,
            Brush::GLOW,
            b.color,
            fade * BEAM_OPACITY * 0.45,
        );
        build.streak(
            b.start + seg * 0.5,
            seg / len,
            len * 0.5,
            BEAM_HALF_W * taper,
            cam_fwd,
            Brush::CORE,
            BEAM_CORE,
            fade * BEAM_OPACITY,
        );
    }

    // ── explosion shells ─────────────────────────────────────────────────
    for s in &fx.shells {
        let t = (s.age / s.life).clamp(0.0, 1.0);
        // Fast then slow, and hot then cold. See `Shell::ease` and `Shell::cool`
        // for why either matters; between them they are most of the difference
        // between a fireball and an inflating orange balloon.
        let r = s.from + (s.to - s.from) * t.powf(s.ease);
        build.puff(
            s.pos,
            r,
            cam_right,
            cam_up,
            cam_fwd,
            Brush::GLOW,
            mix(s.color, s.cool, t),
            (1.0 - t).powf(s.fade) * s.opacity,
        );
    }

    // ── trail, exhaust, damage and pulse motes ───────────────────────────
    //
    // Damage first, so a fresh flame draws over the smoke it came out of rather
    // than under it. All three lists are the same record and the same quad;
    // they are separate only so their caps are.
    for m in fx
        .damage
        .iter()
        .chain(fx.motes.iter())
        .chain(fx.pulse.iter())
    {
        let t = (m.age / m.life).clamp(0.0, 1.0);
        let r = m.half * (1.0 + t * m.grow) * (1.0 - t * m.shrink);
        let color = mix(m.color, m.cool, t);
        let alpha = (1.0 - t) * m.opacity;

        // A particle moving several of its own diameters per frame is a bead on
        // a string however many of them there are, so it is drawn smeared along
        // its motion instead — see `MOTE_SMEAR`. The threshold is what keeps a
        // slow puff a puff: below it the streak would be shorter than the quad
        // is wide and the branch would only cost a normalise.
        //
        // **Relative to the viewer, not to the world.** Smearing is a stand-in
        // for the streak a fast-crossing particle would leave on a real sensor,
        // and "fast" there means fast *across the view*. Exhaust inherits the
        // ship's lateral velocity whole, so while drifting it hangs at the
        // nozzle and is very nearly stationary to the pilot looking at it —
        // while its world velocity is the better part of 80 u/s. Keyed off that
        // number, every plume stretched into a line the moment the ship stopped
        // flying straight, for motion the viewer could not see. Flying straight
        // hid it: there the mote sheds 30% along the nose, and world and
        // relative speed are close enough to look right by accident.
        let motion = m.vel - viewer_vel;
        let speed = motion.length();
        let smear = speed * m.smear;
        if smear > r * 0.4 {
            build.streak(
                m.pos,
                motion / speed,
                r + smear,
                r,
                cam_fwd,
                m.brush,
                color,
                alpha,
            );
        } else {
            build.puff(m.pos, r, cam_right, cam_up, cam_fwd, m.brush, color, alpha);
        }
    }

    // An empty vertex buffer is a zero-byte allocation, which wgpu rejects.
    // Pad to a fixed size. This is not tidiness -- a mesh whose vertex count
    // changes between frames makes Bevy's slab allocator free and reallocate
    // its GPU entry, and the render world's copy step then references the key
    // that just went away:
    //
    //     ERROR bevy_render::slab_allocator: Use-after-free: attempted to copy
    //     element data for an unallocated key
    //
    // That fired twice a frame -- once here, once in `warp.rs` -- roughly 166
    // times a second, for the whole session.
    //
    // Padding with degenerate triangles keeps the allocation stable: they are
    // zero-area so the rasteriser discards them, and zero-alpha so an additive
    // blend contributes nothing even if one survived.
    //
    // Overflow is *dropped whole quads*, not truncated buffers. `resize` down
    // would cut the vertices and the indices independently, and an index left
    // pointing past the shortened vertex array is a read out of bounds on the
    // GPU — a much worse failure than the missing effects, and one that only
    // appears in the frame that overruns. `debug_assert` catches it in a test
    // run; this catches it in a shipped one.
    let cap_verts = MESH_VERTEX_CAPACITY;
    let cap_indices = MESH_INDEX_CAPACITY;
    debug_assert!(build.pos.len() <= cap_verts, "vertex budget exceeded");
    debug_assert!(build.index.len() <= cap_indices, "index budget exceeded");
    if build.pos.len() > cap_verts {
        warn_once!("effects mesh overran its {MESH_QUAD_CAPACITY}-quad budget");
        build.pos.truncate(cap_verts);
        build.normal.truncate(cap_verts);
        build.uv.truncate(cap_verts);
        build.color.truncate(cap_verts);
        // Six indices to a quad, four vertices to a quad: keep only the
        // triangles whose vertices all survived.
        build.index.truncate(cap_verts / 4 * 6);
    }
    build.pos.resize(cap_verts, [0.0; 3]);
    build.normal.resize(cap_verts, [0.0, 0.0, 1.0]);
    build.uv.resize(cap_verts, [0.0; 2]);
    build.color.resize(cap_verts, [0.0; 4]);
    build.index.resize(cap_indices, 0);

    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, build.pos.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, build.normal.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, build.uv.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, build.color.clone());
    mesh.insert_indices(Indices::U32(build.index.clone()));
}

// ── the palette, resolved once ───────────────────────────────────────────

/// One shot's two colours: the near-white core and the wide, saturated halo
/// that carries the hue.
///
/// The core is deliberately *not* the identifying colour. A bolt's core sits
/// well over the bloom threshold and blows out to white on screen whatever it
/// started as, so the halo is the only part of a laser a player can actually
/// read a hue off — which is why the three halos below are strongly separated
/// and the three cores barely differ.
#[derive(Clone, Copy)]
struct BoltInk {
    core: LinearRgba,
    halo: LinearRgba,
}

/// `bullets.js`'s `mkLaserMats` pairs, in [`Allegiance`] order: `localMats`,
/// `allyMats`, `enemyMats` (`bullets.js:26`–`:28`).
///
/// Green is yours, blue is your team's, red is coming at you — the convention
/// the shipped game already trained its players on, kept exactly.
const BOLT_HEX: [(u32, u32); 3] = [
    (0xeaffe6, 0x44ffb0),
    (0xe6f0ff, 0x4aa3ff),
    (0xffe0e0, 0xff3030),
];

/// How far over the bloom threshold each half of a bolt sits.
///
/// The core is unchanged from what this module shipped: `glow(0xeaffe6, 4.4)`
/// is exactly the white-hot channel it always drew. The halo is brighter than
/// the 1.5 it was, for the reason in [`BOLT_HALO_HALF_W`] — it now has to carry
/// a hue through an ACES grade rather than just soften an edge.
const BOLT_CORE_GLOW: f32 = 4.4;
const BOLT_HALO_GLOW: f32 = 2.4;

/// The palette for one allegiance.
///
/// `glow` costs three `powf`s a channel, so this is called once per *frame* per
/// palette (hoisted in [`build_surface`]) and once per *event* for the beams and
/// muzzle flashes, never once per bolt.
fn bolt_ink(allegiance: Allegiance) -> BoltInk {
    let (core, halo) = BOLT_HEX[allegiance as usize];
    BoltInk {
        core: glow(core, BOLT_CORE_GLOW),
        halo: glow(halo, BOLT_HALO_GLOW),
    }
}

/// Every palette at once, for the per-frame loops.
fn bolt_palette() -> [BoltInk; 3] {
    [
        bolt_ink(Allegiance::Own),
        bolt_ink(Allegiance::Ally),
        bolt_ink(Allegiance::Hostile),
    ]
}

/// `missiles.js` nozzle glow `0xff9900`. Not tinted by allegiance: a rocket
/// motor is a flame, and a green or red one reads as a bug rather than as a
/// friend. The rocket says whose it is with the halo around its body instead —
/// see [`MISSILE_HALO_ALPHA`].
static MISSILE_NOZZLE: LinearRgba = LinearRgba::rgb(5.10, 1.30, 0.0);

/// How bright the allegiance halo around a missile body is.
///
/// The body itself is three flat greys ([`missile_mesh`]) and stays that way:
/// it is the one solid object in this module and repainting it team colours
/// would cost the shared-mesh batch that makes it free. A soft streak of the
/// shooter's own hue *around* it costs one quad in the buffer that was already
/// being rebuilt, and it is what makes an incoming rocket readable at the range
/// where the body is four pixels long.
const MISSILE_HALO_ALPHA: f32 = 0.5;
/// Half-width of that halo, comfortably wider than [`MISSILE_BODY_RAD`] so it
/// reads as a glow around the body rather than as a stripe on it.
const MISSILE_HALO_HALF_W: f32 = 1.15;
/// `missiles.js` flare core `0xffffff`.
static FLARE_CORE: LinearRgba = LinearRgba::rgb(5.10, 5.10, 5.10);
/// `missiles.js` flare glow `0xffcc22`.
static FLARE_GLOW: LinearRgba = LinearRgba::rgb(3.40, 2.02, 0.11);

/// The beam's own core, hotter and whiter than the halo around it so the two
/// passes read as one lit channel rather than as two ribbons.
static BEAM_CORE: LinearRgba = LinearRgba::rgb(4.20, 6.00, 5.20);

/// Damage smoke at birth and at death, both held **under** `camera.rs`'s 0.9
/// bloom prefilter threshold on every channel so neither glows. See the note
/// above [`SMOKE_AT`] on why they cannot simply be dark.
///
/// Two colours rather than one because additive blending gives this effect
/// exactly one lever and this is it: a puff leaving a burning wing is lit by the
/// fire and a puff a second downstream is not, so the plume has a warm root and
/// a cold tail. Held one flat grey — which is what it was — a plume has no depth
/// cue at all and reads as a smudge on the lens.
static SMOKE_HOT: LinearRgba = LinearRgba::rgb(0.42, 0.28, 0.18);
static SMOKE_COLD: LinearRgba = LinearRgba::rgb(0.10, 0.11, 0.14);

/// Fire, well over the bloom threshold and picked from per flame so a burning
/// wing flickers between yellow and deep orange instead of pulsing as one.
static FIRE_COLORS: [LinearRgba; 3] = [
    LinearRgba::rgb(5.60, 2.30, 0.30),
    LinearRgba::rgb(4.40, 1.20, 0.12),
    LinearRgba::rgb(6.00, 3.40, 0.70),
];

/// What every flame cools to. Under the bloom threshold on purpose: the tail of
/// a flame is soot, not light.
static FIRE_EMBER: LinearRgba = LinearRgba::rgb(0.55, 0.06, 0.02);

// ---------------------------------------------------------------------------
// The stress harness
// ---------------------------------------------------------------------------

/// Synthetic projectiles, for measuring against a busy scene.
///
/// This was written when `sim_bridge::tick` ran no weapons at all and
/// `Frame::bullets` was therefore always empty. It does now — the bridge calls
/// `sim::tick::tick`, and a held trigger puts real bolts in the frame — so the
/// harness is no longer the *only* way to see a projectile. It is still the way
/// to see three hundred at once, which is the question it was built for: how a
/// scene far busier than a real match costs, against the draw-call budget the
/// module header sets out.
///
/// Rather than fake data inside the simulation — which would put render
/// scaffolding on the wrong side of the boundary — these records are
/// synthesised here and *chained* onto the frame's own slices at every read
/// site. Nothing else in the module knows it exists.
#[derive(Default)]
struct Demo {
    bullets: Vec<ProjView>,
    missiles: Vec<ProjView>,
    flares: Vec<FlareView>,
    /// How many bullets to hold in flight. Zero disables the whole harness.
    count: usize,
    started: bool,
}

/// The allegiances [`run_demo`] cycles its synthetic projectiles through.
const DEMO_SIDES: [Allegiance; 3] = [Allegiance::Own, Allegiance::Ally, Allegiance::Hostile];

fn run_demo(time: Res<Time>, frame: Res<SimFrame>, mut fx: ResMut<Effects>) {
    if !fx.demo.started {
        fx.demo.started = true;
        fx.demo.count = std::env::var("SPACESHIPS_FX_DEMO")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if fx.demo.count > 0 {
            info!("SPACESHIPS_FX_DEMO: {} synthetic bullets", fx.demo.count);
        }
    }
    if fx.demo.count == 0 {
        return;
    }

    let n = fx.demo.count;
    let t = time.elapsed_secs();
    let dt = time.delta_secs();

    // Anchor on the player so the swarm is always in shot.
    let anchor = frame
        .0
        .ships
        .first()
        .map_or(Vec3::ZERO, |s| to_vec3(s.pos) + to_vec3(s.vel) * 0.25);

    // Bullets: a slow torus of bolts around the anchor, each pointing along its
    // own tangent, cycling so the buffer churns the way a real firefight does.
    fx.demo.bullets.clear();
    for i in 0..n {
        let f = i as f32 / n as f32;
        let a = f * std::f32::consts::TAU * 7.0 + t * 0.6;
        let ring = 60.0 + 90.0 * (f * 11.0).sin();
        let dir = Vec3::new(-a.sin(), 0.12 * (a * 3.0).cos(), a.cos()).normalize();
        fx.demo.bullets.push(ProjView {
            key: i as u64,
            pos: (anchor
                + Vec3::new(
                    a.cos() * ring,
                    24.0 * (f * 7.0 + t * 0.4).sin(),
                    a.sin() * ring,
                ))
            .to_array(),
            dir: dir.to_array(),
            // Round-robin, so the stress scene exercises all three palettes and
            // a screenshot of it shows the cost of the busiest case.
            allegiance: DEMO_SIDES[i % DEMO_SIDES.len()],
        });
    }

    // Missiles: a tenth as many, so the exhaust emitter is exercised too.
    let missiles = (n / 10).max(4);
    fx.demo.missiles.clear();
    for i in 0..missiles {
        let f = i as f32 / missiles as f32;
        let a = f * std::f32::consts::TAU + t * 0.35;
        let dir = Vec3::new(-a.sin(), 0.0, a.cos());
        fx.demo.missiles.push(ProjView {
            key: 1_000_000 + i as u64,
            pos: (anchor + Vec3::new(a.cos() * 110.0, -18.0 + 36.0 * f, a.sin() * 110.0))
                .to_array(),
            dir: dir.to_array(),
            allegiance: DEMO_SIDES[i % DEMO_SIDES.len()],
        });
    }

    // Flares: a handful of decoys, cycling through their burn so the flicker
    // and the fade are both on screen at once.
    let flares = (n / 12).max(3);
    fx.demo.flares.clear();
    let life = RULES.weapons.flare_life as f32;
    for i in 0..flares {
        let f = i as f32 / flares as f32;
        let age = (t * 0.7 + f * life) % life;
        let a = f * std::f32::consts::TAU * 3.0;
        fx.demo.flares.push(FlareView {
            key: 2_000_000 + i as u64,
            pos: (anchor + Vec3::new(a.cos() * 55.0, 12.0 * (a * 2.0).sin(), a.sin() * 55.0))
                .to_array(),
            age,
            life: life - age,
        });
    }

    // Explosions and beams, on a timer, so every branch of `consume_events` is
    // represented without needing the simulation to produce one.
    let mut fire = ((t / 0.22) as u32) != (((t - dt) / 0.22) as u32);
    if dt <= 0.0 {
        fire = false;
    }
    if fire {
        let a = t * 1.9;
        let p = anchor + Vec3::new(a.cos() * 70.0, 10.0 * (a * 0.7).sin(), a.sin() * 70.0);
        let kinds = [
            ExplosionKind::Impact,
            ExplosionKind::MissileHit,
            ExplosionKind::AsteroidBreak,
            ExplosionKind::FlareBurst,
            ExplosionKind::ShipDeath,
        ];
        let kind = kinds[((t / 0.22) as usize) % kinds.len()];
        let scale = match kind {
            ExplosionKind::ShipDeath => 6.0,
            ExplosionKind::AsteroidBreak => 14.0,
            _ => 1.0,
        };
        spawn_explosion(&mut fx, kind, p, scale);

        push_beam(
            &mut fx.beams,
            BeamFx {
                start: anchor + Vec3::new(a.sin() * 12.0, -4.0, a.cos() * 12.0),
                end: p,
                age: 0.0,
                color: bolt_ink(DEMO_SIDES[((t / 0.22) as usize) % DEMO_SIDES.len()]).halo,
            },
        );
    }
}

// ---------------------------------------------------------------------------
// The screenshot harness
// ---------------------------------------------------------------------------

/// `SPACESHIPS_FX_SCENE=impact@0.35`: holds one effect, at one age, in shot.
///
/// [`Demo`] above answers "what does a busy frame cost"; this answers "what does
/// *this* effect look like", which is a different question and the one a
/// before/after pair needs. An explosion lives for half a second, so catching
/// the same moment of the same effect twice by hand is not a thing that happens
/// — and a comparison of two different moments says nothing about the change.
///
/// So the scene is **restaged every frame**: the shell list is cleared, the
/// named effect is pushed, and every shell in it is aged to a fixed fraction of
/// its life. The result is a still image that does not move, which is exactly
/// what a screenshot wants.
///
/// Names are [`ExplosionKind`]'s, plus `muzzle` and `beam` for the two effects
/// that are not explosions, and `rocket` for the missile body. `@t` sets the
/// life fraction and defaults to 0.35 — far enough in for a shell to have
/// expanded, early enough that it has not faded out.
#[derive(Clone, Copy)]
struct Scene {
    what: SceneKind,
    at: f32,
}

#[derive(Clone, Copy, PartialEq)]
enum SceneKind {
    Explosion(ExplosionKind),
    Muzzle,
    Beam,
    /// A flight of missile bodies, held still. See [`stage_rockets`].
    Rocket,
    /// One volley of each allegiance, side by side. See [`stage_allegiance`].
    Allegiance,
    /// An EMP wavefront, staged **far** enough out to be seen from outside.
    ///
    /// Its own variant rather than an [`ExplosionKind`] because it is not one,
    /// and because it is the only effect here whose radius is larger than the
    /// distance every other scene is staged at: at [`SCENE_AHEAD`] a
    /// three-hundred-unit front swallows the camera on the first frame, which is
    /// the exact failure that made this effect a front of particles instead of a
    /// billboard in the first place. `emp@0.3` stages it two radii out.
    Emp,
}

fn fx_scene() -> Option<Scene> {
    #[cfg(target_arch = "wasm32")]
    return None;

    #[cfg(not(target_arch = "wasm32"))]
    {
        static SCENE: std::sync::OnceLock<Option<Scene>> = std::sync::OnceLock::new();
        *SCENE.get_or_init(|| {
            let raw = std::env::var("SPACESHIPS_FX_SCENE").ok()?;
            let (name, arg) = match raw.split_once('@') {
                Some((n, t)) => (n, t.trim().parse::<f32>().ok()),
                None => (raw.as_str(), None),
            };
            let what = match name.trim().to_ascii_lowercase().as_str() {
                "impact" => SceneKind::Explosion(ExplosionKind::Impact),
                "death" | "shipdeath" => SceneKind::Explosion(ExplosionKind::ShipDeath),
                "asteroid" => SceneKind::Explosion(ExplosionKind::AsteroidBreak),
                "missile" => SceneKind::Explosion(ExplosionKind::MissileHit),
                "flare" => SceneKind::Explosion(ExplosionKind::FlareBurst),
                "muzzle" => SceneKind::Muzzle,
                "beam" => SceneKind::Beam,
                "rocket" | "body" => SceneKind::Rocket,
                "allegiance" | "sides" => SceneKind::Allegiance,
                "emp" | "pulse" => SceneKind::Emp,
                other => {
                    warn!("SPACESHIPS_FX_SCENE={other} is not an effect this module draws");
                    return None;
                }
            };
            let at = match what {
                // For a rocket the argument is a **distance in units**, not a
                // life fraction: a missile body does not age, and the question a
                // still of one has to answer is how it reads close up against
                // how it reads at the range it is actually seen from. Floored
                // clear of the camera's own near plane and of the hull.
                //
                // `allegiance` is the same: three volleys do not age either, and
                // the question is whether the sides stay apart at range.
                SceneKind::Rocket | SceneKind::Allegiance => arg.unwrap_or(SCENE_AHEAD).max(6.0),
                _ => arg.unwrap_or(0.35).clamp(0.0, 0.99),
            };
            Some(Scene { what, at })
        })
    }
}

/// How far ahead of the ship the staged effect sits, and how far above it.
///
/// Far enough that the hull does not cover it, near enough that a 1-unit impact
/// spark is still more than a pixel. The lift clears the nose so the chase
/// camera, which looks slightly down, does not put the effect behind the ship.
const SCENE_AHEAD: f32 = 34.0;
const SCENE_LIFT: f32 = 2.0;

fn stage_scene(frame: Res<SimFrame>, mut fx: ResMut<Effects>) {
    let Some(scene) = fx_scene() else {
        return;
    };
    let Some(ship) = frame.0.ships.iter().find(|s| s.id == crate::LOCAL_ID) else {
        return;
    };

    let quat = rot(ship.quat);
    let fwd = quat * Vec3::Z;
    // `Rocket` spends its argument on range rather than on age, so it is also
    // the one scene that decides how far out it is staged.
    let ahead = match scene.what {
        SceneKind::Rocket | SceneKind::Allegiance => scene.at,
        // Two radii out, so the whole front is in shot and the camera is
        // outside it.
        SceneKind::Emp => EMP_SCENE_RADIUS * 2.0,
        _ => SCENE_AHEAD,
    };
    let at = to_vec3(ship.pos) + fwd * ahead + quat * Vec3::Y * SCENE_LIFT;

    // Every pool the staged effect writes into, including the shared mote pool
    // the sparks come out of. Without the last one a scene restaged 100 times a
    // second buries itself in its own fragments.
    fx.shells.clear();
    fx.beams.clear();
    fx.motes.clear();
    fx.pulse.clear();

    match scene.what {
        SceneKind::Emp => spawn_emp(&mut fx, at, EMP_SCENE_RADIUS),
        SceneKind::Allegiance => stage_allegiance(&mut fx, at, quat, scene.at),
        SceneKind::Explosion(kind) => {
            let scale = match kind {
                ExplosionKind::ShipDeath => 6.0,
                ExplosionKind::AsteroidBreak => 14.0,
                _ => 1.0,
            };
            spawn_explosion(&mut fx, kind, at, scale);
        }
        SceneKind::Muzzle => muzzle_flash(&mut fx, at, fwd, Allegiance::Own),
        SceneKind::Beam => fx.beams.push(BeamFx {
            start: at - fwd * 30.0 + quat * Vec3::X * 3.0,
            end: at + fwd * 30.0,
            age: BEAM_LIFE * scene.at,
            color: bolt_ink(Allegiance::Own).halo,
        }),
        SceneKind::Rocket => stage_rockets(&mut fx, at, quat, scene.at),
    }

    // Hold everything the stage just pushed at one moment of its life, so the
    // frame is reproducible rather than whatever the clock happened to be.
    for s in &mut fx.shells {
        s.age = s.life * scene.at;
    }
    // A particle has to be *flown* to that moment rather than have its age
    // written, or every spark would sit on the detonation point. Substepped
    // because the drag makes the path curved and a single jump of a third of a
    // second would land in the wrong place.
    // Two separate loops, not one chained borrow: `Effects` is behind a
    // `ResMut`, so `fx.motes.iter_mut().chain(fx.pulse.iter_mut())` reborrows
    // the whole resource twice and the checker refuses it — the same disjoint
    // field problem `emit_damage` notes about `fx.rng` and `fx.damage`.
    let hold = |m: &mut Mote| {
        let dt = m.life * scene.at / SCENE_SUBSTEPS as f32;
        for _ in 0..SCENE_SUBSTEPS {
            step(m, dt);
        }
    };
    fx.motes.iter_mut().for_each(hold);
    fx.pulse.iter_mut().for_each(hold);
}

/// The radius the `emp` scene stages, which is
/// [`sim::rules::EmpRules::radius`] rather than a number picked here — a
/// wavefront photographed at the wrong size is a photograph of nothing.
const EMP_SCENE_RADIUS: f32 = sim::rules::Rules::DEFAULT.emp.radius as f32;

/// How finely the harness integrates a staged particle to its held age. Sixty
/// is a frame-rate's worth, which is the accuracy the live effect gets.
const SCENE_SUBSTEPS: u32 = 60;

/// `SPACESHIPS_FX_SCENE=rocket@12`: three missile bodies parked in front of the
/// camera.
///
/// The body is the one thing in this module that a *live* frame is bad at
/// showing. A missile is in the air for [`sim::rules::WeaponRules::missile_life`]
/// seconds at 160 units a second, it is fired away from the camera, and it is
/// gone before the screenshot timer in `main.rs` reaches three seconds. So it
/// gets the same treatment every other effect here already has: held still, at a
/// chosen range, so a before-and-after pair is a pair of the same thing.
///
/// Six of them, not one, in two ranks at three attitudes each.
///
/// The attitudes are because a body only ever photographed abeam says nothing
/// about the angles a player actually sees it from, which are all of them:
/// broadside for the silhouette, quartering away for the nose, quartering back
/// and pitched up for the fin cross. The second rank is because "does it read as
/// a rocket" and "does it still read at the range it is normally seen" are two
/// questions and a still that answers only the first is the one that gets
/// shipped. Six also puts a squadron's worth of bodies on screen at once, which
/// is the case the shared mesh exists for.
///
/// They are pushed into [`Demo`]'s missile list rather than into the frame,
/// because that list is already chained onto `Frame::missiles` at every read
/// site — the body placement, the nozzle glow, and the exhaust emitter all pick
/// them up with no knowledge that a harness exists. Offsets are in **ship**
/// space and scale with the rank's own range, so the flight fills the same part
/// of the frame at 12 units as at 90.
fn stage_rockets(fx: &mut Effects, at: Vec3, quat: Quat, dist: f32) {
    let right = quat * Vec3::X;
    let up = quat * Vec3::Y;
    let fwd = quat * Vec3::Z;
    let ship_space = |v: Vec3| right * v.x + up * v.y + fwd * v.z;

    /// `(offset, facing)`, both in ship space, the offset in units of range.
    const ATTITUDES: [(Vec3, Vec3); 3] = [
        // Dead abeam: the full silhouette, nose to nozzle.
        (Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0)),
        // Above and outboard, quartering away from the camera.
        (Vec3::new(0.42, 0.20, -0.15), Vec3::new(0.50, 0.0, 0.87)),
        // Below and inboard, quartering back toward it and pitched up.
        (Vec3::new(-0.40, -0.16, 0.08), Vec3::new(0.55, 0.42, -0.72)),
    ];
    /// The two ranks, as multiples of the staged range.
    const RANKS: [f32; 2] = [1.0, 2.6];

    fx.demo.missiles.clear();
    for (r, rank) in RANKS.into_iter().enumerate() {
        // `at` is already one range ahead, so the far rank only adds the
        // difference.
        let anchor = at + fwd * dist * (rank - 1.0);
        for (i, (offset, facing)) in ATTITUDES.into_iter().enumerate() {
            fx.demo.missiles.push(ProjView {
                key: 3_000_000 + (r * ATTITUDES.len() + i) as u64,
                pos: (anchor + ship_space(offset) * dist * rank).to_array(),
                dir: ship_space(facing).normalize().to_array(),
                allegiance: Allegiance::Own,
            });
        }
    }
}

/// `SPACESHIPS_FX_SCENE=allegiance@40`: three volleys, one per side, held still.
///
/// The question a still of this has to answer is not "does a bolt look good" —
/// [`Demo`] and the live game both answer that — but **"can you tell them apart
/// without thinking about it"**, and that is a question about three things next
/// to each other. One volley of each allegiance is parked abeam at the staged
/// range, each a short stream of bolts with a rocket flying alongside it, so a
/// single frame carries every surface the palette touches: bolt core, bolt halo,
/// missile halo, and the grey body they have to stay legible against.
///
/// They fly *across* the camera rather than away from it, because a bolt seen
/// end-on is a dot and a player almost never sees one that way — the shot you
/// have to identify in a real fight is the one crossing your view.
fn stage_allegiance(fx: &mut Effects, at: Vec3, quat: Quat, dist: f32) {
    /// Bolts in each volley, and the gap between them along the line of flight.
    const PER_VOLLEY: usize = 7;
    const SPACING: f32 = 9.0;
    /// Vertical separation between the three volleys, in units of range, so the
    /// layout holds its proportions at 12 units and at 90.
    const LANE: f32 = 0.30;

    let right = quat * Vec3::X;
    let up = quat * Vec3::Y;

    fx.demo.bullets.clear();
    fx.demo.missiles.clear();
    for (lane, side) in DEMO_SIDES.into_iter().enumerate() {
        // Top lane is yours, bottom is theirs, so the reading order matches the
        // one the HUD already uses.
        let height = (1.0 - lane as f32) * LANE * dist;
        let origin = at + up * height - right * (PER_VOLLEY as f32 * SPACING * 0.5);
        for i in 0..PER_VOLLEY {
            fx.demo.bullets.push(ProjView {
                key: 4_000_000 + (lane * PER_VOLLEY + i) as u64,
                pos: (origin + right * (i as f32 * SPACING)).to_array(),
                dir: right.to_array(),
                allegiance: side,
            });
        }
        fx.demo.missiles.push(ProjView {
            key: 5_000_000 + lane as u64,
            pos: (origin + right * (PER_VOLLEY as f32 * SPACING) - up * dist * 0.09).to_array(),
            dir: right.to_array(),
            allegiance: side,
        });
    }
}

// ---------------------------------------------------------------------------
// Draw-call probe
// ---------------------------------------------------------------------------

/// Installs [`report_draw_calls`] into the render world, under
/// `SPACESHIPS_DRAWCALLS=1`.
///
/// It runs *after* `PrepareResourcesBatchPhases`, which is the set that merges
/// phase items into batches. Counting before it would report entities, not draw
/// calls, and those are the two numbers this module exists to keep apart.
fn install_draw_call_probe(app: &mut App) {
    use bevy::render::{Render, RenderApp, RenderSystems};

    let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
        warn!("SPACESHIPS_DRAWCALLS set but there is no render world");
        return;
    };
    render_app.add_systems(
        Render,
        report_draw_calls.after(RenderSystems::PrepareResourcesBatchPhases),
    );
}

/// Counts the batched draw calls in the three main 3D phases, once a second.
///
/// What the numbers are:
///
/// - **opaque / alpha-mask** are binned phases. `multidrawable_meshes` is one
///   multi-draw-indirect call per batch set; `batchable_meshes` is one draw per
///   bin; `unbatchable_meshes` is one draw per entity. The asteroid field lands
///   in the first two — sixty rocks over six meshes and one material — which is
///   the case `main.rs` describes the scene as being built for.
/// - **transparent** is a sorted phase, where batching merges neighbours by
///   giving the leader the whole instance range and the followers an empty one.
///   Draw calls are therefore the items with a non-empty `batch_range`. Every
///   projectile and effect in this module is in here, in one item.
///
/// Shadow and prepass phases are not counted; they are a separate cost and this
/// module contributes nothing to either (`NotShadowCaster`, and a transparent
/// material is not in the prepass).
fn report_draw_calls(
    opaque: Res<
        bevy::render::render_phase::ViewBinnedRenderPhases<bevy::core_pipeline::core_3d::Opaque3d>,
    >,
    alpha_mask: Res<
        bevy::render::render_phase::ViewBinnedRenderPhases<
            bevy::core_pipeline::core_3d::AlphaMask3d,
        >,
    >,
    transparent: Res<
        bevy::render::render_phase::ViewSortedRenderPhases<
            bevy::core_pipeline::core_3d::Transparent3d,
        >,
    >,
    mut last: Local<f64>,
    time: Res<Time>,
) {
    let now = time.elapsed_secs_f64();
    if now - *last < 1.0 {
        return;
    }
    *last = now;

    let mut opaque_calls = 0usize;
    for phase in opaque.values() {
        opaque_calls += binned_draw_calls(
            phase.multidrawable_meshes.len(),
            phase.batchable_meshes.len(),
            phase
                .unbatchable_meshes
                .values()
                .map(|u| u.entities.len())
                .sum(),
            phase
                .non_mesh_items
                .values()
                .map(|u| u.entities.len())
                .sum(),
        );
    }

    let mut mask_calls = 0usize;
    for phase in alpha_mask.values() {
        mask_calls += binned_draw_calls(
            phase.multidrawable_meshes.len(),
            phase.batchable_meshes.len(),
            phase
                .unbatchable_meshes
                .values()
                .map(|u| u.entities.len())
                .sum(),
            phase
                .non_mesh_items
                .values()
                .map(|u| u.entities.len())
                .sum(),
        );
    }

    let mut transparent_calls = 0usize;
    let mut transparent_items = 0usize;
    for phase in transparent.values() {
        use bevy::render::render_phase::PhaseItem;
        transparent_items += phase.items.len();
        transparent_calls += phase
            .items
            .values()
            .filter(|item| !item.batch_range().is_empty())
            .count();
    }

    info!(
        "draw calls: {} total = {} opaque + {} alpha-mask + {} transparent ({} transparent items)",
        opaque_calls + mask_calls + transparent_calls,
        opaque_calls,
        mask_calls,
        transparent_calls,
        transparent_items,
    );
}

fn binned_draw_calls(
    multidraw: usize,
    batchable: usize,
    unbatchable: usize,
    non_mesh: usize,
) -> usize {
    multidraw + batchable + unbatchable + non_mesh
}

#[cfg(test)]
mod tests {
    /// Smearing is about motion *across the view*, so a particle travelling
    /// with the viewer must stay round however fast the pair of them is going.
    ///
    /// The bug: the streak was keyed off world velocity. Exhaust inherits the
    /// ship's lateral velocity whole — which is what keeps the plume on the
    /// nozzles while drifting — so a drifting ship's motes were nearly still
    /// relative to the pilot and yet carried ~80 u/s of world velocity. Every
    /// plume became a line the moment the ship stopped flying straight.
    #[test]
    fn a_particle_moving_with_the_viewer_does_not_streak() {
        // The rule `build_surface` applies, in one place so the test cannot
        // drift from it.
        fn streaks(mote_vel: Vec3, viewer_vel: Vec3, radius: f32) -> bool {
            let motion = mote_vel - viewer_vel;
            motion.length() * MOTE_SMEAR > radius * 0.4
        }

        let r = 0.35;
        let drifting = Vec3::new(80.0, 0.0, 0.0);

        // Co-moving: fast through the world, motionless to the pilot.
        assert!(
            !streaks(drifting, drifting, r),
            "exhaust hanging at the nozzle must stay a puff",
        );

        // The old rule, for contrast: judged against a still world it streaks,
        // which is exactly what was on screen.
        assert!(
            streaks(drifting, Vec3::ZERO, r),
            "this is the case the fix exists for",
        );

        // And something genuinely crossing the view still streaks.
        assert!(
            streaks(Vec3::new(0.0, 0.0, 60.0), drifting, r),
            "a bolt crossing the view must still smear",
        );
    }

    /// The exhaust may only fall behind *along its own axis*.
    ///
    /// The bug this pins: `vel * inherit` shed 30% of whatever direction the
    /// ship happened to be travelling, so a strafing or drifting aircraft left
    /// its plume sliding off to one side instead of streaming from the nozzles.
    #[test]
    fn exhaust_sheds_speed_only_along_the_nozzle_axis() {
        let back = Vec3::NEG_Z;
        let inherit = 0.70;

        // Straight ahead: unchanged from the expression this replaced, which is
        // the case the emitter was tuned against.
        let ahead = Vec3::new(0.0, 0.0, 80.0);
        assert!(carried_momentum(ahead, back, inherit).abs_diff_eq(ahead * inherit, 1e-5));

        // Pure sideways: nothing is shed, so the plume cannot walk off the
        // side of the aircraft.
        let sideways = Vec3::new(80.0, 0.0, 0.0);
        assert!(carried_momentum(sideways, back, inherit).abs_diff_eq(sideways, 1e-5));

        // A drifting ship: the lateral component survives whole and only the
        // axial one is reduced.
        let drift = Vec3::new(60.0, 12.0, 45.0);
        let got = carried_momentum(drift, back, inherit);
        assert!((got.x - drift.x).abs() < 1e-4, "x moved: {got:?}");
        assert!((got.y - drift.y).abs() < 1e-4, "y moved: {got:?}");
        assert!(
            (got.z - drift.z * inherit).abs() < 1e-4,
            "z should shed 30%: {got:?}"
        );

        // And the residual — what the plume drifts at relative to the ship — is
        // parallel to the exhaust axis, whatever the ship is doing.
        let residual = drift - got;
        assert!(
            residual.normalize().abs_diff_eq(back, 1e-4)
                || residual.normalize().abs_diff_eq(-back, 1e-4),
            "residual {residual:?} is not along the nozzles"
        );
    }

    use super::*;

    #[test]
    fn the_atlas_is_two_cells_and_fades_to_nothing_at_every_edge() {
        let image = build_glow_atlas();
        let w = ATLAS_CELL * 2;
        let data = image.data.as_ref().expect("atlas has pixels");
        assert_eq!(data.len(), (w * ATLAS_CELL * 4) as usize);

        let alpha = |x: u32, y: u32| data[((y * w + x) * 4 + 3) as usize];

        // Both brushes are opaque at their centre...
        assert!(alpha(ATLAS_CELL / 2, ATLAS_CELL / 2) > 200, "glow centre");
        assert!(
            alpha(ATLAS_CELL + ATLAS_CELL / 2, ATLAS_CELL / 2) > 200,
            "core centre"
        );

        // ...and transparent at every corner, which is what makes the seam
        // between them safe under linear filtering.
        for (x, y) in [(0, 0), (ATLAS_CELL - 1, 0), (ATLAS_CELL, 0), (w - 1, 0)] {
            assert_eq!(alpha(x, y), 0, "corner ({x}, {y}) must be transparent");
        }

        // The core brush is the harder-edged of the two: at 60% of the radius
        // it is still solid where the glow has fallen well off.
        let r60 = (ATLAS_CELL as f32 * 0.5 * 0.6) as u32;
        let mid = ATLAS_CELL / 2;
        assert!(alpha(ATLAS_CELL + mid + r60, mid) > alpha(mid + r60, mid));
    }

    /// The whole point of the module: geometry count is independent of entity
    /// count, and the entity count is one.
    #[test]
    fn every_bullet_costs_two_quads_and_no_entities() {
        let mut build = MeshBuild::default();
        let ink = bolt_palette();
        // Mixed sides, because the point of the test is that colouring a bolt
        // by whose it is changes the *vertex* count and nothing else.
        let bullets: Vec<ProjView> = (0..250)
            .map(|i| ProjView {
                key: i,
                pos: [i as f32, 0.0, 0.0],
                dir: [0.0, 0.0, 1.0],
                allegiance: DEMO_SIDES[i as usize % DEMO_SIDES.len()],
            })
            .collect();

        for b in &bullets {
            let dir = Vec3::from_array(b.dir);
            let center = Vec3::from_array(b.pos);
            let ink = ink[b.allegiance as usize];
            build.streak(
                center,
                dir,
                BOLT_HALO_LEN * 0.5,
                BOLT_HALO_HALF_W,
                Vec3::NEG_Z,
                Brush::GLOW,
                ink.halo,
                0.55,
            );
            build.streak(
                center,
                dir,
                BOLT_LEN * 0.5,
                BOLT_CORE_HALF_W,
                Vec3::NEG_Z,
                Brush::CORE,
                ink.core,
                0.95,
            );
        }

        // Two quads a bolt, four vertices and six indices a quad — and one
        // mesh holding all of it.
        assert_eq!(build.pos.len(), 250 * 2 * 4);
        assert_eq!(build.index.len(), 250 * 2 * 6);
        assert_eq!(build.color.len(), build.pos.len());
        assert_eq!(build.uv.len(), build.pos.len());
        assert_eq!(build.normal.len(), build.pos.len());

        // Every index addresses a real vertex.
        let max = *build.index.iter().max().unwrap() as usize;
        assert_eq!(max, build.pos.len() - 1);
    }

    // --- the missile body ---------------------------------------------------

    /// Every triangle in the body faces outward.
    ///
    /// Back faces are culled, so a triangle wound the wrong way is not a
    /// slightly wrong missile — it is a hole through it, and the only place that
    /// shows is on screen. `(b - a) × (c - a)` agreeing with the vertex normals
    /// is exactly Bevy's `FrontFace::Ccw`, so this is that convention checked
    /// without a render device.
    #[test]
    fn every_face_of_the_body_winds_outward() {
        let b = missile_geometry();
        assert_eq!(b.index.len() % 3, 0, "triangles come in threes");
        assert_eq!(b.normal.len(), b.pos.len());
        assert_eq!(b.color.len(), b.pos.len());

        for tri in b.index.chunks(3) {
            let p = |i: u32| Vec3::from_array(b.pos[i as usize]);
            let (a, bb, c) = (p(tri[0]), p(tri[1]), p(tri[2]));
            let geo = (bb - a).cross(c - a);
            assert!(
                geo.length_squared() > 1e-12,
                "degenerate triangle {tri:?} at {a:?}"
            );
            let n = Vec3::from_array(b.normal[tri[0] as usize]);
            assert!(
                geo.normalize().dot(n) > 0.0,
                "triangle {tri:?} winds inward: face {:?} against normal {n:?}",
                geo.normalize()
            );
        }
    }

    /// The proportions are `missiles.js`'s, and the nose points **forward**.
    ///
    /// The second half is the deliberate departure: evaluated, the JS's
    /// `rotateX(-PI / 2)` leaves its cone with radius 0 at the *back* and full
    /// radius at the front — a funnel. This pins the fix so nobody restores the
    /// bug in the name of fidelity.
    #[test]
    fn the_body_is_the_js_missile_pointing_the_right_way() {
        let b = missile_geometry();
        let zs: Vec<f32> = b.pos.iter().map(|p| p[2]).collect();
        let front = zs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let back = zs.iter().copied().fold(f32::INFINITY, f32::min);

        // Nose tip at `BODY_LEN / 2 + NOSE_LEN`, bell mouth at the flame plane.
        assert!((front - (MISSILE_BODY_LEN / 2.0 + MISSILE_NOSE_LEN)).abs() < 1e-4);
        assert!((back - MISSILE_FLAME_Z).abs() < 1e-4);

        // The widest thing on the missile is the fin span, and it is centred.
        let half_span = b.pos.iter().map(|p| p[0].abs()).fold(0.0f32, f32::max);
        assert!((half_span - MISSILE_FIN_SPAN / 2.0).abs() < 1e-4);

        // The nose is a *point*: everything within a whisker of the tip is on
        // the axis. Under the JS's sign it would be a full-radius disc, which
        // is the whole bug.
        let at_tip = b.pos.iter().filter(|p| p[2] > front - 1e-4);
        let mut counted = 0;
        for p in at_tip {
            counted += 1;
            assert!(
                p[0].hypot(p[1]) < 1e-5,
                "the nose is blunt: {p:?} is off the axis at the tip"
            );
        }
        assert_eq!(
            counted,
            MISSILE_SEGMENTS as usize * 2,
            "one tip per segment"
        );

        // And the bell flares: it is wider at the mouth than at its throat.
        let radius_at = |z: f32| {
            b.pos
                .iter()
                .filter(|p| (p[2] - z).abs() < 1e-4)
                .map(|p| p[0].hypot(p[1]))
                .fold(0.0f32, f32::max)
        };
        assert!(
            radius_at(MISSILE_FLAME_Z) > MISSILE_BELL_THROAT_R,
            "the nozzle converges instead of flaring"
        );
    }

    /// The plume leaves the bell, not the middle of the tail.
    ///
    /// The body and the exhaust emitter are written in two different places and
    /// there is nothing but this holding them to the same nozzle — which is
    /// exactly how the JS ended up emitting from inside its own cone.
    #[test]
    fn the_exhaust_leaves_the_bell_mouth() {
        let b = missile_geometry();
        let back = b.pos.iter().map(|p| p[2]).fold(f32::INFINITY, f32::min);
        assert!(
            (MISSILE_FLAME_Z - back).abs() < 1e-4,
            "the emitter at {MISSILE_FLAME_Z} is not on the aft face at {back}"
        );
    }

    /// The pool is fixed and holds a full lobby's salvo.
    #[test]
    fn the_body_pool_covers_a_full_lobby() {
        // Ten ships emptying their tubes, with headroom for the ones already in
        // the air.
        assert!(MISSILE_BODY_POOL >= RULES.weapons.missile_max as usize * 10);
        // And a body is cheap enough that the pool is not a mesh budget:
        // one shared mesh, whatever the count.
        assert!(missile_geometry().pos.len() < 400);
    }

    /// A bolt flying straight at the camera degenerates the side vector. It
    /// must still produce a finite quad rather than NaNs, because a NaN
    /// position poisons the mesh's bounding box and takes the whole buffer off
    /// screen — every effect at once, from one unlucky bullet.
    #[test]
    fn a_bolt_aimed_at_the_camera_stays_finite() {
        let mut build = MeshBuild::default();
        let along = Vec3::Z;
        build.streak(
            Vec3::ZERO,
            along,
            2.5,
            0.16,
            along, // camera forward parallel to the bolt
            Brush::CORE,
            bolt_ink(Allegiance::Own).core,
            1.0,
        );
        assert_eq!(build.pos.len(), 4);
        for p in &build.pos {
            assert!(p.iter().all(|c| c.is_finite()), "degenerate quad: {p:?}");
        }
    }

    /// Runs [`emit_trails`] against one ship for one tick.
    ///
    /// Trails are the one effect a screenshot of a *parked* ship cannot show —
    /// `main.js`'s emitter is silent below `TRAIL_MIN_SPEED`, so the vertical
    /// slice's stationary spawn emits nothing and the geometry goes unseen.
    /// This is that check.
    fn emit_one_tick(flags: ShipFlags, vel: [f32; 3]) -> Effects {
        let mut app = App::new();
        app.init_resource::<SimFrame>()
            .init_resource::<Effects>()
            .add_systems(Update, emit_trails);

        app.world_mut()
            .resource_mut::<SimFrame>()
            .0
            .ships
            .push(sim::world::ShipView {
                id: 1,
                flags: ShipFlags::ALIVE.with(flags),
                pos: [0.0, 0.0, 0.0],
                // Identity rotation, so the nozzles land on `TRAIL_OFFSETS`
                // unchanged and the test is reading placement, not quaternions.
                quat: [0.0, 0.0, 0.0, 1.0],
                vel,
                ..Default::default()
            });

        app.update();
        app.world_mut().remove_resource::<Effects>().unwrap()
    }

    #[test]
    fn a_boosting_ship_smokes_from_both_nozzles() {
        let fx = emit_one_tick(ShipFlags::BOOSTING, [0.0, 0.0, 90.0]);

        // The emitter owes `rate * dt` particles a nozzle each tick, which is
        // below one at any sane tick rate, so the first tick emits nothing and
        // banks the debt. What must hold is that the debt is kept at all --
        // asserting a count here would pin the test to a tick rate.
        let debt = fx.trail_debt.get(&1).copied().unwrap_or(0.0);
        assert!(debt > 0.0, "a boosting ship should owe trail particles");

        // Run enough ticks for the debt to clear several times over.
        let mut app = App::new();
        app.init_resource::<SimFrame>()
            .init_resource::<Effects>()
            .add_systems(Update, emit_trails);
        app.world_mut()
            .resource_mut::<SimFrame>()
            .0
            .ships
            .push(sim::world::ShipView {
                id: 1,
                flags: ShipFlags::ALIVE.with(ShipFlags::BOOSTING),
                quat: [0.0, 0.0, 0.0, 1.0],
                vel: [0.0, 0.0, 90.0],
                ..Default::default()
            });
        // Enough ticks for the debt to clear several times over, expressed in
        // seconds rather than ticks so the expectation follows `TICK_HZ`
        // instead of being pinned to whatever it happened to be.
        let seconds = 8.0 / 60.0;
        let ticks = (seconds * sim::world::TICK_HZ).round() as u32;
        for _ in 0..ticks {
            app.update();
        }
        let fx = app.world_mut().remove_resource::<Effects>().unwrap();

        // Two nozzles at `EMIT_BOOST.rate` for `seconds`, give or take the
        // particle in flight at each end.
        let expected = (EMIT_BOOST.rate * seconds as f32 * 2.0).round() as usize;
        assert!(
            fx.motes.len().abs_diff(expected) <= 2,
            "expected about {expected} motes after {seconds:.3}s of boost, got {}",
            fx.motes.len()
        );

        // Every one of them sits at a nozzle, not at the hull origin. The
        // tolerance carries the sub-tick spread as well as the jitter: a
        // particle is born back where the ship was when it was due, which at
        // 90 u/s is up to a tick's travel behind the pose the emitter sampled.
        let spread = 90.0 * sim::world::TICK_DT as f32;
        for m in &fx.motes {
            let d = trail_offsets()
                .iter()
                .map(|o| m.pos.distance(*o))
                .fold(f32::INFINITY, f32::min);
            assert!(
                d < EMIT_BOOST.jitter * 2.0 + spread,
                "mote at {:?} is {d} from the nearest nozzle",
                m.pos
            );
            assert!(m.half > 0.0 && m.life > 0.0);
        }
    }

    /// The fix for the beading, asserted rather than eyeballed: a particle
    /// leaves the nozzle carrying most of the ship's momentum, so it recedes
    /// slowly instead of being abandoned in place.
    #[test]
    fn exhaust_carries_the_ship_with_it_instead_of_being_left_behind() {
        let ship_speed = 90.0;
        // One tick only banks the debt, so this runs several.
        let mut app = App::new();
        app.init_resource::<SimFrame>()
            .init_resource::<Effects>()
            .add_systems(Update, emit_trails);
        app.world_mut()
            .resource_mut::<SimFrame>()
            .0
            .ships
            .push(sim::world::ShipView {
                id: 1,
                flags: ShipFlags::ALIVE.with(ShipFlags::BOOSTING),
                quat: [0.0, 0.0, 0.0, 1.0],
                vel: [0.0, 0.0, ship_speed],
                ..Default::default()
            });
        for _ in 0..4 {
            app.update();
        }
        let fx = app.world_mut().remove_resource::<Effects>().unwrap();
        assert!(!fx.motes.is_empty(), "four ticks should emit something");

        for m in &fx.motes {
            // Downrange, and slower than the ship: that pair is the effect.
            assert!(m.vel.z > 0.0, "exhaust flying backwards: {:?}", m.vel);
            assert!(
                m.vel.z < ship_speed,
                "exhaust keeping up with the ship: {:?}",
                m.vel
            );
            // And it drags, or the cone never opens.
            assert!(m.drag > 0.0);
        }
    }

    /// A particle moves, slows, and is dropped when its life runs out.
    #[test]
    fn a_particle_flies_and_drags_and_dies() {
        let mut motes = vec![Mote {
            pos: Vec3::ZERO,
            life: 1.0,
            vel: Vec3::new(100.0, 0.0, 0.0),
            drag: 4.0,
            ..default()
        }];

        advance(&mut motes, 0.1);
        let after_one = motes[0].pos.x;
        assert!(after_one > 0.0, "the particle did not move");
        // Semi-implicit: the drag is applied *before* the step, so a tenth of a
        // second at drag 4 moves it by 100 / 1.4 * 0.1, not by 10.
        assert!(
            after_one < 10.0,
            "drag was not applied to the step: {after_one}"
        );

        advance(&mut motes, 0.1);
        let step_two = motes[0].pos.x - after_one;
        assert!(step_two < after_one, "the particle is not slowing down");

        advance(&mut motes, 1.0);
        assert!(motes.is_empty(), "a particle past its life must be dropped");
    }

    /// Colour is a ramp, not a constant — the thing that makes exhaust and fire
    /// look like they are cooling rather than being turned down.
    #[test]
    fn colour_runs_from_hot_to_cold_over_a_life() {
        let hot = LinearRgba::rgb(4.0, 2.0, 0.0);
        let cold = LinearRgba::rgb(0.0, 0.0, 1.0);
        assert_eq!(mix(hot, cold, 0.0).red, 4.0);
        assert_eq!(mix(hot, cold, 1.0).blue, 1.0);
        let half = mix(hot, cold, 0.5);
        assert!((half.red - 2.0).abs() < 1e-6);
        assert!((half.blue - 0.5).abs() < 1e-6);

        // And every emitter actually uses it: no mode ships the same colour at
        // both ends, which would be the old flat-colour behaviour wearing the
        // new field.
        for mode in [&EMIT_MOVE, &EMIT_BOOST, &EMIT_BRAKE] {
            assert_ne!(
                glow(mode.hot.0, mode.hot.1).blue,
                glow(mode.cool.0, mode.cool.1).blue
            );
        }
    }

    /// An explosion is a burst, not a disc: it throws fragments, it flashes
    /// before it burns, and the flash is the brightest thing in it.
    #[test]
    fn an_explosion_flashes_then_burns_then_throws_sparks() {
        let mut fx = Effects::default();
        spawn_explosion(&mut fx, ExplosionKind::ShipDeath, Vec3::ZERO, 6.0);

        assert_eq!(fx.shells.len(), SHIP_DEATH.shells.len());
        assert_eq!(fx.motes.len(), SHIP_DEATH.sparks);

        // The flash is the first shell, and it is both the brightest and the
        // shortest — that ordering is what separates a detonation from a glow.
        let peak = |c: LinearRgba| c.red.max(c.green).max(c.blue);
        for pair in fx.shells.windows(2) {
            assert!(
                peak(pair[0].color) > peak(pair[1].color),
                "shells must cool outward"
            );
            assert!(pair[0].life < pair[1].life, "shells must outlive inward");
        }
        // Every shell cools over its own life, too.
        for s in &fx.shells {
            assert!(peak(s.cool) < peak(s.color), "a shell that does not cool");
        }

        // Fragments fly outward from the centre, at a spread of speeds.
        let speeds: Vec<f32> = fx.motes.iter().map(|m| m.vel.length()).collect();
        assert!(
            speeds.iter().all(|s| *s > 0.0),
            "a spark that does not move"
        );
        let lo = speeds.iter().copied().fold(f32::INFINITY, f32::min);
        let hi = speeds.iter().copied().fold(0.0, f32::max);
        assert!(
            hi > lo * 1.5,
            "every spark left at the same speed: {lo}..{hi}"
        );
        for m in &fx.motes {
            assert!(m.vel.dot(m.pos) > 0.0, "a spark flying inward");
        }
    }

    /// The shell's radius eases out and its alpha holds then drops, rather than
    /// both running linearly the way the JS's single sphere does.
    #[test]
    fn a_shell_expands_fast_and_then_coasts() {
        let s = Shell {
            from: 0.0,
            to: 10.0,
            life: 1.0,
            ease: 0.5,
            ..default()
        };
        let radius = |t: f32| s.from + (s.to - s.from) * t.powf(s.ease);
        // Half the life is well past half the radius.
        assert!(radius(0.5) > 6.5, "{}", radius(0.5));
        // And the second half covers less ground than the first.
        assert!(radius(1.0) - radius(0.5) < radius(0.5) - radius(0.0));
    }

    /// `main.js` picks brake over boost over move, in that order, and emits
    /// nothing at all from a ship that is barely drifting.
    #[test]
    fn the_emitter_picks_a_mode_the_way_the_js_does() {
        // Below `TRAIL_MIN_SPEED` and not boosting: silent, and no debt kept.
        let fx = emit_one_tick(ShipFlags::NONE, [0.0, 0.0, 1.0]);
        assert!(fx.motes.is_empty());
        assert!(fx.trail_debt.is_empty(), "a parked ship should owe nothing");

        // Braking wins over boosting when both are set.
        let fx = emit_one_tick(
            ShipFlags::BRAKING.with(ShipFlags::BOOSTING),
            [0.0, 0.0, 90.0],
        );
        // The debt now only holds the *fraction* a tick owes, because the rates
        // are high enough to clear whole particles every tick — so what says
        // which mode won is the colour of what came out. Brake is retro-thrust
        // and warm; boost is afterburner and cool.
        let tick = sim::world::TICK_DT as f32;
        let debt = fx.trail_debt.get(&1).copied().unwrap();
        assert!(
            (debt - (EMIT_BRAKE.rate * tick).fract()).abs() < 1e-4,
            "brake rate should win over boost: {debt}"
        );
        let owed = (EMIT_BRAKE.rate * tick).floor() as usize * 2;
        assert_eq!(fx.motes.len(), owed, "two nozzles at the brake rate");
        for m in &fx.motes {
            assert!(
                m.color.red > m.color.blue,
                "brake exhaust should be warm, got {:?}",
                m.color
            );
        }

        // A boss hitbox is never drawn, so it never smokes either.
        let fx = emit_one_tick(ShipFlags::BOSS_HITBOX.with(ShipFlags::BOOSTING), [0.0; 3]);
        assert!(fx.motes.is_empty());
        assert!(fx.trail_debt.is_empty());
    }

    /// `trails.js` drops the oldest particle on overflow. The cap is what
    /// bounds the per-frame vertex rebuild, so it has to actually hold.
    #[test]
    fn the_mote_cap_holds_and_drops_the_oldest() {
        let mut motes = Vec::new();
        for i in 0..(MAX_MOTES + 40) {
            push_mote(
                &mut motes,
                Mote {
                    pos: Vec3::splat(i as f32),
                    half: 0.2,
                    shrink: 0.45,
                    color: bolt_ink(Allegiance::Own).core,
                    opacity: 0.85,
                    ..default()
                },
            );
        }
        assert_eq!(motes.len(), MAX_MOTES);
        // The survivors are the newest ones.
        assert_eq!(motes[0].pos.x, 40.0);
    }

    // --- battle damage ------------------------------------------------------

    /// Runs [`emit_damage`] against one ship for a given number of seconds.
    ///
    /// Seconds rather than ticks, so the expectations below follow `TICK_HZ`
    /// instead of being pinned to whatever it happens to be — the same reason
    /// `a_boosting_ship_smokes_from_both_nozzles` counts that way.
    fn burn(hp: i32, seconds: f64) -> Effects {
        let mut app = App::new();
        app.init_resource::<SimFrame>()
            .init_resource::<Effects>()
            .add_systems(Update, emit_damage);
        app.world_mut()
            .resource_mut::<SimFrame>()
            .0
            .ships
            .push(sim::world::ShipView {
                id: 1,
                hp,
                flags: ShipFlags::ALIVE,
                quat: [0.0, 0.0, 0.0, 1.0],
                ..Default::default()
            });

        let ticks = (seconds * sim::world::TICK_HZ).round() as u32;
        for _ in 0..ticks {
            app.update();
        }
        app.world_mut().remove_resource::<Effects>().unwrap()
    }

    /// A healthy ship is clean, and does not even keep an accumulator.
    #[test]
    fn a_healthy_ship_does_not_smoke() {
        let fx = burn(RULES.ship.max_hp, 0.5);
        assert!(fx.damage.is_empty());
        assert!(fx.damage_debt.is_empty(), "no debt kept for a clean hull");
    }

    /// The plume thickens as the hull goes, which is the whole read.
    #[test]
    fn the_plume_thickens_as_the_hull_goes() {
        let count = |hp| burn(hp, 0.5).damage.len();
        let light = count(55);
        let heavy = count(30);
        let dying = count(8);

        assert!(light > 0, "a hull past the threshold streams something");
        assert!(heavy > light, "{heavy} at 30 HP is not more than {light}");
        assert!(dying > heavy, "{dying} at 8 HP is not more than {heavy}");
    }

    /// Fire is a *separate* threshold, and it only lights near the end.
    #[test]
    fn fire_only_takes_at_low_hull() {
        // Smoke is dim and under the bloom floor; fire is over it. That is what
        // tells the two apart in the pool without a discriminant field.
        let lit = |fx: &Effects| {
            fx.damage
                .iter()
                .filter(|m| m.color.red.max(m.color.green).max(m.color.blue) > 0.9)
                .count()
        };
        assert_eq!(lit(&burn(50, 0.5)), 0, "half a hull is smoke, not fire");
        assert!(lit(&burn(10, 0.5)) > 0, "a tenth of a hull is burning");
    }

    /// A wreck stops streaming — the death explosion is the effect for that —
    /// and a boss hitbox never starts, since twenty of them are one ship.
    #[test]
    fn the_dead_and_the_boss_do_not_burn() {
        let mut app = App::new();
        app.init_resource::<SimFrame>()
            .init_resource::<Effects>()
            .add_systems(Update, emit_damage);
        app.world_mut().resource_mut::<SimFrame>().0.ships.extend([
            sim::world::ShipView {
                id: 1,
                hp: 5,
                flags: ShipFlags::NONE,
                quat: [0.0, 0.0, 0.0, 1.0],
                ..Default::default()
            },
            sim::world::ShipView {
                id: 2,
                hp: 5,
                flags: ShipFlags::ALIVE.with(ShipFlags::BOSS_HITBOX),
                quat: [0.0, 0.0, 0.0, 1.0],
                ..Default::default()
            },
        ]);
        for _ in 0..30 {
            app.update();
        }
        let fx = app.world_mut().remove_resource::<Effects>().unwrap();
        assert!(fx.damage.is_empty());
        assert!(fx.damage_debt.is_empty());
    }

    /// The pool is bounded on its own, so a squadron of burning wrecks cannot
    /// evict a single engine trail — and neither list can grow without limit.
    #[test]
    fn the_damage_pool_is_capped_separately() {
        let fx = burn(1, 30.0);
        assert!(fx.damage.len() <= MAX_DAMAGE_MOTES);
        assert_eq!(fx.damage.len(), MAX_DAMAGE_MOTES, "30s should fill it");
        assert!(fx.motes.is_empty(), "damage must not touch the trail pool");

        // And all three together stay inside the mesh's fixed vertex budget,
        // which is what stops the slab allocator's use-after-free coming back.
        const { assert!(MAX_MOTES + MAX_DAMAGE_MOTES + MAX_PULSE_MOTES < MESH_QUAD_CAPACITY) };
    }

    /// The EMP front gets its own pool, and this is the bug that made it need
    /// one: a saturated trail pool deleted the wave before it was sixty units
    /// out, so the pulse was invisible in exactly the situation — a busy
    /// dogfight — that it is fired in.
    #[test]
    fn the_pulse_front_cannot_be_evicted_by_engine_trails() {
        let mut fx = Effects::default();
        // A trail pool already at its cap, as a ten-ship skirmish reaches within
        // seconds of the match starting.
        for _ in 0..MAX_MOTES {
            push_mote(&mut fx.motes, Mote::default());
        }
        spawn_emp(&mut fx, Vec3::ZERO, 300.0);
        assert_eq!(fx.pulse.len(), EMP_MOTES);
        assert_eq!(fx.motes.len(), MAX_MOTES, "the front took no trail slots");

        // A second full second of trail emission does not touch it.
        for _ in 0..MAX_MOTES {
            push_mote(&mut fx.motes, Mote::default());
        }
        assert_eq!(fx.pulse.len(), EMP_MOTES, "and none of it was evicted");

        // Two overlapping pulses both survive whole; a third evicts the first.
        spawn_emp(&mut fx, Vec3::ZERO, 300.0);
        assert_eq!(fx.pulse.len(), MAX_PULSE_MOTES);
        spawn_emp(&mut fx, Vec3::ZERO, 300.0);
        assert_eq!(fx.pulse.len(), MAX_PULSE_MOTES, "bounded on its own");
    }

    /// Every particle sits on the wing, not at the hull origin — the fit test.
    ///
    /// With an identity rotation and the ship at the origin, a mote's position
    /// *is* the anchor plus its jitter, so this reads placement directly.
    #[test]
    fn the_plume_hangs_off_an_anchor_that_follows_the_fit() {
        let anchors = damage_offsets();
        // The fit is a scale about a shifted origin, so an anchor is never at
        // the ship's own origin — which is the failure this guards.
        assert!(anchors[0].length() > 1.0, "{:?}", anchors[0]);
        assert!((anchors[0].x + anchors[1].x).abs() < 1e-4, "wings mirror");

        let fx = burn(10, 0.5);
        for m in &fx.damage {
            let d = anchors
                .iter()
                .map(|a| m.pos.distance(*a))
                .fold(f32::INFINITY, f32::min);
            assert!(d <= SMOKE_JITTER * 2.0, "mote at {:?} is {d} off", m.pos);
            assert!(m.half > 0.0 && m.life > 0.0);
        }
    }

    /// Smoke is additive and therefore cannot be dark; it must at least be
    /// dim enough not to bloom, or it reads as light rather than as matter.
    #[test]
    fn smoke_stays_under_the_bloom_floor_and_fire_clears_it() {
        let peak = |c: LinearRgba| c.red.max(c.green).max(c.blue);
        for (name, c) in [("hot", SMOKE_HOT), ("cold", SMOKE_COLD)] {
            assert!(peak(c) < 0.9, "{name} smoke peaks at {}", peak(c));
        }
        // And it cools as it goes, which is the only depth cue an additive
        // plume has: lit at the wing, dead downstream.
        assert!(peak(SMOKE_COLD) < peak(SMOKE_HOT));

        for c in FIRE_COLORS {
            assert!(peak(c) > 0.9, "fire colour peaks at {}", peak(c));
        }
        // The ember every flame ends on is under the floor: the tail of a flame
        // is soot, not light.
        assert!(
            peak(FIRE_EMBER) < 0.9,
            "ember peaks at {}",
            peak(FIRE_EMBER)
        );
    }

    /// The ramp is continuous, so the plume grows into view rather than
    /// switching on.
    #[test]
    fn the_damage_ramp_is_continuous() {
        assert_eq!(ramp(1.0, SMOKE_AT), 0.0);
        assert_eq!(ramp(SMOKE_AT, SMOKE_AT), 0.0);
        assert_eq!(ramp(0.0, SMOKE_AT), 1.0);
        assert!((ramp(SMOKE_AT / 2.0, SMOKE_AT) - 0.5).abs() < 1e-6);
        // Fire is the tighter threshold of the two, so a ship always smokes
        // before it burns.
        const { assert!(FIRE_AT < SMOKE_AT) };
    }

    /// The palette constants are hand-resolved from the JS hexes, so a test
    /// has to be what keeps them honest.
    #[test]
    fn the_palette_matches_the_js_hexes() {
        let close = |a: f32, b: f32, what: &str| {
            assert!((a - b).abs() < 0.02, "{what}: {a} vs {b}");
        };
        // The local pair is what this module drew before it knew about sides,
        // and it must not have moved: `bullets.js` `localMats`, core 0xeaffe6
        // at 4.4 and halo 0x44ffb0 at 1.5.
        let own = bolt_ink(Allegiance::Own);
        close(own.core.red, 6.16, "core red");
        close(own.core.green, 7.49, "core green");
        close(own.core.blue, 5.92, "core blue");
        // The halo's *intensity* is a tuning knob (see `BOLT_HALO_HALF_W`), so
        // what is pinned is the hex it is built from — `bullets.js` `localMats`
        // halo 0x44ffb0 — not the brightness it is scaled to.
        let want = glow(0x44ffb0, BOLT_HALO_GLOW);
        close(own.halo.red, want.red, "halo red");
        close(own.halo.green, want.green, "halo green");
        close(own.halo.blue, want.blue, "halo blue");
        assert!(
            own.halo.green > own.halo.blue && own.halo.blue > own.halo.red,
            "0x44ffb0 is a green-dominant teal: {own:?}",
            own = own.halo
        );

        // Every effect colour has to clear the bloom prefilter threshold of
        // 0.9 on at least one channel, or the Ultra look is not being hit.
        let mut named: Vec<(String, LinearRgba)> = vec![
            ("missile nozzle".to_owned(), MISSILE_NOZZLE),
            ("flare core".to_owned(), FLARE_CORE),
            ("flare glow".to_owned(), FLARE_GLOW),
        ];
        for side in DEMO_SIDES {
            let ink = bolt_ink(side);
            named.push((format!("{side:?} core"), ink.core));
            named.push((format!("{side:?} halo"), ink.halo));
        }
        for (name, c) in &named {
            let peak = c.red.max(c.green).max(c.blue);
            assert!(peak > 0.9, "{name} peaks at {peak}, below the bloom floor");
        }
    }

    /// The whole point of the palette: three sides that are not each other.
    ///
    /// A colour test that only pinned hex values would pass on three shades of
    /// the same green. This asserts *separation* — that each halo is dominated
    /// by a different channel, which is the property a player actually reads at
    /// speed, and that no two are close enough to confuse.
    #[test]
    fn the_three_sides_do_not_look_alike() {
        let dominant = |c: LinearRgba| {
            let (r, g, b) = (c.red, c.green, c.blue);
            if r >= g && r >= b {
                0
            } else if g >= b {
                1
            } else {
                2
            }
        };
        let own = bolt_ink(Allegiance::Own).halo;
        let ally = bolt_ink(Allegiance::Ally).halo;
        let hostile = bolt_ink(Allegiance::Hostile).halo;

        // Green is yours, blue is your team's, red is incoming — `bullets.js`'s
        // convention, which the shipped game already taught its players.
        assert_eq!(dominant(own), 1, "your own fire should read green");
        assert_eq!(dominant(ally), 2, "a team-mate's should read blue");
        assert_eq!(dominant(hostile), 0, "incoming should read red");

        // And they are far apart in the space the shader actually blends in.
        let apart = |a: LinearRgba, b: LinearRgba| {
            ((a.red - b.red).powi(2) + (a.green - b.green).powi(2) + (a.blue - b.blue).powi(2))
                .sqrt()
        };
        for (l, r, what) in [
            (own, ally, "own vs ally"),
            (own, hostile, "own vs hostile"),
            (ally, hostile, "ally vs hostile"),
        ] {
            assert!(apart(l, r) > 1.5, "{what} are only {} apart", apart(l, r));
        }
    }

    /// A frame with every side in it costs the same as a frame with one side.
    ///
    /// The constraint the whole module is built around: colour rides in the
    /// vertex buffer, so knowing whose a bolt is cannot turn into a mesh, a
    /// material, or an entity per shooter.
    #[test]
    fn colouring_by_side_costs_no_geometry() {
        let build = |sides: &[Allegiance]| {
            let mut b = MeshBuild::default();
            let ink = bolt_palette();
            for (i, side) in sides.iter().enumerate() {
                b.streak(
                    Vec3::new(i as f32, 0.0, 0.0),
                    Vec3::Z,
                    BOLT_LEN * 0.5,
                    BOLT_CORE_HALF_W,
                    Vec3::NEG_Z,
                    Brush::CORE,
                    ink[*side as usize].core,
                    0.95,
                );
            }
            b
        };
        let one = build(&[Allegiance::Own; 60]);
        let all: Vec<Allegiance> = (0..60).map(|i| DEMO_SIDES[i % DEMO_SIDES.len()]).collect();
        let mixed = build(&all);

        assert_eq!(one.pos.len(), mixed.pos.len());
        assert_eq!(one.index.len(), mixed.index.len());
        // Same geometry, different paint — which is the only thing that may
        // differ between the two.
        assert_eq!(one.pos, mixed.pos);
        assert_ne!(one.color, mixed.color);
    }
}
