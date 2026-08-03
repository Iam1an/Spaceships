//! The Sierras map: ground, water, scenery, bases, weather, and its own sun.
//!
//! # What this module draws, and what decides it
//!
//! **Nothing here has its own idea of where the ground is.** [`height`] is a
//! one-line forward to [`sim::terrain::ground_height`] and is the only way
//! anything in this module or its submodules learns about the surface — the
//! mesh, the tree line, the boulder fields, the base pads. That is not
//! tidiness. The simulation kills a ship below
//! [`sim::terrain::kill_altitude`], so a renderer with a second copy of the
//! heightfield produces ships sinking into hillsides and players dying to
//! invisible geometry, which is strictly worse than drawing no ground at all.
//!
//! Since the heightfield was rebuilt as a lattice, that forward is also
//! *exact*: [`ground`] draws one triangle per lattice face, so the surface you
//! see is the surface you collide with rather than a mesh fine enough that the
//! difference fits inside the kill clearance. The module this replaced carried a
//! table of error percentiles and an argument for 295,000 triangles; the
//! argument is gone along with 250,000 of the triangles.
//!
//! # Layout
//!
//! | Submodule | Draws |
//! |---|---|
//! | [`ground`] | The heightfield and the sea |
//! | [`props`] | Trees, boulders, and cloud banks |
//! | [`base`] | The two team mesas: pads, hangars, towers, lights |
//!
//! Lighting, fog, the sky probe, and the map-swap lifecycle stay here, because
//! they are the things that are true of the map rather than of anything on it.
//!
//! # Draw calls
//!
//! `main.rs` says at length why this codebase counts draw calls and not
//! triangles — the JS client's problem was 477 of them. Everything static that
//! can be merged into one mesh is, so `SPACESHIPS_BATCHES=1` reports shapes and
//! not entities:
//!
//! | Prop | Entities | Batch keys |
//! |---|---|---|
//! | Ground | 1 | 1 |
//! | Sea | 1 | 1 |
//! | Trees (merged) | 1 | 1 |
//! | Boulders (merged) | 1 | 1 |
//! | Clouds (one baked mesh per cluster) | 26 | 26 |
//! | Bases (two) | ~130 | ~22 |
//!
//! # Swapping maps
//!
//! [`apply_map`] is the whole lifecycle. It early-outs on an unchanged map, so
//! it costs one enum compare a frame; when the map *does* change it despawns
//! everything tagged [`MapScenery`] — this module's props and whichever
//! lighting rig is installed — hides or shows `scene.rs`'s space props, and
//! rebuilds. That is what makes the lobby's Space/Sierras toggle work without
//! `scene.rs` having to know the terrain exists.

mod base;
mod ground;
mod props;

use bevy::asset::RenderAssetUsages;
use bevy::image::Image;
use bevy::light::{CascadeShadowConfigBuilder, GeneratedEnvironmentMapLight, Skybox};
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};

use spaceships_sim as sim;

use crate::camera::FlightCamera;
use crate::scene::{hex, install_space_lights, MapScenery, SpaceScenery};
use crate::sim_bridge::MatchSetup;
use crate::skybox::NebulaCubemap;
use sim::rules::Rules;
use sim::world::MapKind;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Colour the distance fades to.
///
/// `main.js:122` is `new THREE.Fog(0xbbd5f0, ...)`, a pale haze that was picked
/// against a map with no horizon in it — the JS terrain stopped dead at ±1800
/// and you looked over the edge into flat sky. This map has an ocean running out
/// to [`FOG_END`], so the fog colour *is* the horizon, and at the JS's value the
/// sea faded to a band of near-white sitting under a mid-blue sky with a hard
/// line between them. Pulled toward [`SKY_COLOR`], a shade lighter, so the
/// distance dissolves instead of ending.
const FOG_COLOR: u32 = 0x0083_b2da;
/// Distance at which the fog starts to bite. `main.js:122`.
const FOG_START: f32 = 1400.0;
/// Distance at which the fog is total. `main.js:122`. Also how far the sea
/// reaches, so the horizon is water fading into haze and never a map edge.
pub(crate) const FOG_END: f32 = 4800.0;

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
/// in space and not here is albedo: rock and hull sit near 0.3, while the snow
/// band in [`ground`] is 0.93, and a sun sized for the first blows out the
/// second. So this is set from the albedo instead — `albedo * lux / pi`, at
/// Bevy's default EV100 of 9.7, landing just under 1.0 for the brightest band
/// the map contains.
const SUN_LUX: f32 = 3_300.0;

