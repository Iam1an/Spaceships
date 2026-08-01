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

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::light::{CascadeShadowConfigBuilder, NotShadowCaster};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::world_serialization::{WorldAsset, WorldAssetRoot, WorldInstanceReady};
use std::f32::consts::FRAC_PI_2;

use spaceships_sim as sim;

use crate::sim_bridge::{pos, rot, SimFrame, SimSet};

/// The player model, at the asset root. `spaceshipADMIN.glb` is the other one
/// and is 4.9 MB, which is a decision for later, not a default.
const SHIP_MODEL: &str = "spaceship.glb";

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

/// Handles the sync systems need but must not reload every frame.
#[derive(Resource)]
struct SceneAssets {
    /// The glTF scene inside `spaceship.glb`.
    ship: Handle<WorldAsset>,
    /// Six deformed icospheres, matching `asteroids.js`'s six variants.
    rock_meshes: Vec<Handle<Mesh>>,
    rock_material: Handle<StandardMaterial>,
}

pub struct ScenePlugin;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Registry>()
            .add_systems(Startup, setup)
            // Sampling is per *tick*. `FixedUpdate`, after `SimSet`, is the
            // only place `prev` and `curr` are guaranteed to be consecutive
            // ticks — see the module docs on why sampling in `Update` freezes
            // on a frame that ran no tick and lurches on one that ran two.
            .add_systems(FixedUpdate, (sample_ships, sample_rocks).after(SimSet))
            // Drawing is per *frame*. `AfterFixedMainLoop` is where Bevy's own
            // docs put this: the fixed loop has finished, so
            // `overstep_fraction` is the leftover accumulator and nothing else
            // will consume it, and it still lands before `Update` and
            // `PostUpdate` — which is to say before the chase camera reads the
            // scene and before `TransformSystems::Propagate`.
            .add_systems(
                RunFixedMainLoop,
                draw_interpolated.in_set(RunFixedMainLoopSystems::AfterFixedMainLoop),
            );
    }
}

// ---------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------

