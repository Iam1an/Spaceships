//! Projectiles, beams, and effects — **one mesh, one material, one draw call**.
//!
//! [`crate`]'s module docs name this file's constraint directly: the JS client
//! spends 477 draw calls at p99 because it allocates a mesh per entity, and
//! `ProjView`/`FlareView` are 32-byte records with hundreds in flight. A mesh
//! per bolt reproduces that bottleneck exactly.
//!
//! # The shape that does not
//!
//! There is exactly **one** rendered entity in this module — [`EffectSurface`]
//! — carrying one [`Mesh`] and one [`StandardMaterial`]. Every frame,
//! [`build_surface`] clears that mesh and rewrites it from the current
//! [`SimFrame`] slices plus this module's own effect state. Bullets, bullet
//! halos, missile bodies, missile exhaust, flare cores and glows, beams,
//! explosion shells, muzzle flashes, and engine trails all land in the same
//! vertex buffer.
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
//! # What is missing from `Frame`
//!
//! - **`ProjView` has no owner or team.** `bullets.js` keeps three material
//!   pairs — `self`/`ally`/`enemy` — and a bolt's colour is the clearest read
//!   on whether it is about to hurt you. Nothing in the 32-byte record says. A
//!   `team: i32` (or an owner id, which also buys per-shooter trails) is the
//!   missing field. Everything here draws the `self` palette.
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
//! projectiles so the count can be taken against a busy scene.
//! [`crate::sim_bridge::tick`] does not yet run `sim::bullets` or
//! `sim::missiles`, so `Frame`'s projectile slices are empty in this build and
//! there is otherwise nothing to draw.

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::image::{ImageSampler, ImageSamplerDescriptor};
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use sim::world::{ExplosionKind, FlareView, ProjView, ShipFlags, SimEvent, WeaponKind};
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
/// so the core is widened to keep a bolt visible at range; the halo is
/// faithful.
const BOLT_CORE_HALF_W: f32 = 0.16;
/// `bullets.js` halo radius, and its 1.15× length.
const BOLT_HALO_HALF_W: f32 = 0.32;
/// `bullets.js`: halo cylinder is `LASER_LEN * 1.15`.
const BOLT_HALO_LEN: f32 = BOLT_LEN * 1.15;

/// `beams.js`: cylinder radius 0.5.
const BEAM_HALF_W: f32 = 0.5;
/// `beams.js`: `LIFE`.
const BEAM_LIFE: f32 = 0.18;
/// `beams.js`: base opacity, faded linearly over `BEAM_LIFE`.
const BEAM_OPACITY: f32 = 0.9;

/// `missiles.js`: `BODY_LEN`.
const MISSILE_BODY_LEN: f32 = 3.5;
/// `missiles.js`: `BODY_RAD`, widened for the same billboard reason as the
/// bolt core.
const MISSILE_BODY_HALF_W: f32 = 0.34;
/// `missiles.js`: `NOZZLE_Z`, the local-space exhaust origin.
const MISSILE_NOZZLE_Z: f32 = -1.93;
/// `missiles.js`: `TRAIL_INTERVAL`, the exhaust emission period.
const MISSILE_EXHAUST_INTERVAL: f32 = 0.028;

/// `missiles.js`: flare core sphere radius.
const FLARE_CORE_R: f32 = 0.30;
/// `missiles.js`: flare glow sphere radius.
const FLARE_GLOW_R: f32 = 1.10;

/// `trails.js`: `MAX_PARTICLES`. A hard cap across every ship, which is what
/// bounds the vertex rebuild — see [`Effects::motes`].
const MAX_MOTES: usize = 320;

