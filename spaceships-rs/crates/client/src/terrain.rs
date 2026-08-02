//! The Sierras map: ground, airfields, trees, clouds, fog, and its own sun.
//!
//! Ports `public/src/terrain.js`, `trees.js`, `clouds.js` and `airfield.js`,
//! plus the terrain half of `main.js:103`–`:182` that wires them together and
//! swaps the lighting rig.
//!
//! # The one rule this module exists to keep
//!
//! **The ground you see is generated from [`sim::ship::terrain_height`], never
//! from a second copy of the noise.** [`height`] is a one-line forward to the
//! simulation's own function and is the *only* way anything here learns where
//! the surface is — the mesh, the tree placement, and the tests all go through
//! it. That is not tidiness: the sim kills a ship below
//! `terrain_height + `[`WorldRules::terrain_kill_clearance`], so a renderer
//! with its own heightfield produces ships sinking into hillsides and players
//! dying to invisible geometry, which is strictly worse than drawing no ground
//! at all. `terrain_height_matches_the_simulation` pins the forward.
//!
//! # Level of detail: what was measured, and what was chosen
//!
//! `terrain.js:42` draws one `PlaneGeometry(3600, 3600, 96, 96)` — 37.5-unit
//! cells — and that is too coarse *here* for a reason the JS never had to face.
//! Both halves of the JS read `getTerrainHeight` and only ever at the ship's own
//! point, so nothing compared the drawn surface to the sampled one. A
//! triangulated heightfield agrees with the function it was sampled from **only
//! at its vertices**; in between it is a chord.
//!
//! ## The metric is perpendicular distance, not height
//!
//! The obvious measure — how far the mesh's `y` is from
//! [`sim::ship::terrain_height`] at the same `(x, z)` — badly overstates the
//! problem, and `survey_the_error_against_segment_count` shows why. The largest
//! vertical disagreements are all on the near-vertical rim where `airfield_blend`
//! ramps the flattened apron back up into the surrounding hills: the gradient
//! there reaches about 5, so 20 units of *vertical* error is four units of
//! sideways miss against a cliff face. What a player sees, and what decides
//! whether a ship dies at the visible surface, is the gap **normal** to the
//! ground, which is the vertical error divided by `sqrt(1 + |grad h|^2)`.
//!
//! Measured over 200,000 points, normal gap in units:
//!
//! | segments | cell | mean | p99 | p99.9 | max |
//! |---|---|---|---|---|---|
//! | 96 (`terrain.js`) | 37.50 | 0.79 | 4.64 | 10.72 | 23.53 |
//! | 192 | 18.75 | 0.21 | 1.58 | 4.02 | 12.37 |
//! | 256 | 14.06 | 0.12 | 0.99 | 2.56 | 6.66 |
//! | **384** | **9.375** | **0.05** | **0.53** | **1.25** | **3.68** |
//!
//! The budget is [`WorldRules::terrain_kill_clearance`], 5 units: at the point
//! the mesh is that far off the surface, a ship dies at the drawn ground rather
//! than above it. [`GROUND_SEGMENTS`] is therefore **384** — the first power-of-
//! two multiple of the JS's own 96 whose *worst* case clears the budget, not
//! just its average. `the_drawn_surface_stays_inside_the_kill_clearance` asserts
//! it, and `the_javascripts_own_resolution_would_not_have_cleared_it` asserts
//! that the JS's number does not, so neither claim rots.
//!
//! ## Why one uniform mesh and not a distance LOD
//!
//! 384 segments is 294,912 triangles in **one** draw call and one 9 MB upload
//! per match. It is deliberately not split by distance:
//!
//! - **Chunked LOD** (tiles, each with a mesh per level) would take the triangle
//!   count down and the draw call count *up*, from 1 to one per tile. `main.rs`
//!   says at length why this codebase counts draw calls and not triangles — the
//!   JS client's problem was 477 of them — and a hundred-odd batch keys to save
//!   200k triangles is the wrong side of that trade.
//! - **A clipmap** (rings that follow the player) keeps the draw calls low but
//!   re-uploads vertex buffers every time the player crosses a cell, roughly
//!   once a second at cruise. That is the pattern commit "Stop the per-frame GPU
//!   use-after-free in the effects and warp meshes" was written to get rid of.
//! - **Refining only where the error is** — the two airfield rims — sounds like
//!   the surgical answer and is not. Keeping the grid a tensor product, which is
//!   what keeps it crack-free, means a refined band in x adds columns spanning
//!   all of z and vice versa: refining both aprons 4x costs ~175k vertices
//!   against 384's 148k. Separate patch meshes escape that and buy a seam.
//!
//! And all three would be paying for something this map does not have. The fog
//! is total at [`FOG_END`] = 4800 and the terrain is 3600 across, so **the whole
//! map is inside view distance from anywhere on it**; there is no far field to
//! throw away. A static mesh, built once per match, is both the cheap answer and
//! the safe one.
//!
//! [`WorldRules::terrain_kill_clearance`]: sim::rules::WorldRules::terrain_kill_clearance
//!
//! # Draw calls
//!
//! Everything here shares handles the way `scene.rs`'s asteroid field does, so
//! `SPACESHIPS_BATCHES=1` counts shapes and not entities:
//!
//! | Prop | Entities | Batch keys |
//! |---|---|---|
//! | Ground + horizon skirt | 2 | 2 |
//! | Trees (`trees.js`, 340 of them) | 680 | 2 |
//! | Clouds (`clouds.js`, 26 clusters) | ~220 | 1 |
//! | Airfields (`airfield.js`, two) | ~130 | ~22 |
//!
//! # Swapping maps
//!
//! [`apply_map`] is the whole lifecycle. It early-outs on an unchanged map, so
//! it costs one enum compare a frame; when the map *does* change it despawns
//! everything tagged [`MapScenery`] — this module's props and whichever
//! lighting rig is installed — hides or shows `scene.rs`'s space props, and
//! rebuilds. That is what makes the lobby's Space/Sierras toggle work without
//! `scene.rs` having to know the terrain exists.

use bevy::asset::RenderAssetUsages;
use bevy::image::Image;
use bevy::light::{
    CascadeShadowConfigBuilder, GeneratedEnvironmentMapLight, NotShadowCaster, Skybox,
};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};
use std::f32::consts::{FRAC_PI_3, PI};

use spaceships_sim as sim;

use crate::camera::FlightCamera;
use crate::scene::{glow, hex, install_space_lights, MapScenery, SpaceScenery, GLOW_BOOST};
use crate::sim_bridge::MatchSetup;
use crate::skybox::NebulaCubemap;
use sim::rules::Rules;
use sim::world::MapKind;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Quads per side of the ground mesh — 9.375-unit cells over the 3,600-unit
/// map.
///
/// `terrain.js:3` uses 96. Four times that is where the *worst* disagreement
/// between the drawn surface and [`sim::ship::terrain_height`] first fits
/// inside the kill clearance; the module docs carry the measurements and the
/// three LOD schemes that were rejected in favour of one uniform mesh.
const GROUND_SEGMENTS: u32 = 384;

/// `main.js:122` — `new THREE.Fog(0xbbd5f0, 1400, 4800)`.
const FOG_COLOR: u32 = 0x00bb_d5f0;
/// Distance at which the fog starts to bite. `main.js:122`.
const FOG_START: f32 = 1400.0;
/// Distance at which the fog is total. `main.js:122`. Also how far the horizon
/// skirt reaches, so the map edge is never a visible cliff into the sky.
const FOG_END: f32 = 4800.0;

/// `main.js:121` — `scene.background = new THREE.Color(0x6fa8d4)`, and the sky
/// half of `applySkyEnvironment` at `main.js:124`.
const SKY_COLOR: u32 = 0x006f_a8d4;
/// The ground half of `applySkyEnvironment(scene, renderer, 0x6fa8d4, 0x4a4335)`.
/// `main.js:124`.
const SKY_GROUND_COLOR: u32 = 0x004a_4335;