/// Direction the sunlight travels.
///
/// **This is a deliberate break from the JS**, which is worth being explicit
/// about because the rest of the lighting is parity. `main.js:1711`–`:1713`
/// copies the ship's position into the light's target and puts the light
/// directly above it, so the sun points *straight down*: every vertical face
/// takes `N · L = 0` and is lit only by ambient and the sky probe.
///
/// That was survivable on a smooth-shaded map whose relief read from baked
/// elevation colours. It is not survivable here. The whole point of a faceted
/// heightfield is that neighbouring facets catch the light differently, and
/// with the sun on the zenith every facet that is not level catches the same
/// amount of nothing — the map flattens into a coloured plan view. A fixed
/// afternoon sun, low enough to rake across the ranges, is what makes the
/// ravines read as depth and the mesas read as height.
///
/// Fixed in *world* space, not relative to the player: a sun that swung as you
/// turned would take every shadow on the map with it.
///
/// The bearing is mostly along **x** on purpose. The two bases face each other
/// down the z axis, so a sun with much z in it backlights one team's whole view
/// and frontlights the other's — the first screenshot from team 0's pad had the
/// entire foreground in silhouette. Across the axis of play, both halves get the
/// same cross-light.
const SUN_DIR: Vec3 = Vec3::new(-0.66, -0.68, -0.32);

/// How far up the shadow-casting light sits above the player.
///
/// The direction is [`SUN_DIR`] and does not change; only the position follows,
/// and only so the tight cascade box stays over the ship. `main.js:1713`.
const SUN_HEIGHT: f32 = 500.0;

/// Ambient brightness.
///
/// `main.js:108` is `AmbientLight(0xfff8e8, 0.28)` under Ultra, low because
/// Ultra also installs `applySkyEnvironment` — see [`sky_ground_cubemap`] — and
/// the sky is expected to do the filling.
///
/// Lower than the straight-down rig needed. That version had to raise the
/// ambient until unlit verticals stopped rendering black, because with the sun
/// on the zenith the ambient was the *only* light a cliff face received. With
/// [`SUN_DIR`] raking across the map, faces are lit or in shadow on their own
/// merits and the ambient's job is back to what it should be: keeping the
/// shadowed side readable without flattening it.
///
/// "Readable" is doing work in that sentence. At 520, with the sky probe at its
/// space-map strength, the shadowed side of every hill rendered near black under
/// ACES — and on a map made of hard-edged facets that loses half of them.
///
/// The fix is mostly *not* here, though. Flat ambient lifts a shadow by washing
/// it, and past a certain point the map looks like a model kit. Most of the lift
/// comes from [`SKY_PROBE`] instead, which is directional: a cliff in shadow
/// picks up the blue above it and the dark ground below it, which is what
/// actually happens outdoors. This is the smaller half of the pair.
const AMBIENT_BRIGHTNESS: f32 = 620.0;

/// Strength of the sky/ground environment probe.
///
/// Well above the 900 the space map uses for its nebula, and it is the main
/// source of fill light here rather than the ambient. Outdoors, the light on a
/// surface the sun cannot reach comes from the sky it can see, which is
/// directional — blue from above, dark from the ground below — and a probe
/// reproduces that where a uniform ambient cannot. Raising this instead of
/// [`AMBIENT_BRIGHTNESS`] is what keeps a shadowed cliff readable without
/// flattening every facet on it.
const SKY_PROBE: f32 = 2_200.0;

/// Seed for every placement decision on this map — trees, boulders, clouds.
///
/// `trees.js` and `clouds.js` both call `Math.random`, so the JS reshuffles the
/// forest on every load. A fixed seed is the house rule (`sim::rng`) and it is
/// also what makes a screenshot comparable to the last one.
pub(crate) const SCATTER_SEED: u64 = 0x5133_2A55;

// ---------------------------------------------------------------------------
// The height function
// ---------------------------------------------------------------------------

/// Terrain bed height at a world `(x, z)` — **the** sampler, and a plain forward
/// to the simulation's own.
///
/// The *bed*, so it goes below [`sim::rules::WorldRules::water_level`] in the
/// lake and the river gorges. Anything placing scenery wants this one: a tree
/// belongs on the ground, and whether that ground is underwater is a question
/// the tree line answers rather than something to be clamped away.
///
/// Deliberately trivial: the moment the renderer grows its own copy of the
/// noise, the ground stops agreeing with the thing that kills you.
#[must_use]
pub fn height(x: f64, z: f64) -> f64 {
    sim::terrain::ground_height(x, z, &Rules::DEFAULT.world)
}

// ---------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------

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
        app.add_systems(Update, (apply_map, props::drift_clouds, follow_sun));
    }
}