/// Fixed vertex and index budget for the effects mesh.
///
/// The mesh is rebuilt every frame, and it **must not change size** doing so --
/// see the padding in `rebuild`. Sized for the worst case the caps above allow,
/// with headroom: every quad is 4 vertices and 6 indices.
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
/// Keyed off the same `SPACESHIPS_SHIP_MODEL` switch `scene.rs` reads, so the
/// trails follow the model rather than needing a second decision. When the jet
/// becomes the default this collapses to one constant.
/// Multiplied by [`crate::scene::SHIP_SCALE`] because these are consumed in
/// **world** space — `emit` computes `ship.pos + quat * offset` rather than
/// parenting the motes to the hull — so unlike the mesh they do not inherit the
/// ship's scale and have to be given it. Both constants are authored in the
/// same pre-scale space the JS authors `TRAIL_OFFSETS` in, where they are
/// children of a group `main.js:219` scales.
fn trail_offsets() -> [Vec3; 2] {
    #[cfg(not(target_arch = "wasm32"))]
    if std::env::var("SPACESHIPS_SHIP_MODEL").is_ok_and(|m| m.contains("jet")) {
        return TRAIL_OFFSETS_JET.map(|o| o * crate::scene::SHIP_SCALE);
    }
    TRAIL_OFFSETS_LEGACY.map(|o| o * crate::scene::SHIP_SCALE)
}

/// One row of `main.js`'s `EMIT_CONFIG`.
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
    /// Colours, chosen uniformly.
    colors: &'static [u32],
}

/// `EMIT_CONFIG.move` — the idle cruise trail.
const EMIT_MOVE: EmitMode = EmitMode {
    rate: 18.0,
    scale: (0.16, 0.28),
    life: (0.18, 0.30),
    jitter: 0.05,
    colors: &[0xffffff],
};
/// `EMIT_CONFIG.boost`.
const EMIT_BOOST: EmitMode = EmitMode {
    rate: 45.0,
    scale: (0.50, 0.85),
    life: (0.45, 0.65),
    jitter: 0.13,
    colors: &[0x66ddff, 0xffffff],
};
/// `EMIT_CONFIG.brake`.
const EMIT_BRAKE: EmitMode = EmitMode {
    rate: 35.0,
    scale: (0.36, 0.60),
    life: (0.28, 0.45),
    jitter: 0.10,
    colors: &[0xffd933, 0xffaa33],
};

/// `main.js:1143`: below this speed a coasting ship emits nothing.
const TRAIL_MIN_SPEED: f32 = 5.0;

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
            .add_systems(FixedUpdate, (consume_events, emit_trails).after(SimSet))
            // Ageing and the rebuild are per *frame*: they are what makes the
            // effects smooth on a display that is not the tick rate.
            .add_systems(Update, (run_demo, age_effects).chain())
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
    /// A near-solid disc with a soft rim: bolt cores, beams, missile bodies —
    /// anything that should read as a hard object rather than a haze.
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
    /// Radius at birth and at death; the JS lerps linearly between them.
    from: f32,
    to: f32,
    color: LinearRgba,
    opacity: f32,
}

/// A sustained hitscan beam, kept alive for [`BEAM_LIFE`] because
/// `SimEvent::Fired` reports it exactly once.
struct BeamFx {
    start: Vec3,
    end: Vec3,
    age: f32,
    color: LinearRgba,
}

/// One trail or exhaust particle. `trails.js` calls these particles; they do
/// not move after emission.
struct Mote {
    pos: Vec3,
    age: f32,
    life: f32,
    /// Half-extent at birth, in world units.
    half: f32,
    /// Per-second growth factor applied over the life, as
    /// `half * (1 + t * grow)`. Zero for engine trails, 2.8 for exhaust.
    grow: f32,
    /// Shrink factor, as `half * (1 - t * shrink)`. `trails.js` uses 0.45.
    shrink: f32,
    color: LinearRgba,
    opacity: f32,
}