/// Sun illuminance, in lux.
///
/// `main.js:109` builds the sun at three.js intensity `1.4` and
/// `upgradeTerrainSun` (`graphics.js:474`) raises it to `2.2` for Ultra, which
/// is the path this client targets. Three.js intensity is a unitless multiplier
/// and Bevy's is lux, so only ratios carry over and the absolute has to be
/// anchored somewhere.
///
/// **Not anchored where `scene.rs` anchors the space key.** Carrying that
/// across (intensity 2.7 at 9,000 lux, so 7,333 here) puts the snow band at
/// twice full white before tonemapping and the whole upper half of the map
/// clips to a flat sheet — checked on screen. The reason the same anchor works
/// in space and not here is albedo: rock and hull sit near 0.3, while
/// `elevation_color`'s snow is 0.96, and a sun sized for the first blows out the
/// second. So this is set from the albedo instead — `albedo * lux / pi`, at
/// Bevy's default EV100 of 9.7, landing just under 1.0 for the brightest band
/// the map contains.
const SUN_LUX: f32 = 3_300.0;

/// How far above the player the sun sits. `main.js:1713`.
///
/// It looks **straight down**, which is a real consequence and not an
/// approximation: `main.js:1711` copies the ship's position into the light's
/// target and `:1713` puts the light directly over it. Every vertical face on
/// the map therefore takes `N · L = 0` and is lit only by the ambient and the
/// sky environment, and relief on the hills reads from the baked elevation
/// colours far more than from the shading. Keeping it is parity; the fix, if
/// anyone wants one, is a fixed sun angle and it belongs in the JS too.
const SUN_HEIGHT: f32 = 500.0;

/// Ambient brightness.
///
/// `main.js:108` is `AmbientLight(0xfff8e8, 0.28)` under Ultra, low because
/// Ultra also installs `applySkyEnvironment` — see [`sky_ground_cubemap`] — and
/// the sky is expected to do the filling.
///
/// Raised well above the ratio `scene.rs` uses for space, for the reason above:
/// with the sun straight down, the ambient plus the sky probe is the *only*
/// thing lighting a cliff face, a hangar wall or a tree's flank, and at the
/// space rig's proportion those all render black. This is what puts an
/// unlit vertical at roughly a quarter of a sunlit horizontal, which is about
/// what an overcast-free sky does outdoors.
const AMBIENT_BRIGHTNESS: f32 = 900.0;

/// Seed for every placement decision on this map — trees and clouds.
///
/// `trees.js` and `clouds.js` both call `Math.random`, so the JS reshuffles the
/// forest on every load. A fixed seed is the house rule (`sim::rng`) and it is
/// also what makes a screenshot comparable to the last one.
const SCATTER_SEED: u64 = 0x5133_2A55;

// ---------------------------------------------------------------------------
// The height function
// ---------------------------------------------------------------------------

/// Surface height at a world `(x, z)` — **the** sampler, and a plain forward to
/// the simulation's own.
///
/// Everything visible on this map is placed through this function, and it is
/// deliberately trivial: the moment the renderer grows its own copy of the
/// noise, the ground stops agreeing with the thing that kills you. See the
/// module docs.
#[must_use]
pub fn height(x: f64, z: f64) -> f64 {
    sim::ship::terrain_height(x, z, &Rules::DEFAULT)
}

// ---------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------

/// A cloud cluster's drift, in units per second along x. `clouds.js:5`
/// (`DRIFT_SPEED`) times the per-cluster direction and rate at `clouds.js:22`.
#[derive(Component)]
struct CloudDrift(f32);

/// The sun that follows the player. `main.js:1710`.
#[derive(Component)]
struct TerrainSun;

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

pub struct TerrainPlugin;

impl Plugin for TerrainPlugin {
    fn build(&self, app: &mut App) {
        // `Update`, not `Startup`: the map is not known until `MatchSetup` is,
        // and the lobby can change it at any time. `apply_map` is written to be
        // run every frame and do nothing.
        app.add_systems(Update, (apply_map, drift_clouds, follow_sun));
    }
}