/// Installs or tears down the whole map when [`MatchSetup::map`] changes.
///
/// Keyed on a `Local` rather than on `Res::is_changed`, because the resource is
/// also written by things that do not touch the map (the callsign, the seed)
/// and rebuilding the terrain because someone renamed themselves would be a
/// visible hitch for no reason. The first run always fires, since `None` never
/// equals a map.
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
        ground::install(&mut commands, &mut meshes, &mut materials, &rules);
        base::install(&mut commands, &mut meshes, &mut materials, &rules);
        props::install(&mut commands, &mut meshes, &mut materials, &rules);

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
                intensity: SKY_PROBE,
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
        sun_transform(Vec3::ZERO),
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

/// Where the sun sits and which way it looks, for a player at `at`.
///
/// Position tracks the player so the cascade box does; direction is [`SUN_DIR`]
/// and never moves. `looking_to` needs an up vector that is not parallel to the
/// view direction, and `SUN_DIR` is not vertical, so `+Y` is safe here — unlike
/// the straight-down rig this replaced, which had to pass `+Z`.
fn sun_transform(at: Vec3) -> Transform {
    Transform::from_translation(at - SUN_DIR.normalize() * SUN_HEIGHT).looking_to(SUN_DIR, Vec3::Y)
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
    *tf = sun_transform(me.translation);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The forward, pinned bit for bit.
    ///
    /// Trivially true today, which is the point — it is here so that the day
    /// somebody "optimises" [`height`] into a local copy of the noise, or
    /// caches it into an `f32` grid, the test that breaks names the invariant
    /// instead of a screenshot looking slightly wrong.
    #[test]
    fn height_matches_the_simulation() {
        let rules = Rules::DEFAULT;
        let mut rng = sim::rng::Rng::new(0x7E44_A177);
        for _ in 0..2_000 {
            let x = rng.range_f64(-2_000.0, 2_000.0);
            let z = rng.range_f64(-2_000.0, 2_000.0);
            assert_eq!(
                height(x, z).to_bits(),
                sim::terrain::ground_height(x, z, &rules.world).to_bits(),
                "at ({x}, {z})",
            );
        }
    }

    /// [`height`] is the *bed* and not the surface, which is the distinction
    /// every scenery placement depends on: a lake bed is somewhere a tree must
    /// not stand, and clamping it to the waterline would hide that.
    #[test]
    fn height_is_the_bed_and_not_the_waterline() {
        let rules = Rules::DEFAULT;
        let mut found_submerged = false;
        let mut rng = sim::rng::Rng::new(0x0BED_0BED);
        for _ in 0..4_000 {
            let x = rng.range_f64(-1_700.0, 1_700.0);
            let z = rng.range_f64(-1_700.0, 1_700.0);
            if height(x, z) < rules.world.water_level {
                found_submerged = true;
                assert_eq!(
                    sim::ship::terrain_height(x, z, &rules),
                    rules.world.water_level,
                    "the surface over a lake bed is the water",
                );
            }
        }
        assert!(found_submerged, "the map is supposed to have water in it");
    }

    /// The sun rakes across the map instead of standing on the zenith, which is
    /// the one thing flat shading cannot do without.
    ///
    /// Asserted as an angle rather than as the vector, so retuning the bearing
    /// does not break it but flattening it back to vertical does.
    #[test]
    fn the_sun_is_not_overhead() {
        let d = SUN_DIR.normalize();
        assert!(d.y < 0.0, "the sun must shine downward");
        // Roughly 25°–65° of elevation: below that it grazes and the shadows
        // swamp the map, above it the facets stop separating.
        assert!(
            (0.42..=0.91).contains(&-d.y),
            "sun elevation is outside the usable band: {}",
            -d.y,
        );
        assert!(
            Vec2::new(d.x, d.z).length() > 0.35,
            "the sun needs a horizontal bearing to rake with",
        );
    }

    /// The light is placed *up* its own direction from the player, so the
    /// cascade in front of it contains the ship rather than sitting behind it.
    #[test]
    fn the_sun_sits_upwind_of_the_player_and_looks_at_them() {
        let at = Vec3::new(120.0, 40.0, -300.0);
        let tf = sun_transform(at);
        assert!(tf.translation.y > at.y, "the sun is above the player");
        let to_player = (at - tf.translation).normalize();
        assert!(
            to_player.dot(SUN_DIR.normalize()) > 0.999,
            "the light does not point at the player",
        );
        // And the direction is the same wherever the player is.
        let far = sun_transform(Vec3::new(-900.0, 300.0, 1_200.0));
        assert!(far.forward().dot(*tf.forward()) > 0.999);
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