/// Everything this module owns that is not in [`SimFrame`].
#[derive(Resource, Default)]
struct Effects {
    shells: Vec<Shell>,
    beams: Vec<BeamFx>,
    motes: Vec<Mote>,
    /// Trail emission accumulators, keyed by ship id. A ship emitting at 45 Hz
    /// against a 60 Hz tick owes a fractional particle each tick.
    trail_debt: HashMap<i32, f32>,
    /// Missile exhaust accumulators, keyed by `ProjView::key`.
    exhaust_debt: HashMap<u64, f32>,
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
                ..
            } => match weapon {
                // For a beam `dir` is the *endpoint*, not a direction —
                // `SimEvent::Fired` says so, and it is the one field in this
                // enum whose meaning changes with a sibling.
                WeaponKind::Beam => fx.beams.push(BeamFx {
                    start: to_vec3([origin.x as f32, origin.y as f32, origin.z as f32]),
                    end: to_vec3([dir.x as f32, dir.y as f32, dir.z as f32]),
                    age: 0.0,
                    color: glow(0x88ffd6, 1.6),
                }),
                // A muzzle flash. `bullets.js` has none — the bolt spawns at
                // the muzzle and that reads as one — but the gun fires at
                // 20 Hz and a single frame of light at the barrel is what
                // sells it at this brightness.
                WeaponKind::Bullet | WeaponKind::Missile => {
                    let p = to_vec3([origin.x as f32, origin.y as f32, origin.z as f32]);
                    fx.shells.push(Shell {
                        pos: p,
                        age: 0.0,
                        life: 0.06,
                        from: 0.5,
                        to: 1.4,
                        color: glow(0xeaffe6, 2.2),
                        opacity: 0.9,
                    });
                }
            },

            SimEvent::FlareBurst { origin, .. } => {
                let p = to_vec3([origin.x as f32, origin.y as f32, origin.z as f32]);
                push_shells(&mut fx.shells, p, 1.0, FLARE_BURST_SHELLS);
            }

            SimEvent::Explosion { pos, scale, kind } => {
                let p = to_vec3([pos.x as f32, pos.y as f32, pos.z as f32]);
                let s = scale as f32;
                match kind {
                    // `missiles.spawnExplosion`: three concentric shells.
                    ExplosionKind::MissileHit => {
                        push_shells(&mut fx.shells, p, s.max(0.6), MISSILE_HIT_SHELLS);
                    }
                    ExplosionKind::FlareBurst => {
                        push_shells(&mut fx.shells, p, s.max(0.6), FLARE_BURST_SHELLS);
                    }
                    // `bullets.spawnExplosion`: one shell, `lerp(s*0.4, s*2.6)`
                    // over 0.55 s. The caller's `scale` is what distinguishes a
                    // 0.4-unit bullet spark from a 6-unit ship death.
                    ExplosionKind::Impact
                    | ExplosionKind::ShipDeath
                    | ExplosionKind::AsteroidBreak => {
                        let color = match kind {
                            ExplosionKind::AsteroidBreak => glow(0xffaa55, 1.5),
                            ExplosionKind::ShipDeath => glow(0xffcc88, 2.2),
                            _ => glow(0xffaa55, 1.8),
                        };
                        fx.shells.push(Shell {
                            pos: p,
                            age: 0.0,
                            life: 0.55,
                            from: s * 0.4,
                            to: s * 2.6,
                            color,
                            opacity: 0.95,
                        });
                    }
                }
            }

            _ => {}
        }
    }
}

/// `missiles.spawnExplosion` — flash, fire, smoke. `(hex, intensity, from, to, life)`.
const MISSILE_HIT_SHELLS: &[(u32, f32, f32, f32, f32)] = &[
    (0xffffff, 2.6, 0.8, 5.0, 0.30),
    (0xff9900, 2.0, 1.4, 11.0, 0.52),
    (0xff3300, 1.4, 2.0, 16.0, 0.70),
];

/// `spawnFlareBurst` — the same three-shell shape, sharper and smaller.
const FLARE_BURST_SHELLS: &[(u32, f32, f32, f32, f32)] = &[
    (0xffffff, 2.8, 0.15, 3.5, 0.18),
    (0xffee44, 2.2, 0.40, 6.5, 0.26),
    (0xff8800, 1.6, 0.70, 10.0, 0.32),
];