fn setup(
    mut commands: Commands,
    assets: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let rules = sim::rules::Rules::DEFAULT;

    install_space_lights(&mut commands, &rules);

    // -- The moon -------------------------------------------------------------
    // `World::new` already put the collision sphere at the origin; this is only
    // its mesh. Radius comes from the rules so the two can never disagree.
    let moon_r = rules.world.moon_radius as f32;
    commands.spawn((
        Mesh3d(meshes.add(Sphere::new(moon_r).mesh().uv(96, 48))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color_texture: Some(sharp_texture(&assets, "moon Texture.jpg")),
            perceptual_roughness: 0.95,
            metallic: 0.0,
            ..default()
        })),
        Transform::from_xyz(
            rules.world.moon_pos.x as f32,
            rules.world.moon_pos.y as f32,
            rules.world.moon_pos.z as f32,
        ),
        // `graphics.js`: "Big background props stay out of the shadow pass."
        NotShadowCaster,
    ));

    // -- Motherships ----------------------------------------------------------
    // Placeholder boxes at the two spawn hulls, straight off `World::boxes`, so
    // that flying into one visibly bounces — `resolve_world_collisions` already
    // does the physics.
    //
    // TODO(art): the JS has real mothership geometry. These are the collision
    // volumes, drawn.
    let hull = materials.add(StandardMaterial {
        base_color: Color::srgb(0.20, 0.24, 0.30),
        // Ultra's hull treatment: "authored flat-matte; a little metal and a
        // tight roughness is what gives them specular highlights".
        perceptual_roughness: 0.34,
        metallic: 0.55,
        ..default()
    });
    for z in [-rules.world.mothership_z, rules.world.mothership_z] {
        let h = rules.world.mothership_half;
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(
                h.x as f32 * 2.0,
                h.y as f32 * 2.0,
                h.z as f32 * 2.0,
            ))),
            MeshMaterial3d(hull.clone()),
            Transform::from_xyz(0.0, 0.0, z as f32),
        ));
    }

    // -- Shared handles -------------------------------------------------------
    commands.insert_resource(SceneAssets {
        // 0.19: `GltfAssetLabel::Scene(0).from_asset(..)` is unchanged, but the
        // asset it resolves to is a `WorldAsset` (was `Scene`) and the
        // component that instantiates it is `WorldAssetRoot` (was `SceneRoot`).
        ship: assets.load(bevy::gltf::GltfAssetLabel::Scene(0).from_asset(SHIP_MODEL)),
        rock_meshes: (0..rules.world.asteroid_field.variant_count)
            .map(|v| meshes.add(rock_mesh(v)))
            .collect(),
        rock_material: materials.add(StandardMaterial {
            // `asteroids.js` maps `sounds/asteroid.jpg` with a 1.5x repeat.
            base_color_texture: Some(sharp_texture(&assets, "sounds/asteroid.jpg")),
            perceptual_roughness: 0.95,
            metallic: 0.05,
            ..default()
        }),
    });
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
fn install_space_lights(commands: &mut Commands, rules: &sim::rules::Rules) {
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
    ));

    // `HemisphereLight(0x9fb4d0, 0x2b2f3a, 0.55)`. Bevy has no hemisphere
    // light; the sky/ground split is what an environment map does, and
    // `skybox.rs` installs a `GeneratedEnvironmentMapLight` from the nebula
    // cubemap — which is exactly what `applyEnvironment`'s PMREM pass does in
    // the JS. This ambient is the neutral lift underneath it.
    commands.insert_resource(GlobalAmbientLight {
        color: Color::srgb_u8(0x9f, 0xb4, 0xd0),
        brightness: 120.0,
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
    mut reg: ResMut<Registry>,
    mut q: Query<(&mut Interp, &mut Visibility, Has<Snap>), With<ShipRoot>>,
) {
    for view in &frame.0.ships {
        // Boss hitboxes are never drawn — they exist so one damage path can
        // serve the capital ship too.
        if view.flags.contains(sim::world::ShipFlags::BOSS_HITBOX) {
            continue;
        }

        let pose = Pose::of_ship(view);

        let entity = *reg.ships.entry(view.id).or_insert_with(|| {
            commands
                .spawn((
                    ShipRoot(view.id),
                    pose.transform(),
                    // First tick: no previous pose, so both ends are this one.
                    Interp::spawned(pose),
                    Visibility::default(),
                ))
                .with_child((
                    WorldAssetRoot(scene.ship.clone()),
                    // `ship.js:45` does exactly this: the model's nose rests
                    // along +x (its `gun` node is at x = 3.81), and the
                    // simulation says the nose is local +z. Correcting it on a
                    // child keeps the root transform the simulation's own.
                    Transform::from_rotation(Quat::from_rotation_y(-FRAC_PI_2)),
                ))
                // Ultra's per-ship material treatment, applied once the glTF
                // hierarchy actually exists.
                .observe(ultra_material_sweep)
                .id()
        });

        // Miss on the tick the entity was spawned — `Interp::spawned` above
        // already holds this pose, and the commands have not been applied yet.
        if let Ok((mut interp, mut vis, marked)) = q.get_mut(entity) {
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
        }

        // TODO(team colours): `applyColorsToShip` in `ship.js` recolours hull
        // vs. accent meshes per team, picking accents by name
        // (cockpit/engine/window/glass) or by luma < 0.35. The walk is already
        // written in `ultra_material_sweep`; it needs `GltfMaterialName` and
        // the team from `view.team`.
        // TODO(trails): `BOOSTING`/`BRAKING` are already in `view.flags` and are
        // what `trails.js` emits from. See the module docs on batching before
        // adding a mesh per trail segment.
        // TODO(hit flash): `view.hit_flash` should drive an emissive pulse,
        // which under Ultra is what the bloom pass catches.
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

/// Ultra's `upgradeMaterials`, for the ship: metal hull, tight roughness, and
/// anisotropic sampling on whatever maps the glTF brought with it.
///
/// Fires on [`WorldInstanceReady`], because until the glTF has been
/// instantiated there is no hierarchy to walk. This mutates the shared
/// `StandardMaterial` assets rather than cloning per entity, which is what the
/// JS does too — `_upgraded` is a `WeakSet` guarding exactly that.
fn ultra_material_sweep(
    ready: On<WorldInstanceReady>,
    children: Query<&Children>,
    meshes: Query<&MeshMaterial3d<StandardMaterial>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    for descendant in children.iter_descendants(ready.entity) {
        let Ok(MeshMaterial3d(handle)) = meshes.get(descendant) else {
            continue;
        };
        let Some(mut mat) = materials.get_mut(handle) else {
            continue;
        };

        // `sweepScene`: ships get `metalness: 0.55, roughness: 0.34`.
        mat.metallic = 0.55;
        mat.perceptual_roughness = 0.34;

        // `emissiveIntensity *= glowBoost` (1.7) — pushing emissive past 1.0 is
        // what makes the bloom pass bite.
        mat.emissive *= 1.7;

        // The anisotropy half of `upgradeMaterials`. Textures inside the glTF
        // were loaded by the gltf loader with its own sampler, so they are
        // patched here rather than at load time.
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
    }
}

fn sample_rocks(
    mut commands: Commands,
    frame: Res<SimFrame>,
    scene: Res<SceneAssets>,
    mut reg: ResMut<Registry>,
    mut q: Query<(&mut Interp, Has<Snap>), With<Rock>>,
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
                ))
                .id()
        });

        if let Ok((mut interp, marked)) = q.get_mut(entity) {
            if marked {
                commands.entity(entity).remove::<Snap>();
                interp.snap(pose);
            } else {
                interp.advance(pose);
            }
        }

        // TODO(damage tint): `view.hp` and `view.hit_flash` drive the flash in
        // `asteroids.js:101`. A per-rock material would defeat the shared-handle
        // batching below; the right shape is a material extension with a
        // per-instance uniform, or packing the flash into a vertex colour.
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
// Asteroid meshes
// ---------------------------------------------------------------------------

