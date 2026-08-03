//! The two team bases: a landing pad on top of a mesa, and the buildings round
//! it.
//!
//! Ports `createAirfield` (`airfield.js:3`), twice — team 0 at `-airfield_z`
//! and team 1 at `+airfield_z`, the far one turned to face the middle
//! (`main.js:150`).
//!
//! # The pad is the collision box
//!
//! `airfield.js:19` draws a `560 x 3 x 380` slab with its top face at `y = 0`,
//! while [`WorldRules::airfield_half`] — the box `resolve_world_collisions`
//! bounces ships off — is `(280, 4, 190)`. The width and depth already agree
//! exactly; only the *height* did not, and the JS drew a pad three units thick
//! where the solid is eight. So the slab is derived from the rules instead, the
//! same way `scene.rs` derives the mothership hull, and the surface you land on
//! is the surface that stops you.
//!
//! # It is a mesa now, not a hole
//!
//! The JS flattened both airfields to `y = 0` inside a heightfield whose hills
//! reached 500, so each team spawned at the bottom of a smooth circular pit with
//! high ground on every bearing — which is the opposite of what a base is for.
//! [`sim::terrain`] builds a plateau at [`WorldRules::airfield_elevation`]
//! instead, with the ground ramping away from its rim on three sides and the sea
//! behind it.
//!
//! Everything here therefore sits at that elevation rather than at zero. The
//! flat ground under it is still not this module's doing: the heightfield
//! already holds the apron level, which is why the pad can be a plain box.
//!
//! # Why the bases are still entity trees
//!
//! [`super::props`] bakes the forest into one mesh because a tree is four
//! primitives and there are nine hundred of them. A base is sixty-odd
//! primitives across a dozen materials, twice — merging them would buy one draw
//! call per material at the cost of losing the transform hierarchy that makes
//! the two fields mirror each other. The shared-handle pattern already collapses
//! them into roughly 22 batches, which is where the cost stops mattering.
//!
//! [`WorldRules::airfield_half`]: sim::rules::WorldRules::airfield_half
//! [`WorldRules::airfield_elevation`]: sim::rules::WorldRules::airfield_elevation

use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use std::f32::consts::{FRAC_PI_3, PI};

use sim::rules::Rules;
use spaceships_sim as sim;

use crate::scene::{glow, hex, MapScenery, GLOW_BOOST};

/// Spawns both bases.
pub fn install(
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
    // Everything below is authored about a pad whose top face is at the local
    // origin, exactly as `airfield.js` is. The mesa's elevation goes on the root
    // transform, once, so nothing inside has to know about it.
    let deck = rules.world.airfield_elevation as f32;

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
                Transform::from_xyz(0.0, deck, z as f32).with_rotation(facing),
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::mesh::Mesh;

    fn bounds(mesh: &Mesh) -> ([f32; 3], [f32; 3]) {
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(p)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the mesh must carry float positions");
        };
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for v in p {
            for axis in 0..3 {
                lo[axis] = lo[axis].min(v[axis]);
                hi[axis] = hi[axis].max(v[axis]);
            }
        }
        (lo, hi)
    }

    /// The pad the renderer draws is the box the simulation bounces ships off.
    #[test]
    fn the_pad_is_the_collision_box() {
        let half = Rules::DEFAULT.world.airfield_half;
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
    }

    /// The base is drawn where the simulation's landing box is, and the mesa
    /// under both is flat across the whole footprint.
    ///
    /// Three numbers have to agree — the heightfield's plateau, the collision
    /// box's `y`, and the transform this module puts the pad at — and they are
    /// set in three different crates. This is the test that notices when one of
    /// them moves.
    #[test]
    fn the_base_sits_on_the_mesa_the_simulation_built() {
        let rules = Rules::DEFAULT;
        let deck = rules.world.airfield_elevation;
        let world = sim::world::World::new(
            1,
            rules,
            sim::world::Mode::Skirmish,
            sim::world::MapKind::Terrain,
        );
        assert_eq!(world.boxes.len(), 2);
        for b in &world.boxes {
            assert_eq!(b.pos.y, deck, "the landing box is off the mesa");
        }

        let half = rules.world.airfield_half;
        for cz in [-rules.world.airfield_z, rules.world.airfield_z] {
            for i in 0..=8 {
                for j in 0..=8 {
                    let x = -half.x + f64::from(i) / 8.0 * 2.0 * half.x;
                    let z = cz - half.z + f64::from(j) / 8.0 * 2.0 * half.z;
                    // A tolerance and not an equality: away from a lattice
                    // node the height is `a*(1-u-v) + b*u + c*v`, and three
                    // identical corner values do not sum back to exactly one of
                    // them in floating point. A part in 10^11 is flat.
                    let h = super::super::height(x, z);
                    assert!(
                        (h - deck).abs() < 1e-9,
                        "the mesa is {h}, not {deck}, under the pad at ({x}, {z})",
                    );
                }
            }
        }
    }

    /// The mesa is high ground, which is the entire reason it exists.
    #[test]
    fn the_mesa_stands_above_the_ground_it_launches_over() {
        let rules = Rules::DEFAULT.world;
        let deck = rules.airfield_elevation;
        for cz in [-rules.airfield_z, rules.airfield_z] {
            // Toward the middle of the map, past the ramp.
            let ahead = cz - cz.signum() * (rules.airfield_half.z + 420.0);
            assert!(
                super::super::height(0.0, ahead) < deck,
                "the ground ahead of the pad at z={cz} is above it",
            );
        }
    }
}