fn push_shells(out: &mut Vec<Shell>, pos: Vec3, scale: f32, spec: &[(u32, f32, f32, f32, f32)]) {
    for &(h, intensity, from, to, life) in spec {
        out.push(Shell {
            pos,
            age: 0.0,
            life,
            from: from * scale,
            to: to * scale,
            color: glow(h, intensity),
            opacity: 0.95,
        });
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
        let mode = if ship.flags.contains(ShipFlags::BRAKING) {
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

        for offset in trail_offsets() {
            let nozzle = base + quat * offset;
            for _ in 0..n {
                let j = mode.jitter;
                let jitter = Vec3::new(
                    fx.rng.range(-j, j),
                    fx.rng.range(-j, j),
                    fx.rng.range(-j, j),
                );
                let scale = fx.rng.range(mode.scale.0, mode.scale.1);
                let life = fx.rng.range(mode.life.0, mode.life.1);
                let color = fx.rng.pick(mode.colors);
                push_mote(
                    &mut fx.motes,
                    Mote {
                        pos: nozzle + jitter,
                        age: 0.0,
                        life,
                        // `trails.js` geometry is a radius-0.5 sphere scaled by
                        // `scale`, so the world half-extent is half of it.
                        half: scale * 0.5,
                        grow: 0.0,
                        shrink: 0.45,
                        color: glow(color, 1.4),
                        opacity: 0.85,
                    },
                );
            }
        }
    }
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

    for m in &mut fx.motes {
        m.age += dt;
    }
    fx.motes.retain(|m| m.age < m.life);

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
        for _ in 0..puffs {
            let dir = to_vec3(m.dir);
            let nozzle = to_vec3(m.pos) + dir * MISSILE_NOZZLE_Z;
            let scale = fx.rng.range(0.45, 1.10);
            let life = fx.rng.range(0.30, 0.42);
            push_mote(
                &mut fx.motes,
                Mote {
                    pos: nozzle,
                    age: 0.0,
                    life,
                    half: scale * 0.5,
                    // `missiles.js`: `initScale * (1 + t * 2.8)`.
                    grow: 2.8,
                    shrink: 0.0,
                    color: glow(0xff7700, 1.8),
                    opacity: 0.72,
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
    /// the billboard a cylinder degrades to. Bolts, beams, missile bodies, and
    /// trail ribbons are all this.
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

    // ── bullets ──────────────────────────────────────────────────────────
    //
    // `bullets.js` draws a core cylinder inside a wider, dimmer halo cylinder,
    // both additive. Two quads per bolt, both in this buffer.
    for b in frame.0.bullets.iter().chain(fx.demo.bullets.iter()) {
        let dir = to_vec3(b.dir);
        let center = to_vec3(b.pos) + dir * bullet_lead;
        build.streak(
            center,
            dir,
            BOLT_HALO_LEN * 0.5,
            BOLT_HALO_HALF_W,
            cam_fwd,
            Brush::GLOW,
            BOLT_HALO,
            0.55,
        );
        build.streak(
            center,
            dir,
            BOLT_LEN * 0.5,
            BOLT_CORE_HALF_W,
            cam_fwd,
            Brush::CORE,
            BOLT_CORE,
            0.95,
        );
    }

    // ── missiles ─────────────────────────────────────────────────────────
    //
    // The JS body is six opaque parts; at the distance a missile is ever seen
    // that is a grey streak with a pulsing orange nozzle, which is what these
    // two quads are. The exhaust is emitted as motes in `age_effects`.
    let t = frame.0.time as f32;
    for m in frame.0.missiles.iter().chain(fx.demo.missiles.iter()) {
        let dir = to_vec3(m.dir);
        let center = to_vec3(m.pos) + dir * missile_lead;
        build.streak(
            center,
            dir,
            MISSILE_BODY_LEN * 0.5,
            MISSILE_BODY_HALF_W,
            cam_fwd,
            Brush::CORE,
            MISSILE_BODY,
            1.0,
        );
        // `missiles.js`: `pulse = 0.75 + 0.45 * |sin(age * 19)|`. Phase is
        // offset per missile off the key so a salvo does not throb in unison.
        let phase = (m.key % 997) as f32 * 0.0063;
        let pulse = 0.75 + 0.45 * (t * 19.0 + phase).sin().abs();
        build.puff(
            center + dir * MISSILE_NOZZLE_Z,
            0.55 * pulse,
            cam_right,
            cam_up,
            cam_fwd,
            Brush::GLOW,
            MISSILE_NOZZLE,
            0.70 + 0.25 * pulse,
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
    for b in &fx.beams {
        let seg = b.end - b.start;
        let len = seg.length();
        if len < 1e-3 {
            continue;
        }
        let fade = 1.0 - b.age / BEAM_LIFE;
        build.streak(
            b.start + seg * 0.5,
            seg / len,
            len * 0.5,
            BEAM_HALF_W,
            cam_fwd,
            Brush::CORE,
            b.color,
            fade * BEAM_OPACITY,
        );
    }

    // ── explosion shells ─────────────────────────────────────────────────
    for s in &fx.shells {
        let t = (s.age / s.life).clamp(0.0, 1.0);
        let r = s.from + (s.to - s.from) * t;
        build.puff(
            s.pos,
            r,
            cam_right,
            cam_up,
            cam_fwd,
            Brush::GLOW,
            s.color,
            (1.0 - t) * s.opacity,
        );
    }

    // ── trail and exhaust motes ──────────────────────────────────────────
    for m in &fx.motes {
        let t = (m.age / m.life).clamp(0.0, 1.0);
        let r = m.half * (1.0 + t * m.grow) * (1.0 - t * m.shrink);
        build.puff(
            m.pos,
            r,
            cam_right,
            cam_up,
            cam_fwd,
            Brush::GLOW,
            m.color,
            (1.0 - t) * m.opacity,
        );
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
    let cap_verts = MESH_VERTEX_CAPACITY;
    let cap_indices = MESH_INDEX_CAPACITY;
    debug_assert!(build.pos.len() <= cap_verts, "vertex budget exceeded");
    debug_assert!(build.index.len() <= cap_indices, "index budget exceeded");
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
//
// `bullets.js` keeps `self`/`ally`/`enemy` pairs. `ProjView` carries no owner,
// so there is no way to choose between them here and everything draws `self`.
// See the module docs.

/// `bullets.js` `self.coreColor`.
static BOLT_CORE: LinearRgba = LinearRgba::rgb(6.16, 7.49, 5.92);
/// `bullets.js` `self.haloColor`.
static BOLT_HALO: LinearRgba = LinearRgba::rgb(0.14, 2.55, 1.11);
/// `missiles.js` fuselage `0xd4dce8`, kept near 1.0 so the body reads as a
/// lit object rather than a light source.
static MISSILE_BODY: LinearRgba = LinearRgba::rgb(1.09, 1.20, 1.36);
/// `missiles.js` nozzle glow `0xff9900`.
static MISSILE_NOZZLE: LinearRgba = LinearRgba::rgb(5.10, 1.30, 0.0);
/// `missiles.js` flare core `0xffffff`.
static FLARE_CORE: LinearRgba = LinearRgba::rgb(5.10, 5.10, 5.10);
/// `missiles.js` flare glow `0xffcc22`.
static FLARE_GLOW: LinearRgba = LinearRgba::rgb(3.40, 2.02, 0.11);

// ---------------------------------------------------------------------------
// The stress harness
// ---------------------------------------------------------------------------

/// Synthetic projectiles, for measuring against a busy scene.
///
/// `sim_bridge::tick` does not call `sim::bullets` or `sim::missiles` yet, so
/// `Frame::bullets`, `::missiles`, and `::flares` are empty in this build and
/// the renderer above has nothing to draw. Rather than fake data inside the
/// simulation — which would put render scaffolding on the wrong side of the
/// boundary — these records are synthesised here and *chained* onto the frame's
/// own slices at every read site. When the projectile phases land in the
/// bridge, this becomes dead weight and deletes cleanly; nothing else in the
/// module knows it exists.
#[derive(Default)]
struct Demo {
    bullets: Vec<ProjView>,
    missiles: Vec<ProjView>,
    flares: Vec<FlareView>,
    /// How many bullets to hold in flight. Zero disables the whole harness.
    count: usize,
    started: bool,
}

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
        match kind {
            ExplosionKind::MissileHit => push_shells(&mut fx.shells, p, 1.0, MISSILE_HIT_SHELLS),
            ExplosionKind::FlareBurst => push_shells(&mut fx.shells, p, 1.0, FLARE_BURST_SHELLS),
            _ => fx.shells.push(Shell {
                pos: p,
                age: 0.0,
                life: 0.55,
                from: scale * 0.4,
                to: scale * 2.6,
                color: glow(0xffaa55, 1.8),
                opacity: 0.95,
            }),
        }

        fx.beams.push(BeamFx {
            start: anchor + Vec3::new(a.sin() * 12.0, -4.0, a.cos() * 12.0),
            end: p,
            age: 0.0,
            color: glow(0x88ffd6, 1.6),
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
        let bullets: Vec<ProjView> = (0..250)
            .map(|i| ProjView {
                key: i,
                pos: [i as f32, 0.0, 0.0],
                dir: [0.0, 0.0, 1.0],
            })
            .collect();

        for b in &bullets {
            let dir = Vec3::from_array(b.dir);
            let center = Vec3::from_array(b.pos);
            build.streak(
                center,
                dir,
                BOLT_HALO_LEN * 0.5,
                BOLT_HALO_HALF_W,
                Vec3::NEG_Z,
                Brush::GLOW,
                BOLT_HALO,
                0.55,
            );
            build.streak(
                center,
                dir,
                BOLT_LEN * 0.5,
                BOLT_CORE_HALF_W,
                Vec3::NEG_Z,
                Brush::CORE,
                BOLT_CORE,
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
            BOLT_CORE,
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

        // Every one of them sits at a nozzle, not at the hull origin.
        for m in &fx.motes {
            let d = trail_offsets()
                .iter()
                .map(|o| m.pos.distance(*o))
                .fold(f32::INFINITY, f32::min);
            assert!(
                d < EMIT_BOOST.jitter * 2.0,
                "mote at {:?} is {d} from the nearest nozzle",
                m.pos
            );
            assert!(m.half > 0.0 && m.life > 0.0);
        }
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
        let debt = fx.trail_debt.get(&1).copied().unwrap();
        let tick = sim::world::TICK_DT as f32;
        assert!(
            (debt - EMIT_BRAKE.rate * tick).abs() < 1e-4,
            "brake rate should win over boost: {debt}"
        );

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
                    age: 0.0,
                    life: 1.0,
                    half: 0.2,
                    grow: 0.0,
                    shrink: 0.45,
                    color: BOLT_CORE,
                    opacity: 0.85,
                },
            );
        }
        assert_eq!(motes.len(), MAX_MOTES);
        // The survivors are the newest ones.
        assert_eq!(motes[0].pos.x, 40.0);
    }

    /// The palette constants are hand-resolved from the JS hexes, so a test
    /// has to be what keeps them honest.
    #[test]
    fn the_palette_matches_the_js_hexes() {
        let close = |a: f32, b: f32, what: &str| {
            assert!((a - b).abs() < 0.02, "{what}: {a} vs {b}");
        };
        // `bullets.js` self core 0xeaffe6 at intensity 4.4.
        let core = glow(0xeaffe6, 4.4);
        close(core.red, BOLT_CORE.red, "core red");
        close(core.green, BOLT_CORE.green, "core green");
        close(core.blue, BOLT_CORE.blue, "core blue");

        // Every effect colour has to clear the bloom prefilter threshold of
        // 0.9 on at least one channel, or the Ultra look is not being hit.
        for (name, c) in [
            ("bolt core", BOLT_CORE),
            ("bolt halo", BOLT_HALO),
            ("missile nozzle", MISSILE_NOZZLE),
            ("flare core", FLARE_CORE),
            ("flare glow", FLARE_GLOW),
        ] {
            let peak = c.red.max(c.green).max(c.blue);
            assert!(peak > 0.9, "{name} peaks at {peak}, below the bloom floor");
        }
    }
}
