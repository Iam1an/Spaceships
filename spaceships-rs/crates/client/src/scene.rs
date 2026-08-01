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

/// Marks the root entity of a ship. The glTF model hangs off this as a child;
/// this entity's transform is the simulation's, unmodified.
///
/// The id is carried on the component as well as in [`Registry`] so that a
/// system which does not have the registry — a trail emitter, a nameplate, the
/// HUD's lock-on marker — can still tell which ship it is looking at.
#[derive(Component)]
#[expect(dead_code, reason = "read by systems this slice does not have yet")]
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
            // `Update` after the fixed step, so a frame that ran a tick renders
            // that tick rather than the previous one.
            //
            // TODO(interpolation): at 60 Hz sim and a 144 Hz display this snaps
            // to the last tick, which shows as judder. The fix is the standard
            // one — keep the previous transform and lerp by
            // `Time<Fixed>::overstep_fraction()` in `RunFixedMainLoop`'s
            // `AfterFixedMainLoop` set. Left out here because it is a
            // smoothness change, not a pipeline one.
            .add_systems(Update, (sync_ships, sync_rocks).after(SimSet));
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
// Per-frame sync
// ---------------------------------------------------------------------------

fn sync_ships(
    mut commands: Commands,
    frame: Res<SimFrame>,
    scene: Res<SceneAssets>,
    mut reg: ResMut<Registry>,
    mut q: Query<(&mut Transform, &mut Visibility), With<ShipRoot>>,
) {
    for view in &frame.0.ships {
        // Boss hitboxes are never drawn — they exist so one damage path can
        // serve the capital ship too.
        if view.flags.contains(sim::world::ShipFlags::BOSS_HITBOX) {
            continue;
        }

        let entity = *reg.ships.entry(view.id).or_insert_with(|| {
            commands
                .spawn((
                    ShipRoot(view.id),
                    Transform::from_translation(pos(view.pos)).with_rotation(rot(view.quat)),
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

        if let Ok((mut tf, mut vis)) = q.get_mut(entity) {
            tf.translation = pos(view.pos);
            tf.rotation = rot(view.quat);
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

fn sync_rocks(
    mut commands: Commands,
    frame: Res<SimFrame>,
    scene: Res<SceneAssets>,
    mut reg: ResMut<Registry>,
    mut q: Query<&mut Transform, With<Rock>>,
) {
    for view in &frame.0.asteroids {
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
                    Transform::default(),
                ))
                .id()
        });

        if let Ok(mut tf) = q.get_mut(entity) {
            tf.translation = pos(view.pos);
            // The rock meshes are unit-radius, so `size` is the scale directly.
            tf.scale = Vec3::splat(view.size);
            tf.rotation = Quat::from_euler(EulerRot::XYZ, view.rot[0], view.rot[1], view.rot[2]);
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