/// Installs or tears down the whole map when [`MatchSetup::map`] changes.
///
/// Keyed on a `Local` rather than on `Res::is_changed`, because the resource is
/// also written by things that do not touch the map (the callsign, the seed)
/// and rebuilding a 66,000-vertex mesh because someone renamed themselves would
/// be a visible hitch for no reason. The first run always fires, since `None`
/// never equals a map.
#[allow(clippy::too_many_arguments)]
fn apply_map(
    mut commands: Commands,
    setup: Res<MatchSetup>,
    mut built: Local<Option<MapKind>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut clear: ResMut<ClearColor>,
    nebula: Option<Res<NebulaCubemap>>,
    old: Query<Entity, With<MapScenery>>,
    mut space_props: Query<&mut Visibility, With<SpaceScenery>>,
    camera: Query<Entity, With<FlightCamera>>,
) {
    if *built == Some(setup.map) {
        return;
    }
    // No camera means `spawn_camera` has not run, and half of what follows is a
    // component on it. Leaving `built` alone retries next frame.
    let Ok(cam) = camera.single() else {
        return;
    };
    *built = Some(setup.map);

    for entity in &old {
        commands.entity(entity).despawn();
    }

    let terrain = setup.map == MapKind::Terrain;
    for mut vis in &mut space_props {
        *vis = if terrain {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        };
    }

    let rules = Rules::DEFAULT;
    if terrain {
        install_terrain_lights(&mut commands);
        install_ground(&mut commands, &mut meshes, &mut materials, &rules);
        install_airfields(&mut commands, &mut meshes, &mut materials, &rules);
        install_trees(&mut commands, &mut meshes, &mut materials, &rules);
        install_clouds(&mut commands, &mut meshes, &mut materials);

        // `main.js:121`–`:124`: a flat blue sky instead of the starfield, a
        // linear fog, and a sky/ground gradient standing in for the nebula
        // environment so hulls reflect daylight rather than deep space.
        let sky = images.add(sky_ground_cubemap());
        clear.0 = hex(SKY_COLOR).into();
        commands.entity(cam).remove::<Skybox>().insert((
            DistanceFog {
                color: hex(FOG_COLOR).into(),
                falloff: FogFalloff::Linear {
                    start: FOG_START,
                    end: FOG_END,
                },
                ..default()
            },
            GeneratedEnvironmentMapLight {
                environment_map: sky,
                intensity: 900.0,
                ..default()
            },
        ));
    } else {
        install_space_lights(&mut commands, &rules);
        clear.0 = Color::srgb(0.012, 0.016, 0.032);
        commands.entity(cam).remove::<DistanceFog>();
        if let Some(nebula) = nebula {
            commands.entity(cam).insert((
                Skybox {
                    image: Some(nebula.0.clone()),
                    brightness: 3000.0,
                    ..default()
                },
                GeneratedEnvironmentMapLight {
                    environment_map: nebula.0.clone(),
                    intensity: 900.0,
                    ..default()
                },
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Lighting
// ---------------------------------------------------------------------------

/// `main.js:106`–`:123` plus `upgradeTerrainSun` (`graphics.js:472`).
///
/// One directional light and an ambient, and that is the whole rig — there is
/// no fill and no rim, because outdoors at midday there is no reason for one.
/// The sun is the only shadow caster in the game: `installSpaceLights` never
/// sets `castShadow`, and this is the light `upgradeTerrainSun` hands a
/// 2048-pixel map.
fn install_terrain_lights(commands: &mut Commands) {
    commands.spawn((
        DirectionalLight {
            // `upgradeTerrainSun` overrides `main.js:109`'s `0xfff5cc`.
            color: hex(0x00ff_f0d0).into(),
            illuminance: SUN_LUX,
            shadow_maps_enabled: true,
            // `shadow.normalBias = 0.6` against Bevy's default 1.8. The JS
            // number is in world units and so is Bevy's, which is the one place
            // the two shadow configurations are directly comparable.
            shadow_normal_bias: 0.6,
            ..default()
        },
        // Straight down, from 500 units up — `main.js:110`, and reasserted
        // every frame by `follow_sun`. `look_at` needs an up vector that is not
        // parallel to the view direction, hence +Z rather than the usual +Y.
        Transform::from_xyz(0.0, SUN_HEIGHT, 0.0).looking_at(Vec3::ZERO, Vec3::Z),
        // `shadow.camera.far = 700` with a ±150 frustum (`main.js:113`–`:118`),
        // which is a tight box around the player rather than a map-wide map.
        // Only ships cast into it — everything this module spawns is a
        // `NotShadowCaster`, exactly as the JS leaves `castShadow` false on the
        // terrain, the trees and the airfields.
        CascadeShadowConfigBuilder {
            num_cascades: 2,
            maximum_distance: 700.0,
            first_cascade_far_bound: 150.0,
            ..default()
        }
        .build(),
        TerrainSun,
        MapScenery,
    ));

    commands.insert_resource(GlobalAmbientLight {
        color: hex(0x00ff_f8e8).into(),
        brightness: AMBIENT_BRIGHTNESS,
        ..default()
    });
}

/// `main.js:1710`–`:1713`: the sun rides above the player so its tight shadow
/// frustum is always over the ship.
///
/// Reads the *interpolated* transform for the same reason `camera.rs` does — a
/// shadow map snapping at 60 Hz under a 144 Hz display shimmers.
fn follow_sun(
    ships: Query<(&crate::scene::ShipRoot, &Transform), Without<TerrainSun>>,
    mut sun: Query<&mut Transform, With<TerrainSun>>,
) {
    let Some((_, me)) = ships.iter().find(|(root, _)| root.0 == crate::LOCAL_ID) else {
        return;
    };
    let Ok(mut tf) = sun.single_mut() else {
        return;
    };
    let at = me.translation;
    *tf = Transform::from_translation(at + Vec3::Y * SUN_HEIGHT).looking_at(at, Vec3::Z);
}

// ---------------------------------------------------------------------------
// Ground
// ---------------------------------------------------------------------------

/// The heightfield, plus the flat skirt that carries the horizon out into the
/// fog.
fn install_ground(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    rules: &Rules,
) {
    // `terrain.js:76`. `vertexColors: true` with no map: the elevation bands
    // are baked into the mesh, which is what makes one material enough for the
    // whole 3,600-unit map.
    let material = materials.add(StandardMaterial {
        perceptual_roughness: 0.92,
        metallic: 0.0,
        ..default()
    });

    commands.spawn((
        Mesh3d(meshes.add(ground_mesh(rules, GROUND_SEGMENTS))),
        MeshMaterial3d(material.clone()),
        // `main.js:176` sets `receiveShadow` and never `castShadow`. Receiving
        // is Bevy's default; not casting has to be asked for, and it matters —
        // a 295k-triangle mesh in the shadow pass would double the map's cost
        // to draw the ground's shadow onto itself.
        NotShadowCaster,
        MapScenery,
    ));

    commands.spawn((
        Mesh3d(meshes.add(skirt_mesh(rules, GROUND_SEGMENTS))),
        MeshMaterial3d(material),
        NotShadowCaster,
        MapScenery,
    ));
}

/// One `PlaneGeometry(TERRAIN_SIZE, TERRAIN_SIZE, segs, segs)` displaced by
/// [`height`]. `terrain.js:42`.
///
/// Sampled in `f64` and stored in `f32`: [`sim::ship::terrain_height`] is `f64`
/// throughout and rounding once, at the end, is both closer to the function and
/// the only rounding the test has to account for.
fn ground_mesh(rules: &Rules, segs: u32) -> Mesh {
    let size = rules.world.terrain_size;
    let half = size * 0.5;
    let step = size / f64::from(segs);
    let n = segs + 1;

    let verts = (n * n) as usize;
    let mut positions = Vec::with_capacity(verts);
    let mut colors = Vec::with_capacity(verts);
    let mut indices = Vec::with_capacity((segs * segs * 6) as usize);

    for iz in 0..n {
        let z = -half + f64::from(iz) * step;
        for ix in 0..n {
            let x = -half + f64::from(ix) * step;
            let h = height(x, z);
            positions.push([x as f32, h as f32, z as f32]);
            colors.push(elevation_color(h));
        }
    }

    for iz in 0..segs {
        for ix in 0..segs {
            let a = iz * n + ix;
            let b = a + 1;
            let c = a + n;
            let d = c + 1;
            // `(a, c, b)` and not `(a, b, c)`: the front face is counter-
            // clockwise seen from +y, and with +x to the right and +z away the
            // naive order faces the ground down into the void.
            indices.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
    // `geo.computeVertexNormals()` (`terrain.js:73`). Indexed and unsplit, so
    // this is the smooth normal the JS gets, not a faceted one.
    mesh.compute_normals();
    mesh
}

/// What closes the map edge: a cliff down from the heightfield's boundary to
/// sea level, and a flat plain from there out to [`FOG_END`].
///
/// Neither is in the JS, and both should be. `terrain.js` draws one plane that
/// stops dead at ±1800 while the *height function* keeps going and returns zero
/// past that line — so the JS map has a 400-unit-tall open wall around it, and
/// from the spawn, 400 units inside the edge, you look straight over it into the
/// sky. This is not invention: the cliff face and the plain beyond it are what
/// [`sim::ship::terrain_height`] already says is there, drawn.
///
/// The cliff samples the ground mesh's own boundary vertices at the same
/// `segs`, so the two share an edge exactly and there is no crack to see
/// through. The plain reaches [`FOG_END`], where the fog is total and the
/// horizon is the same colour as the sky.
///
/// Cost: `4 * segs` quads plus four for the plain — about 3,000 triangles
/// against the ground's 295,000.
fn skirt_mesh(rules: &Rules, segs: u32) -> Mesh {
    let half = (rules.world.terrain_size * 0.5) as f32;
    let outer = FOG_END;
    let step = rules.world.terrain_size / f64::from(segs);

    let mut positions = Vec::new();
    let mut colors = Vec::new();
    let mut indices = Vec::new();

    // The four sides, each as `(fixed axis value, the direction the walk runs)`.
    //
    // The walk order is what gets the wall facing outward. With `up` fixed at
    // +y, a quad wound `(top, bottom, next top)` faces `along x up` — so each
    // side is walked in whichever direction makes that cross product point away
    // from the map, which is the perimeter traversed one consistent way round.
    let sides: [(bool, f32, f32); 4] = [
        // (walk along x?, the fixed coordinate, walk direction)
        (true, -half, -1.0), // north edge, z = -half, walked -x
        (false, -half, 1.0), // west edge,  x = -half, walked +z
        (true, half, 1.0),   // south edge, z = +half, walked +x
        (false, half, -1.0), // east edge,  x = +half, walked -z
    ];
    for (along_x, fixed, dir) in sides {
        let base = positions.len() as u32;
        for i in 0..=segs {
            // `dir` flips which end the walk starts from; the sample positions
            // themselves are the ground mesh's, to the bit.
            let t = if dir > 0.0 {
                -half as f64 + f64::from(i) * step
            } else {
                half as f64 - f64::from(i) * step
            };
            let (x, z) = if along_x {
                (t, f64::from(fixed))
            } else {
                (f64::from(fixed), t)
            };
            let h = height(x, z);
            positions.push([x as f32, h as f32, z as f32]);
            colors.push(elevation_color(h));
            positions.push([x as f32, 0.0, z as f32]);
            colors.push(elevation_color(0.0));
        }
        for i in 0..segs {
            let (t0, b0) = (base + i * 2, base + i * 2 + 1);
            let (t1, b1) = (t0 + 2, b0 + 2);
            indices.extend_from_slice(&[t0, b0, t1, t1, b0, b1]);
        }
    }

    // The plain, in four strips. The east/west pair stops at the heightfield's
    // z extent so the corners are covered exactly once.
    let strips = [
        (-outer, outer, -outer, -half),
        (-outer, outer, half, outer),
        (-outer, -half, -half, half),
        (half, outer, -half, half),
    ];
    for (x0, x1, z0, z1) in strips {
        let base = positions.len() as u32;
        for (x, z) in [(x0, z0), (x1, z0), (x0, z1), (x1, z1)] {
            positions.push([x, 0.0, z]);
            colors.push(elevation_color(0.0));
        }
        indices.extend_from_slice(&[base, base + 2, base + 1, base + 1, base + 2, base + 3]);
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
    mesh.compute_normals();
    mesh
}

/// The elevation ramp from `terrain.js:50`–`:68`: grass, forest, a fade to
/// rock, bare rock, then snow.
///
/// Transcribed rather than parameterised, because the band edges are the map's
/// visual identity and there is nothing else that would ever read them. The
/// numbers go straight into `Mesh::ATTRIBUTE_COLOR`, which Bevy multiplies into
/// `base_color` in linear space — the same space three.js has kept vertex
/// colours in since r152, so no conversion belongs here.
fn elevation_color(h: f64) -> [f32; 4] {
    let h = h as f32;
    let [r, g, b] = if h < 10.0 {
        [0.36, 0.50, 0.22]
    } else if h < 120.0 {
        [0.28, 0.48, 0.18]
    } else if h < 270.0 {
        let t = (h - 120.0) / 150.0;
        [0.28 + t * 0.26, 0.48 - t * 0.18, 0.18 + t * 0.12]
    } else if h < 420.0 {
        [0.54, 0.48, 0.40]
    } else {
        let t = ((h - 420.0) / 90.0).min(1.0);
        [0.54 + t * 0.42, 0.48 + t * 0.46, 0.40 + t * 0.55]
    };
    [r, g, b, 1.0]
}

// ---------------------------------------------------------------------------
// Trees
// ---------------------------------------------------------------------------

/// `trees.js:3` — how many to place.
const TREE_COUNT: usize = 340;
/// Elevation band trees grow in. `trees.js:4`–`:5`: above the valley floors,
/// below the bare rock.
const TREE_MIN_HEIGHT: f64 = 8.0;
/// See [`TREE_MIN_HEIGHT`].
const TREE_MAX_HEIGHT: f64 = 115.0;
/// Radius around each airfield centre kept clear. `trees.js:6`.
const TREE_AIRFIELD_CLEAR: f64 = 320.0;
/// Canopy cone: `ConeGeometry(7, 22, 6)`. `trees.js:19`.
const TREE_CANOPY: (f32, f32) = (7.0, 22.0);
/// Trunk frustum: `CylinderGeometry(1.2, 1.6, 10, 6)` — top radius first, the
/// same order `ConicalFrustum` takes. `trees.js:20`.
const TREE_TRUNK: (f32, f32, f32) = (1.2, 1.6, 10.0);

/// Where one tree stands, in world space. Split out from the spawn so the
/// rejection sampling can be tested without a render device.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Tree {
    x: f64,
    z: f64,
    /// Surface height under it, from [`height`].
    ground: f64,
    scale: f32,
    yaw: f32,
}

/// `createTrees` (`trees.js:18`), minus the `Math.random`.
///
/// Rejection sampling: draw a point, keep it if it is in the elevation band and
/// clear of both airfields. `trees.js:31` caps the attempts at twenty times the
/// target, which matters — the band is narrow and a run that cannot fill it
/// must end.
fn tree_placements(rules: &Rules) -> Vec<Tree> {
    let mut rng = sim::rng::Rng::with_stream(SCATTER_SEED, 1);
    // `trees.js:30`: inside the heightfield by 50 units, so no tree straddles
    // the hard zero at the map edge.
    let half = rules.world.terrain_size * 0.5 - 50.0;
    let centres = [-rules.world.airfield_z, rules.world.airfield_z];

    let mut out = Vec::with_capacity(TREE_COUNT);
    let mut attempts = 0;
    while out.len() < TREE_COUNT && attempts < TREE_COUNT * 20 {
        attempts += 1;
        let x = rng.next_f64_signed() * half;
        let z = rng.next_f64_signed() * half;
        let ground = height(x, z);
        if !(TREE_MIN_HEIGHT..=TREE_MAX_HEIGHT).contains(&ground) {
            continue;
        }
        if centres
            .iter()
            .any(|cz| (x * x + (z - cz) * (z - cz)) < TREE_AIRFIELD_CLEAR * TREE_AIRFIELD_CLEAR)
        {
            continue;
        }
        out.push(Tree {
            x,
            z,
            ground,
            scale: rng.range_f64(0.7, 1.5) as f32,
            yaw: rng.range_f64(0.0, 2.0 * std::f64::consts::PI) as f32,
        });
    }
    out
}

fn install_trees(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    rules: &Rules,
) {
    // Two meshes and two materials for the whole forest — the handles are
    // cloned per tree, never the assets, which is what collapses 680 entities
    // into two batches. Same idiom as `scene.rs`'s asteroid field.
    let canopy_mesh = meshes.add(
        Cone {
            radius: TREE_CANOPY.0,
            height: TREE_CANOPY.1,
        }
        .mesh()
        .resolution(6)
        .build(),
    );
    let trunk_mesh = meshes.add(
        ConicalFrustum {
            radius_top: TREE_TRUNK.0,
            radius_bottom: TREE_TRUNK.1,
            height: TREE_TRUNK.2,
        }
        .mesh()
        .resolution(6)
        .build(),
    );
    let canopy_mat = materials.add(StandardMaterial {
        base_color: hex(0x002d_5a1b).into(),
        perceptual_roughness: 0.9,
        ..default()
    });
    let trunk_mat = materials.add(StandardMaterial {
        base_color: hex(0x005a_3a1a).into(),
        perceptual_roughness: 0.95,
        ..default()
    });

    for tree in tree_placements(rules) {
        let (x, z) = (tree.x as f32, tree.z as f32);
        let ground = tree.ground as f32;
        let yaw = Quat::from_rotation_y(tree.yaw);

        // Both primitives are centred on their own midpoint, in Bevy and in
        // three.js alike, so the offsets are half-heights: the trunk's base
        // lands on the ground and the canopy's base lands on the trunk's top.
        // `trees.js:38` and `:42`.
        commands.spawn((
            Mesh3d(trunk_mesh.clone()),
            MeshMaterial3d(trunk_mat.clone()),
            Transform::from_xyz(x, ground + 5.0 * tree.scale, z)
                .with_rotation(yaw)
                .with_scale(Vec3::splat(tree.scale)),
            // `trees.js:24` sets `castShadow = false` on both instanced meshes.
            NotShadowCaster,
            MapScenery,
        ));
        commands.spawn((
            Mesh3d(canopy_mesh.clone()),
            MeshMaterial3d(canopy_mat.clone()),
            Transform::from_xyz(x, ground + 21.0 * tree.scale, z)
                .with_rotation(yaw)
                .with_scale(Vec3::splat(tree.scale)),
            NotShadowCaster,
            MapScenery,
        ));
    }
}

// ---------------------------------------------------------------------------
// Clouds
// ---------------------------------------------------------------------------

/// `clouds.js:2`–`:5` and `:15`.
const CLOUD_CLUSTERS: usize = 26;
/// Lowest cluster altitude. `clouds.js:3`.
const CLOUD_MIN_ALT: f64 = 280.0;
/// Highest cluster altitude. `clouds.js:4`.
const CLOUD_MAX_ALT: f64 = 520.0;
/// Half-width of the box clusters are scattered in, and the wrap distance.
/// `clouds.js:15`.
const CLOUD_SPREAD: f32 = 1700.0;
/// Base drift rate along x, in units per second. `clouds.js:5`.
const CLOUD_DRIFT_SPEED: f32 = 0.8;

/// `createClouds` (`clouds.js:6`): clusters of six to nine overlapping spheres.
///
/// The JS builds a `SphereGeometry` per puff, at a different radius each time —
/// two hundred distinct geometries. Here it is **one unit sphere** scaled by the
/// transform, so the whole sky is a single batch. Nothing else changes: a
/// uniformly scaled sphere is the sphere the JS asked for.
fn install_clouds(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) {
    let mut rng = sim::rng::Rng::with_stream(SCATTER_SEED, 2);

    // `SphereGeometry(r, 7, 5)` — deliberately faceted, and at this distance
    // the facets read as the lumpiness of a cumulus.
    let puff = meshes.add(Sphere::new(1.0).mesh().uv(7, 5));
    // `clouds.js:7`: white, `opacity: 0.72`, `depthWrite: false`. Bevy's
    // `AlphaMode::Blend` is exactly "sort into the transparent pass and do not
    // write depth", so the flag has no separate spelling here.
    let material = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 1.0, 1.0, 0.72),
        alpha_mode: AlphaMode::Blend,
        perceptual_roughness: 1.0,
        metallic: 0.0,
        ..default()
    });

    for _ in 0..CLOUD_CLUSTERS {
        let cx = rng.next_f64_signed() as f32 * CLOUD_SPREAD;
        let cy = rng.range_f64(CLOUD_MIN_ALT, CLOUD_MAX_ALT) as f32;
        let cz = rng.next_f64_signed() as f32 * CLOUD_SPREAD;
        let scale = rng.range_f64(0.6, 1.5) as f32;
        // `clouds.js:22`: a sign and a rate, drawn separately.
        let dir = if rng.bool_with_probability(0.5) {
            1.0
        } else {
            -1.0
        };
        let drift = dir * rng.range_f64(0.4, 1.0) as f32 * CLOUD_DRIFT_SPEED;
        let puffs = 6 + rng.bounded_usize(4);

        commands
            .spawn((
                Transform::from_xyz(cx, cy, cz),
                Visibility::default(),
                CloudDrift(drift),
                MapScenery,
            ))
            .with_children(|cluster| {
                for _ in 0..puffs {
                    let r = rng.range_f64(18.0, 46.0) as f32 * scale;
                    let sx = (rng.next_f64() as f32 - 0.5) * 60.0 * scale;
                    let sy = (rng.next_f64() as f32 - 0.5) * 14.0 * scale;
                    let sz = (rng.next_f64() as f32 - 0.5) * 50.0 * scale;
                    cluster.spawn((
                        Mesh3d(puff.clone()),
                        MeshMaterial3d(material.clone()),
                        Transform::from_xyz(sx, sy, sz).with_scale(Vec3::splat(r)),
                        NotShadowCaster,
                    ));
                }
            });
    }
}

/// `clouds.js:38`: drift along x and wrap. Twenty-six transforms a frame.
fn drift_clouds(time: Res<Time>, mut clusters: Query<(&mut Transform, &CloudDrift)>) {
    let dt = time.delta_secs();
    let wrap = CLOUD_SPREAD + 200.0;
    for (mut tf, drift) in &mut clusters {
        tf.translation.x += drift.0 * dt;
        if tf.translation.x > wrap {
            tf.translation.x = -wrap;
        } else if tf.translation.x < -wrap {
            tf.translation.x = wrap;
        }
    }
}

// ---------------------------------------------------------------------------
// Airfields
// ---------------------------------------------------------------------------

/// `createAirfield` (`airfield.js:3`), twice — team 0 at `-airfield_z` and team
/// 1 at `+airfield_z`, the far one turned to face the middle (`main.js:150`).
///
/// # The pad is the collision box
///
/// `airfield.js:19` draws a `560 x 3 x 380` slab with its top face at `y = 0`,
/// while [`sim::rules::WorldRules::airfield_half`] — the box
/// `resolve_world_collisions` bounces ships off — is `(280, 4, 190)` centred on
/// zero. The width and depth already agree exactly; only the *height* did not,
/// and the JS drew a pad three units thick where the solid is eight. So the
/// slab is derived from the rules instead, the same way `scene.rs` derives the
/// mothership hull, and the surface you land on is the surface that stops you.
///
/// The flat ground under it is not this module's doing: `airfield_blend`
/// (`sim::ship`) already forces [`height`] to zero across the apron, which is
/// why the pad can be a plain box.
fn install_airfields(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    rules: &Rules,
) {
    let half = rules.world.airfield_half;
    let (w, d, top) = (half.x as f32 * 2.0, half.z as f32 * 2.0, half.y as f32);
    // Markings sit just clear of the pad rather than straddling it. The JS
    // centres them *on* `y = 0` and turns on `polygonOffset` to break the tie
    // (`terrain.js:78`); a real gap needs no material state and cannot fight.
    let paint = top + 0.06;

    // Team-independent, so the two fields share them: paint, glass, and lamps.
    let line = materials.add(unlit(0x00dd_ddbb));
    let taxi = materials.add(unlit(0x00dd_aa00));
    let window = materials.add(unlit(0x00aa_ddff));
    let door_mat = materials.add(unlit(0x0011_1111));
    // The approach and edge lights are lights, so they are pushed past 1.0 to
    // give the bloom something to find — the same trick `scene.rs` plays on the
    // mothership's engine bells.
    let red = materials.add(glow(hex(0x00ff_3300), GLOW_BOOST));
    let green = materials.add(glow(hex(0x0033_ff66), GLOW_BOOST));
    let tank_mat = materials.add(StandardMaterial {
        base_color: hex(0x0088_8866).into(),
        perceptual_roughness: 0.7,
        ..default()
    });

    // Shared shapes. A unit sphere covers both lamp sizes by scale.
    let pad_mesh = meshes.add(Cuboid::new(w, top * 2.0, d));
    let centre_line = meshes.add(Cuboid::new(4.0, 0.05, d * 0.82));
    let threshold_bar = meshes.add(Cuboid::new(18.0, 0.05, 6.0));
    let taxi_line = meshes.add(Cuboid::new(2.0, 0.05, d * 0.6));
    let tower_mesh = meshes.add(Cuboid::new(20.0, 55.0, 20.0));
    let cab_mesh = meshes.add(Cuboid::new(24.0, 10.0, 24.0));
    let glass_mesh = meshes.add(Cuboid::new(24.4, 8.0, 24.4));
    let mast_mesh = meshes.add(Cylinder::new(0.4, 18.0));
    let radar_mesh = meshes.add(Cylinder::new(5.0, 0.5));
    let hangar_mesh = meshes.add(Cuboid::new(90.0, 28.0, 60.0));
    let roof_mesh = meshes.add(Cuboid::new(90.0, 6.0, 61.2));
    let hangar_door = meshes.add(Cuboid::new(63.0, 21.0, 1.0));
    let tank_mesh = meshes.add(Cylinder::new(10.0, 20.0));
    let lamp_mesh = meshes.add(Sphere::new(1.0).mesh().uv(6, 4));

    for team in 0..2u8 {
        // `airfield.js:6`–`:8`.
        let (tarmac_hex, building_hex, accent_hex) = if team == 0 {
            (0x003a_3a3a, 0x007a_8a6a, 0x0044_66aa)
        } else {
            (0x003a_3335, 0x008a_7a6a, 0x00aa_6644)
        };
        let tarmac = materials.add(StandardMaterial {
            base_color: hex(tarmac_hex).into(),
            perceptual_roughness: 0.95,
            metallic: 0.0,
            ..default()
        });
        let building = materials.add(StandardMaterial {
            base_color: hex(building_hex).into(),
            perceptual_roughness: 0.8,
            metallic: 0.1,
            ..default()
        });
        let accent = materials.add(StandardMaterial {
            base_color: hex(accent_hex).into(),
            perceptual_roughness: 0.6,
            metallic: 0.3,
            ..default()
        });

        let z = if team == 0 {
            -rules.world.airfield_z
        } else {
            rules.world.airfield_z
        };
        let facing = if team == 0 {
            Quat::IDENTITY
        } else {
            Quat::from_rotation_y(PI)
        };

        commands
            .spawn((
                Transform::from_xyz(0.0, 0.0, z as f32).with_rotation(facing),
                Visibility::default(),
                // Nothing on the ground casts on this map; see
                // `install_terrain_lights`. Set on the root so it covers every
                // child in one go.
                NotShadowCaster,
                MapScenery,
            ))
            .with_children(|field| {
                field.spawn((Mesh3d(pad_mesh.clone()), MeshMaterial3d(tarmac.clone())));

                // Runway centre line, threshold bars at both ends, and the two
                // taxiway stripes. `airfield.js:21`–`:35`.
                field.spawn((
                    Mesh3d(centre_line.clone()),
                    MeshMaterial3d(line.clone()),
                    Transform::from_xyz(0.0, paint, 0.0),
                ));
                for end_z in [-d * 0.38, d * 0.38] {
                    for k in -3..=3 {
                        field.spawn((
                            Mesh3d(threshold_bar.clone()),
                            MeshMaterial3d(line.clone()),
                            Transform::from_xyz(k as f32 * 28.0, paint, end_z),
                        ));
                    }
                }
                for x in [-w * 0.3, w * 0.3] {
                    field.spawn((
                        Mesh3d(taxi_line.clone()),
                        MeshMaterial3d(taxi.clone()),
                        Transform::from_xyz(x, paint, 0.0),
                    ));
                }

                // Control tower. `airfield.js:37`–`:55`.
                let (tx, tz) = (-w * 0.35, -d * 0.28);
                field.spawn((
                    Mesh3d(tower_mesh.clone()),
                    MeshMaterial3d(building.clone()),
                    Transform::from_xyz(tx, top + 27.5, tz),
                ));
                field.spawn((
                    Mesh3d(cab_mesh.clone()),
                    MeshMaterial3d(accent.clone()),
                    Transform::from_xyz(tx, top + 60.0, tz),
                ));
                // **The one dimension here that is not the JS's.**
                // `airfield.js:48` makes the glass `TW + 2` across inside a cab
                // that is `TW + 4`, so it is sealed inside the solid and has
                // never drawn a pixel — the same class of bug as the
                // mothership's shield plane, and fixed the same way. Widened by
                // 0.4 so it reads as a glazed band around the cab.
                field.spawn((
                    Mesh3d(glass_mesh.clone()),
                    MeshMaterial3d(window.clone()),
                    Transform::from_xyz(tx, top + 60.0, tz),
                ));
                field.spawn((
                    Mesh3d(mast_mesh.clone()),
                    MeshMaterial3d(accent.clone()),
                    Transform::from_xyz(tx, top + 74.0, tz),
                ));
                field.spawn((
                    Mesh3d(radar_mesh.clone()),
                    MeshMaterial3d(accent.clone()),
                    Transform::from_xyz(tx + 6.0, top + 77.0, tz)
                        .with_rotation(Quat::from_rotation_z(FRAC_PI_3)),
                ));

                // Two hangars with a roof cap and a dark door. `airfield.js:57`.
                for hx in [-w * 0.3, w * 0.3] {
                    let hz = d * 0.32;
                    field.spawn((
                        Mesh3d(hangar_mesh.clone()),
                        MeshMaterial3d(building.clone()),
                        Transform::from_xyz(hx, top + 14.0, hz),
                    ));
                    field.spawn((
                        Mesh3d(roof_mesh.clone()),
                        MeshMaterial3d(accent.clone()),
                        Transform::from_xyz(hx, top + 31.0, hz),
                    ));
                    field.spawn((
                        Mesh3d(hangar_door.clone()),
                        MeshMaterial3d(door_mat.clone()),
                        Transform::from_xyz(hx, top + 10.5, hz - 30.3),
                    ));
                }

                // Fuel tanks. `airfield.js:70`.
                for tz in [-d * 0.15, d * 0.15] {
                    field.spawn((
                        Mesh3d(tank_mesh.clone()),
                        MeshMaterial3d(tank_mat.clone()),
                        Transform::from_xyz(w * 0.42, top + 10.0, tz),
                    ));
                }

                // `airfield.js:76`: a coloured point light over the apron. The
                // lumen figure matches `scene.rs`'s hangar lamp, which stands
                // in for the same three.js `intensity: 2.0`.
                field.spawn((
                    PointLight {
                        color: hex(accent_hex).into(),
                        intensity: 4.0e6,
                        range: 200.0,
                        shadow_maps_enabled: false,
                        ..default()
                    },
                    Transform::from_xyz(0.0, top + 8.0, 0.0),
                ));

                // Approach lights across the threshold, then the runway edge
                // rows. `airfield.js:80`–`:94`.
                for k in 0..6 {
                    field.spawn((
                        Mesh3d(lamp_mesh.clone()),
                        MeshMaterial3d(if k % 2 == 0 {
                            red.clone()
                        } else {
                            green.clone()
                        }),
                        Transform::from_xyz(k as f32 * 14.0 - 35.0, top + 1.0, -d * 0.45)
                            .with_scale(Vec3::splat(1.2)),
                    ));
                }
                for k in -4..=4 {
                    for side in [-1.0f32, 1.0] {
                        field.spawn((
                            Mesh3d(lamp_mesh.clone()),
                            MeshMaterial3d(red.clone()),
                            Transform::from_xyz(side * w * 0.24, top + 0.5, k as f32 * (d * 0.09))
                                .with_scale(Vec3::splat(0.8)),
                        ));
                    }
                }
            });
    }
}

/// A `MeshBasicMaterial` at its authored colour: unlit, no glow boost.
///
/// The distinction from `scene.rs`'s [`glow`] is that `upgradeMaterials`
/// (`graphics.js:413`) only multiplies *additively blended* basics past 1.0.
/// The paint on a runway is not additive and must not be brightened, or the
/// centre line outshines the lights beside it.
fn unlit(rgb: u32) -> StandardMaterial {
    StandardMaterial {
        base_color: hex(rgb).into(),
        unlit: true,
        ..default()
    }
}

// ---------------------------------------------------------------------------
// Sky environment
// ---------------------------------------------------------------------------

/// Edge length of one face of the sky cubemap.
///
/// Sixteen pixels. It only ever feeds `GeneratedEnvironmentMapLight`, which
/// convolves it down to an irradiance probe and a handful of roughness mips, so
/// the input to that is a two-colour vertical ramp with nothing above the
/// second spherical harmonic to resolve. `applySkyEnvironment` uses 128 because
/// a `<canvas>` was the cheapest way to make a gradient in the browser, not
/// because the detail was needed.
const SKY_FACE: u32 = 16;

/// `applySkyEnvironment(scene, renderer, 0x6fa8d4, 0x4a4335)`
/// (`graphics.js:337`): sky above, ground below, a vertical ramp on the sides.
///
/// Face order is `+X, -X, +Y, -Y, +Z, -Z` in Bevy's cubemap layers and in
/// `THREE.CubeTexture`'s array alike, which is why the `+Y`/`-Y` special cases
/// carry over as indices 2 and 3 unchanged.
fn sky_ground_cubemap() -> Image {
    let sky = hex(SKY_COLOR);
    let ground = hex(SKY_GROUND_COLOR);
    let px = (SKY_FACE * SKY_FACE) as usize;
    let mut data = Vec::with_capacity(px * 6 * 4);

    for face in 0..6u32 {
        for y in 0..SKY_FACE {
            // Zero at the top of the face, one at the bottom — a canvas
            // gradient's convention and a cubemap face's row order agree here.
            let t = (y as f32 + 0.5) / SKY_FACE as f32;
            let c = match face {
                2 => sky,
                3 => ground,
                _ => Srgba::new(
                    sky.red + (ground.red - sky.red) * t,
                    sky.green + (ground.green - sky.green) * t,
                    sky.blue + (ground.blue - sky.blue) * t,
                    1.0,
                ),
            };
            let rgba = [
                (c.red * 255.0) as u8,
                (c.green * 255.0) as u8,
                (c.blue * 255.0) as u8,
                255,
            ];
            for _ in 0..SKY_FACE {
                data.extend_from_slice(&rgba);
            }
        }
    }

    Image {
        // Six layers are a 2D array until this says otherwise, and the skybox
        // and environment pipelines both fail at bind-group creation rather
        // than anywhere legible. Same note as `skybox.rs`.
        texture_view_descriptor: Some(TextureViewDescriptor {
            dimension: Some(TextureViewDimension::Cube),
            ..default()
        }),
        ..Image::new(
            Extent3d {
                width: SKY_FACE,
                height: SKY_FACE,
                depth_or_array_layers: 6,
            },
            TextureDimension::D2,
            data,
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::RENDER_WORLD,
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Everything the module can be wrong about without a GPU: does the drawn
/// surface agree with the surface that kills you, and do the props stand on it.
#[cfg(test)]
mod tests {
    use super::*;

    /// A mesh's positions, or a panic naming what was expected.
    fn positions(mesh: &Mesh) -> &[[f32; 3]] {
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(p)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the mesh must carry float positions");
        };
        p
    }

    /// Axis-aligned bounds, computed here rather than through Bevy's own
    /// `Aabb`, which lives behind the render types and is not worth reaching
    /// into for three assertions.
    fn bounds(mesh: &Mesh) -> ([f32; 3], [f32; 3]) {
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for p in positions(mesh) {
            for axis in 0..3 {
                lo[axis] = lo[axis].min(p[axis]);
                hi[axis] = hi[axis].max(p[axis]);
            }
        }
        (lo, hi)
    }

    /// The forward, pinned bit for bit.
    ///
    /// Trivially true today, which is the point — it is here so that the day
    /// somebody "optimises" [`height`] into a local copy of the noise, or
    /// caches it into an `f32` grid, the test that breaks names the invariant
    /// instead of a screenshot looking slightly wrong.
    #[test]
    fn terrain_height_matches_the_simulation() {
        let rules = Rules::DEFAULT;
        let mut rng = sim::rng::Rng::new(0x7E44_A177);
        for _ in 0..2_000 {
            let x = rng.range_f64(-2_000.0, 2_000.0);
            let z = rng.range_f64(-2_000.0, 2_000.0);
            assert_eq!(
                height(x, z).to_bits(),
                sim::ship::terrain_height(x, z, &rules).to_bits(),
                "at ({x}, {z})",
            );
        }
    }

    /// Every vertex of the ground mesh is exactly where the simulation says the
    /// surface is, to the `f32` the mesh stores.
    #[test]
    fn ground_vertices_sit_on_the_simulation_surface() {
        let rules = Rules::DEFAULT;
        let segs = 32;
        let mesh = ground_mesh(&rules, segs);
        let verts = positions(&mesh);
        assert_eq!(verts.len(), ((segs + 1) * (segs + 1)) as usize);
        for [x, y, z] in verts {
            assert_eq!(
                *y,
                height(f64::from(*x), f64::from(*z)) as f32,
                "vertex at ({x}, {z})",
            );
        }
    }

    /// The mesh spans the whole heightfield and no more, so the skirt meets it
    /// exactly.
    #[test]
    fn ground_covers_the_heightfield_exactly() {
        let rules = Rules::DEFAULT;
        let (lo, hi) = bounds(&ground_mesh(&rules, 8));
        let half = (rules.world.terrain_size * 0.5) as f32;
        assert!((lo[0] + half).abs() < 1e-3);
        assert!((hi[0] - half).abs() < 1e-3);
        assert!((lo[2] + half).abs() < 1e-3);
        assert!((hi[2] - half).abs() < 1e-3);
    }

    /// Height of the *drawn* surface at an arbitrary point: the same bilinear
    /// pair of triangles the GPU rasterises, evaluated on the CPU.
    ///
    /// Reproduces `ground_mesh`'s `(a, c, b) / (b, c, d)` split, so a change to
    /// the triangulation that this did not follow would show up as sag the
    /// renderer does not actually have.
    fn drawn_surface(rules: &Rules, segs: u32, x: f64, z: f64) -> f64 {
        let size = rules.world.terrain_size;
        let half = size * 0.5;
        let step = size / f64::from(segs);

        let fx = (x + half) / step;
        let fz = (z + half) / step;
        let ix = (fx.floor() as u32).min(segs - 1);
        let iz = (fz.floor() as u32).min(segs - 1);
        let u = fx - f64::from(ix);
        let v = fz - f64::from(iz);

        let at =
            |cx: u32, cz: u32| height(-half + f64::from(cx) * step, -half + f64::from(cz) * step);
        let (ha, hb, hc, hd) = (
            at(ix, iz),
            at(ix + 1, iz),
            at(ix, iz + 1),
            at(ix + 1, iz + 1),
        );

        if u + v <= 1.0 {
            ha + u * (hb - ha) + v * (hc - ha)
        } else {
            hd + (1.0 - u) * (hc - hd) + (1.0 - v) * (hb - hd)
        }
    }

    /// `sqrt(1 + |grad h|^2)` at `(x, z)`: how much longer the surface is than
    /// its footprint, and therefore the factor between a *vertical*
    /// disagreement and the perpendicular gap a player actually sees.
    ///
    /// Central differences over a metre, which resolves every octave in
    /// [`sim::ship::terrain_height`] — the shortest has a 300-unit wavelength —
    /// while staying far enough from the `f64` noise floor to be meaningful.
    fn slope_secant(x: f64, z: f64) -> f64 {
        let d = 0.5;
        let dx = (height(x + d, z) - height(x - d, z)) / (2.0 * d);
        let dz = (height(x, z + d) - height(x, z - d)) / (2.0 * d);
        (1.0 + dx * dx + dz * dz).sqrt()
    }

    /// Worst gap, normal to the ground, between the mesh at `segs` segments and
    /// the surface the simulation kills against. Fixed seed, so the number is
    /// the same on every machine and every run.
    fn worst_normal_gap(segs: u32, samples: u32) -> f64 {
        let rules = Rules::DEFAULT;
        let mut rng = sim::rng::Rng::new(0x0D15_C0DE);
        let mut worst = 0.0f64;
        for _ in 0..samples {
            // Inside the map by a cell, so nothing lands on the hard zero at
            // the edge — that is a real discontinuity in the height function,
            // not something a finer mesh would fix.
            let x = rng.range_f64(-1_780.0, 1_780.0);
            let z = rng.range_f64(-1_780.0, 1_780.0);
            let dy = (drawn_surface(&rules, segs, x, z) - height(x, z)).abs();
            worst = worst.max(dy / slope_secant(x, z));
        }
        worst
    }

    /// **The test this module exists for.**
    ///
    /// Between its vertices the mesh is a chord and the surface is not, and the
    /// question is whether the gap is ever big enough to be seen. The budget is
    /// [`sim::rules::WorldRules::terrain_kill_clearance`]: a ship dies below
    /// `terrain_height + 5`, so a five-unit gap is exactly the point at which a
    /// player dies *at* the drawn ground instead of above it.
    ///
    /// Measured normal to the surface rather than vertically — see the module
    /// docs on why the vertical figure is misleading on a cliff.
    #[test]
    fn the_drawn_surface_stays_inside_the_kill_clearance() {
        let budget = Rules::DEFAULT.world.terrain_kill_clearance;
        let worst = worst_normal_gap(GROUND_SEGMENTS, 100_000);
        assert!(
            worst < budget,
            "worst mesh-vs-simulation gap {worst:.3} exceeds the {budget:.3} clearance",
        );
    }

    /// And the same measurement at the JS's own 96 segments, so the reason this
    /// module does not simply transcribe `terrain.js:3` is recorded as a number
    /// and not as a claim in a comment.
    #[test]
    fn the_javascripts_own_resolution_would_not_have_cleared_it() {
        let budget = Rules::DEFAULT.world.terrain_kill_clearance;
        let worst = worst_normal_gap(96, 100_000);
        assert!(
            worst > budget,
            "96 segments now clears the budget ({worst:.3}); GROUND_SEGMENTS can come down",
        );
    }

    /// Prints the error distribution against segment count. `cargo test -p
    /// spaceships-client -- --ignored --nocapture terrain::tests::survey`.
    ///
    /// Ignored because it is a measurement, not an assertion — it is the
    /// working that [`GROUND_SEGMENTS`] was chosen from, kept runnable so the
    /// choice can be revisited rather than re-derived.
    #[test]
    #[ignore = "measurement, not an assertion"]
    fn survey_the_error_against_segment_count() {
        let rules = Rules::DEFAULT;
        for segs in [96u32, 128, 192, 256, 384, 512] {
            let mut rng = sim::rng::Rng::new(0x0D15_C0DE);
            let mut vertical = Vec::with_capacity(200_000);
            let mut normal = Vec::with_capacity(200_000);
            for _ in 0..200_000 {
                let x = rng.range_f64(-1_780.0, 1_780.0);
                let z = rng.range_f64(-1_780.0, 1_780.0);
                let dy = (drawn_surface(&rules, segs, x, z) - height(x, z)).abs();
                vertical.push(dy);
                normal.push(dy / slope_secant(x, z));
            }
            let report = |name: &str, mut e: Vec<f64>| {
                e.sort_by(|a, b| a.partial_cmp(b).expect("no NaN in a height"));
                let pct = |p: f64| e[((e.len() - 1) as f64 * p) as usize];
                println!(
                    "  {name:10} mean {:6.3}  p50 {:6.3}  p99 {:6.3}  p99.9 {:6.3}  max {:7.3}",
                    e.iter().sum::<f64>() / e.len() as f64,
                    pct(0.50),
                    pct(0.99),
                    pct(0.999),
                    e[e.len() - 1],
                );
            };
            println!(
                "{segs:4} segs ({:5.2} u cells)",
                rules.world.terrain_size / f64::from(segs)
            );
            report("vertical", vertical);
            report("normal", normal);
        }
    }

    /// Trees stand on the ground, inside their elevation band, and off both
    /// aprons.
    #[test]
    fn trees_stand_on_the_simulation_surface() {
        let rules = Rules::DEFAULT;
        let trees = tree_placements(&rules);
        assert_eq!(trees.len(), TREE_COUNT, "the forest has to fill");

        let clear = TREE_AIRFIELD_CLEAR * TREE_AIRFIELD_CLEAR;
        for tree in &trees {
            assert_eq!(tree.ground, height(tree.x, tree.z));
            assert!((TREE_MIN_HEIGHT..=TREE_MAX_HEIGHT).contains(&tree.ground));
            for cz in [-rules.world.airfield_z, rules.world.airfield_z] {
                let dz = tree.z - cz;
                assert!(
                    tree.x * tree.x + dz * dz >= clear,
                    "a tree at ({}, {}) is on an apron",
                    tree.x,
                    tree.z,
                );
            }
            assert!((0.7..=1.5).contains(&tree.scale));
        }
    }

    /// The forest is the same forest every run. `sim::rng` is seeded, but only
    /// as long as nothing here reaches for a clock or a global generator.
    #[test]
    fn tree_placement_is_deterministic() {
        let rules = Rules::DEFAULT;
        assert_eq!(tree_placements(&rules), tree_placements(&rules));
    }

    /// The pad the renderer draws is the box the simulation bounces ships off.
    #[test]
    fn the_airfield_pad_is_the_collision_box() {
        let rules = Rules::DEFAULT;
        let half = rules.world.airfield_half;
        let mesh = Cuboid::new(
            half.x as f32 * 2.0,
            half.y as f32 * 2.0,
            half.z as f32 * 2.0,
        )
        .mesh()
        .build();
        let (lo, hi) = bounds(&mesh);
        assert_eq!(hi, [half.x as f32, half.y as f32, half.z as f32]);
        assert_eq!(lo, [-half.x as f32, -half.y as f32, -half.z as f32]);

        // And the apron it sits on is genuinely flat: `airfield_blend` is what
        // makes a plain box the right shape to draw.
        for dx in [-100.0, 0.0, 100.0] {
            for dz in [-80.0, 0.0, 80.0] {
                assert_eq!(height(dx, -rules.world.airfield_z + dz), 0.0);
                assert_eq!(height(dx, rules.world.airfield_z + dz), 0.0);
            }
        }
    }

    /// The skirt reaches past the point the fog is total, and its cliff meets
    /// the ground mesh's boundary vertex for vertex — a crack there is a hole
    /// through the map edge into the sky, which is what `terrain.js` has.
    #[test]
    fn the_skirt_closes_the_map_edge() {
        let rules = Rules::DEFAULT;
        let segs = 32;
        let skirt = skirt_mesh(&rules, segs);
        let (lo, hi) = bounds(&skirt);
        assert!(hi[0] >= FOG_END);
        assert!(hi[2] >= FOG_END);
        assert_eq!(lo[1], 0.0, "nothing in the skirt goes below sea level");

        // Every boundary vertex of the ground has an exactly equal partner in
        // the cliff.
        let half = (rules.world.terrain_size * 0.5) as f32;
        let wall = positions(&skirt);
        let mut edges = 0;
        for p in positions(&ground_mesh(&rules, segs)) {
            if p[0].abs() != half && p[2].abs() != half {
                continue;
            }
            edges += 1;
            assert!(wall.contains(p), "the cliff has no vertex at {p:?}");
        }
        assert_eq!(edges, (4 * segs) as usize, "four sides of the boundary");
    }

    /// The elevation ramp, transcribed band for band from `terrain.js:50`.
    ///
    /// Pinned as literals because the ramp *is* the map's look and there is
    /// nothing else it could be checked against. Note that two of the four band
    /// edges are deliberate steps rather than fades — the JS jumps at 10 (the
    /// waterline green) and at 270 (grass to bare rock) and only ramps between
    /// 120 and 270 and above 420 — so this cannot be written as a continuity
    /// test, which is the first thing one reaches for and the wrong thing.
    #[test]
    fn the_elevation_ramp_matches_the_javascript() {
        // The two ramped bands are `a + t * b` and land a ulp off the decimal
        // they are quoted as, so this is a tolerance and not an equality.
        let band = |h: f64, want: [f32; 3]| {
            let got = elevation_color(h);
            for c in 0..3 {
                assert!(
                    (got[c] - want[c]).abs() < 1e-6,
                    "at height {h}: {got:?} is not {want:?}",
                );
            }
            assert_eq!(got[3], 1.0);
        };
        band(0.0, [0.36, 0.50, 0.22]);
        band(50.0, [0.28, 0.48, 0.18]);
        // Midway through the fade to rock: t = 0.5.
        band(195.0, [0.41, 0.39, 0.24]);
        band(300.0, [0.54, 0.48, 0.40]);
        // Snow, saturated: t clamps at 1.
        band(9_000.0, [0.96, 0.94, 0.95]);
    }

    /// Six square faces, four bytes each, and the poles are the flat colours
    /// `applySkyEnvironment` paints them.
    #[test]
    fn the_sky_cubemap_is_six_faces() {
        let image = sky_ground_cubemap();
        let face = (SKY_FACE * SKY_FACE * 4) as usize;
        let data = image.data.as_ref().expect("the image is built with data");
        assert_eq!(data.len(), face * 6);

        let sky = hex(SKY_COLOR);
        assert_eq!(data[face * 2], (sky.red * 255.0) as u8);
        let ground = hex(SKY_GROUND_COLOR);
        assert_eq!(data[face * 3], (ground.red * 255.0) as u8);
    }
}