/// One deformed unit icosphere, matching `asteroids.js:29` (`buildVariants`).
///
/// Two octaves: a big lobe that makes the rock lumpy and a fine bump that
/// roughens the surface. Flat normals afterwards, for the faceted look the JS
/// gets from `flatShading: true`.
///
/// Six of these are built once and shared by every rock, so the whole field is
/// six mesh handles and one material — see the batching note in
/// [`crate`]'s docs.
///
/// Uses `sim::rng::Rng` rather than a `rand` dependency: it is already in the
/// graph, and seeding it per variant keeps the six shapes reproducible.
fn rock_mesh(variant: u32) -> Mesh {
    let mut mesh = Sphere::new(1.0)
        .mesh()
        .ico(2)
        .expect("subdivision 2 is well within the icosphere limit");

    let offsets = noise_offsets(variant);

    if let Some(bevy::mesh::VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
    {
        for p in positions.iter_mut() {
            let [x, y, z] = *p;
            let lobe = 0.78 + 0.34 * value_noise(x * 1.3, y * 1.3, z * 1.3, &offsets);
            let bump = 0.94 + 0.12 * value_noise(x * 4.1, y * 4.1, z * 4.1, &offsets);
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

/// Per-variant phase offsets, so the six rocks are different shapes rather than
/// six rotations of one shape.
fn noise_offsets(variant: u32) -> [f32; 6] {
    let mut rng = sim::rng::Rng::with_stream(0xA57E_401D, u64::from(variant));
    let mut out = [0.0f32; 6];
    for o in &mut out {
        *o = (rng.next_f64() * 100.0) as f32;
    }
    out
}

/// Cheap smooth pseudo-noise in `-1..1`. Not a real gradient noise — three
/// summed sine products, which is what `asteroids.js`'s `pseudoNoise` amounts
/// to and is plenty for displacing 162 vertices six times at startup.
fn value_noise(x: f32, y: f32, z: f32, o: &[f32; 6]) -> f32 {
    let a = (x * 1.7 + o[0]).sin() * (y * 1.3 + o[1]).cos();
    let b = (y * 2.1 + o[2]).sin() * (z * 1.9 + o[3]).cos();
    let c = (z * 1.5 + o[4]).sin() * (x * 2.3 + o[5]).cos();
    (a + b + c) / 3.0
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
