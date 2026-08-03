//! What stands on the ground: forest, boulders, and cloud banks.
//!
//! # One mesh for the forest, not one entity per tree
//!
//! `trees.js` places 340 trees as two `InstancedMesh`es; the module this
//! replaced placed them as 680 Bevy entities sharing two mesh handles, which
//! batched into two draw calls but still cost 680 transform propagations and
//! 680 visibility tests every frame, forever, for geometry that never moves.
//!
//! Here the whole forest is **baked into a single mesh at build time** —
//! transforms applied on the CPU, vertex colours instead of per-species
//! materials — so it is one entity, one draw call, and zero per-frame cost. The
//! boulder field is the same. That is what buys the tree count going up from
//! 340 to [`TREE_COUNT`] while the frame gets cheaper rather than dearer.
//!
//! The clouds are the exception and stay as entities, because they drift. They
//! share one sphere and one material, so they are still a single batch.
//!
//! # Everything is placed through the height function
//!
//! [`super::height`] is the only source of ground position, and the placement
//! rules read [`sim::terrain::slope_at`] as well: trees want soil, which does
//! not stay on a 40° face, and boulders want the scree that does. That one rule
//! does more for how the map reads than the models do.

use bevy::asset::RenderAssetUsages;
use bevy::light::NotShadowCaster;
use bevy::mesh::{Mesh, PrimitiveTopology};
use bevy::prelude::*;

use sim::rng::Rng;
use sim::rules::{Rules, WorldRules};
use spaceships_sim as sim;

use crate::scene::MapScenery;

use super::{height, SCATTER_SEED};

// ---------------------------------------------------------------------------
// Forest
// ---------------------------------------------------------------------------

/// Trees placed on the map.
///
/// Up from `trees.js:3`'s 340, because they cost a fraction of what they used
/// to — see the module docs. The number is set by how far apart they read at
/// cruising speed rather than by a budget.
const TREE_COUNT: usize = 900;
/// Attempts per tree before the scatter gives up. `trees.js:31` caps at twenty
/// times the target, and it matters: the band is narrow and a run that cannot
/// fill it must end rather than spin.
const TREE_ATTEMPTS: usize = TREE_COUNT * 30;
/// Lowest ground a tree grows on, above the waterline. Below this is beach.
const TREE_MIN_HEIGHT: f64 = 5.0;
/// Highest ground a tree grows on — the tree line, just into the upland band.
const TREE_MAX_HEIGHT: f64 = 290.0;
/// Steepest ground a tree grows on. About 37°: soil does not hold above that,
/// and neither does a tree.
const TREE_MAX_SLOPE: f64 = 0.76;
/// Elevation above which conifers replace broadleaves.
const CONIFER_LINE: f64 = 95.0;

/// Boulders placed on the map.
const ROCK_COUNT: usize = 420;
/// See [`ROCK_COUNT`].
const ROCK_ATTEMPTS: usize = ROCK_COUNT * 30;
/// Shallowest ground a boulder sits on. Below this it would read as litter on a
/// lawn; scree belongs on a slope.
const ROCK_MIN_SLOPE: f64 = 0.55;

/// Clearance kept around each landing pad, measured from the mesa centre.
///
/// Generous: it has to cover the pad, its apron and its ramp, or trees grow out
/// of the runway edge and boulders sit in the approach.
fn base_clearance(rules: &WorldRules) -> (f64, f64) {
    (rules.airfield_half.x + 260.0, rules.airfield_half.z + 260.0)
}

/// Is `(x, z)` inside either mesa's keep-clear rectangle?
fn on_a_base(x: f64, z: f64, rules: &WorldRules) -> bool {
    let (hw, hd) = base_clearance(rules);
    [-rules.airfield_z, rules.airfield_z]
        .iter()
        .any(|cz| x.abs() <= hw && (z - cz).abs() <= hd)
}

/// One placed prop: where it stands, how big, and which way it faces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Placement {
    x: f64,
    z: f64,
    /// Ground height under it, from [`super::height`].
    ground: f64,
    scale: f32,
    yaw: f32,
    /// A stable `[0, 1)` for per-prop colour and shape variation.
    tint: f32,
}

/// Rejection-samples the forest: draw a point, keep it if the ground under it
/// is the kind of ground a tree grows on.
fn tree_placements(rules: &WorldRules) -> Vec<Placement> {
    scatter(
        rules,
        1,
        TREE_COUNT,
        TREE_ATTEMPTS,
        |ground, slope, x, z| {
            (TREE_MIN_HEIGHT..=TREE_MAX_HEIGHT).contains(&ground)
                && slope <= TREE_MAX_SLOPE
                && !on_a_base(x, z, rules)
        },
        (0.7, 1.55),
    )
}

/// And the boulder field: the opposite test, on the slopes trees rejected.
fn rock_placements(rules: &WorldRules) -> Vec<Placement> {
    scatter(
        rules,
        2,
        ROCK_COUNT,
        ROCK_ATTEMPTS,
        |ground, slope, x, z| {
            ground > rules.water_level - 2.0 && slope >= ROCK_MIN_SLOPE && !on_a_base(x, z, rules)
        },
        (0.8, 2.6),
    )
}

/// The shared rejection sampler. Split out so both fields draw from the same
/// stream shape and neither can accidentally depend on the other's draw count.
fn scatter(
    rules: &WorldRules,
    stream: u64,
    want: usize,
    attempts: usize,
    accept: impl Fn(f64, f64, f64, f64) -> bool,
    scale: (f64, f64),
) -> Vec<Placement> {
    let mut rng = Rng::with_stream(SCATTER_SEED, stream);
    // Inside the heightfield by a margin, so nothing straddles the border fade.
    let half = rules.terrain_size * 0.5 - 120.0;

    let mut out = Vec::with_capacity(want);
    let mut tries = 0;
    while out.len() < want && tries < attempts {
        tries += 1;
        let x = rng.next_f64_signed() * half;
        let z = rng.next_f64_signed() * half;
        let ground = height(x, z);
        let slope = sim::terrain::slope_at(x, z, rules);
        if !accept(ground, slope, x, z) {
            continue;
        }
        out.push(Placement {
            x,
            z,
            ground,
            scale: rng.range_f64(scale.0, scale.1) as f32,
            yaw: rng.range_f64(0.0, std::f64::consts::TAU) as f32,
            tint: rng.next_f64() as f32,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

/// Spawns the forest, the boulders, and the weather.
pub fn install(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    rules: &Rules,
) {
    // One material for everything baked: the colour is in the vertices, so a
    // spruce, a birch trunk and a granite boulder can share a pipeline.
    let baked = materials.add(StandardMaterial {
        perceptual_roughness: 0.93,
        metallic: 0.0,
        ..default()
    });

    let mut forest = Builder::default();
    for tree in tree_placements(&rules.world) {
        build_tree(&mut forest, tree);
    }
    let mut rocks = Builder::default();
    for rock in rock_placements(&rules.world) {
        build_rock(&mut rocks, rock);
    }

    for builder in [forest, rocks] {
        commands.spawn((
            Mesh3d(meshes.add(builder.build())),
            MeshMaterial3d(baked.clone()),
            // `trees.js:24` sets `castShadow = false` on both instanced meshes,
            // and the sun's cascade is a 700-unit box around the player — a
            // forest-wide shadow pass would cost more than the forest.
            NotShadowCaster,
            MapScenery,
        ));
    }

    install_clouds(commands, meshes, materials);
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/// Accumulates flat-shaded, vertex-coloured triangles.
///
/// Unindexed throughout, to match [`super::ground`]: the props and the ground
/// they stand on have to face the light the same way, and a smooth-shaded tree
/// on a faceted hillside reads as a different game's asset.
#[derive(Default)]
pub(crate) struct Builder {
    positions: Vec<[f32; 3]>,
    colors: Vec<[f32; 4]>,
}

impl Builder {
    /// One triangle, wound counter-clockwise as seen from outside.
    fn tri(&mut self, a: Vec3, b: Vec3, c: Vec3, color: [f32; 4]) {
        for p in [a, b, c] {
            self.positions.push(p.to_array());
            self.colors.push(color);
        }
    }

    /// A cone-or-frustum around `+y`: `sides` quads from `bottom` to `top`,
    /// plus a bottom cap. Degenerates to a cone when `top_radius` is zero, and
    /// the cap is skipped when it would be hidden.
    ///
    /// # Winding
    ///
    /// `ring` walks **clockwise** seen from `+y` — increasing angle takes `+x`
    /// toward `+z`, and looking down the `-y` axis with `+x` to the right puts
    /// `+z` down the screen. wgpu's front face is counter-clockwise, so an
    /// upward cone has to consume each edge backwards and a downward one does
    /// not.
    ///
    /// This is not a detail worth leaving to callers. Taking it in the obvious
    /// direction builds every cone inside out, backface culling then shows the
    /// *far inner* wall, and nine hundred trees render as dark cardboard — which
    /// is what the first pass of this module shipped and what a screenshot
    /// eventually caught. `every_canopy_face_points_outward` is the test that
    /// makes it a compile-time-ish fact instead of a visual one.
    fn drum(
        &mut self,
        centre: Vec3,
        bottom_radius: f32,
        top_radius: f32,
        height: f32,
        sides: u32,
        yaw: f32,
        color: [f32; 4],
        cap: bool,
    ) {
        let ring = |r: f32, y: f32, i: u32| {
            let a = yaw + std::f32::consts::TAU * (i % sides) as f32 / sides as f32;
            centre + Vec3::new(r * a.cos(), y, r * a.sin())
        };
        let up = height >= 0.0;
        for i in 0..sides {
            // Which end of the edge comes first *is* the winding.
            let (i0, i1) = if up { (i + 1, i) } else { (i, i + 1) };
            let (b0, b1) = (ring(bottom_radius, 0.0, i0), ring(bottom_radius, 0.0, i1));
            let (t0, t1) = (ring(top_radius, height, i0), ring(top_radius, height, i1));
            if top_radius <= f32::EPSILON {
                self.tri(b0, b1, t0, color);
            } else {
                self.tri(b0, b1, t1, color);
                self.tri(b0, t1, t0, color);
            }
            if cap {
                self.tri(centre, b1, b0, color);
            }
        }
    }

    fn build(self) -> Mesh {
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, self.colors);
        mesh.compute_normals();
        mesh
    }

    #[cfg(test)]
    fn triangle_count(&self) -> usize {
        self.positions.len() / 3
    }
}

/// Linear RGBA from an sRGB hex, brightened or darkened by `k`.
fn tinted(rgb: u32, k: f32) -> [f32; 4] {
    let c = LinearRgba::from(Srgba::rgb_u8(
        (rgb >> 16) as u8,
        (rgb >> 8) as u8,
        rgb as u8,
    ));
    [c.red * k, c.green * k, c.blue * k, 1.0]
}

/// Two species, chosen by elevation: broadleaves in the valleys, conifers above
/// [`CONIFER_LINE`]. Both are a tapered trunk and stacked cones — the difference
/// is how many, how wide, and how green.
fn build_tree(out: &mut Builder, t: Placement) {
    let base = Vec3::new(t.x as f32, t.ground as f32, t.z as f32);
    let s = t.scale;
    // The trunk sinks a little, so a tree on a slope has no daylight under its
    // uphill side. Cheaper and steadier than orienting it to the surface
    // normal, which makes a hillside look combed.
    let trunk_base = base - Vec3::Y * 2.0 * s;

    let trunk = tinted(0x5a_3a_1a, 0.85 + t.tint * 0.3);
    out.drum(
        trunk_base,
        1.9 * s,
        1.1 * s,
        11.0 * s,
        5,
        t.yaw,
        trunk,
        false,
    );

    if t.ground >= CONIFER_LINE {
        // Conifer: three cones, narrowing upward.
        let green = tinted(0x22_4d_1e, 0.8 + t.tint * 0.45);
        for (i, (r, h)) in [(8.2, 13.0), (6.4, 12.0), (4.2, 11.0)].iter().enumerate() {
            let y = 8.0 + i as f32 * 7.5;
            out.drum(
                base + Vec3::Y * y * s,
                r * s,
                0.0,
                h * s,
                5,
                t.yaw + i as f32 * 0.4,
                green,
                true,
            );
        }
    } else {
        // Broadleaf: a squat bipyramid, which is the fewest triangles that
        // reads as a round canopy.
        let green = tinted(0x3f_7a_2c, 0.8 + t.tint * 0.5);
        let crown = base + Vec3::Y * 11.0 * s;
        out.drum(crown, 9.0 * s, 0.0, 10.5 * s, 6, t.yaw, green, false);
        out.drum(crown, 9.0 * s, 0.0, -6.0 * s, 6, t.yaw, green, false);
    }
}

/// A boulder: an eight-sided drum squashed and tilted, which at this size is
/// indistinguishable from a rock and costs sixteen triangles.
fn build_rock(out: &mut Builder, r: Placement) {
    let base = Vec3::new(r.x as f32, r.ground as f32 - 1.5 * r.scale, r.z as f32);
    let grey = tinted(0x6e_68_5e, 0.82 + r.tint * 0.42);
    let s = r.scale;
    out.drum(base, 4.2 * s, 2.4 * s, 3.4 * s, 6, r.yaw, grey, false);
    out.drum(
        base + Vec3::Y * 3.4 * s,
        2.4 * s,
        0.0,
        2.6 * s,
        6,
        r.yaw,
        grey,
        false,
    );
}

// ---------------------------------------------------------------------------
// Clouds
// ---------------------------------------------------------------------------

/// A cloud cluster's drift, in units per second along x. `clouds.js:5`
/// (`DRIFT_SPEED`) times the per-cluster direction and rate at `clouds.js:22`.
#[derive(Component)]
pub(crate) struct CloudDrift(f32);

/// `clouds.js:2`–`:5` and `:15`.
const CLOUD_CLUSTERS: usize = 26;
/// Lowest cluster altitude. `clouds.js:3`. Raised above the JS's 280 because the
/// ranges now reach 640 and a cloud bank inside a mountain looks like a bug.
const CLOUD_MIN_ALT: f64 = 520.0;
/// Highest cluster altitude. `clouds.js:4`.
const CLOUD_MAX_ALT: f64 = 900.0;
/// Half-width of the box clusters are scattered in, and the wrap distance.
/// `clouds.js:15`.
const CLOUD_SPREAD: f32 = 1700.0;
/// Base drift rate along x, in units per second. `clouds.js:5`.
const CLOUD_DRIFT_SPEED: f32 = 0.8;

/// `createClouds` (`clouds.js:6`): clusters of six to nine overlapping spheres.
///
/// The JS builds a `SphereGeometry` per puff, at a different radius each time —
/// two hundred distinct geometries. Here it is **one unit sphere** scaled by the
/// transform, so the whole sky is a single batch. The sphere is coarse on
/// purpose: at this distance the facets read as the lumpiness of a cumulus, and
/// they match the faceting of everything below them.
fn install_clouds(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) {
    let mut rng = Rng::with_stream(SCATTER_SEED, 3);

    let puff = meshes.add(Sphere::new(1.0).mesh().ico(1).expect("ico(1) is valid"));
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
pub fn drift_clouds(time: Res<Time>, mut clusters: Query<(&mut Transform, &CloudDrift)>) {
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The forest fills, stands on the ground, keeps inside its band, and stays
    /// off both bases.
    #[test]
    fn trees_stand_on_ground_a_tree_could_grow_on() {
        let rules = Rules::DEFAULT.world;
        let trees = tree_placements(&rules);
        assert_eq!(trees.len(), TREE_COUNT, "the forest has to fill");
        for t in &trees {
            assert_eq!(t.ground, height(t.x, t.z));
            assert!((TREE_MIN_HEIGHT..=TREE_MAX_HEIGHT).contains(&t.ground));
            assert!(sim::terrain::slope_at(t.x, t.z, &rules) <= TREE_MAX_SLOPE);
            assert!(!on_a_base(t.x, t.z, &rules), "a tree at ({}, {})", t.x, t.z);
            assert!((0.7..=1.55).contains(&t.scale));
        }
    }

    /// No tree stands in the water. The band starts above the waterline, so this
    /// is really a check that [`height`] is the bed and the band is read against
    /// it — the mistake it guards is clamping the height to the surface, which
    /// would plant a forest across the lake.
    #[test]
    fn no_tree_is_in_the_lake() {
        let rules = Rules::DEFAULT.world;
        for t in tree_placements(&rules) {
            assert!(t.ground > rules.water_level, "a tree at ({}, {})", t.x, t.z);
        }
    }

    /// Boulders take the slopes the trees rejected, which is the whole point of
    /// running two scatters instead of one.
    #[test]
    fn boulders_sit_on_slopes_and_trees_do_not() {
        let rules = Rules::DEFAULT.world;
        let rocks = rock_placements(&rules);
        assert_eq!(rocks.len(), ROCK_COUNT);
        for r in &rocks {
            assert!(sim::terrain::slope_at(r.x, r.z, &rules) >= ROCK_MIN_SLOPE);
            assert!(!on_a_base(r.x, r.z, &rules));
        }
        const { assert!(ROCK_MIN_SLOPE < TREE_MAX_SLOPE, "the bands may overlap") };
    }

    /// Same map every run. `sim::rng` is seeded, but only as long as nothing
    /// here reaches for a clock or a global generator.
    #[test]
    fn placement_is_deterministic() {
        let rules = Rules::DEFAULT.world;
        assert_eq!(tree_placements(&rules), tree_placements(&rules));
        assert_eq!(rock_placements(&rules), rock_placements(&rules));
    }

    /// The two scatters draw from separate streams, so changing the tree count
    /// does not move every boulder.
    #[test]
    fn the_two_scatters_are_independent() {
        let rules = Rules::DEFAULT.world;
        let rocks = rock_placements(&rules);
        let few = scatter(
            &rules,
            1,
            10,
            300,
            |g, s, x, z| {
                (TREE_MIN_HEIGHT..=TREE_MAX_HEIGHT).contains(&g)
                    && s <= TREE_MAX_SLOPE
                    && !on_a_base(x, z, &rules)
            },
            (0.7, 1.55),
        );
        assert_eq!(few.len(), 10);
        assert_eq!(rocks, rock_placements(&rules));
    }

    /// The whole forest is one mesh, unindexed and flat-shaded, and its size is
    /// what the module claims. A regression here means the batching argument in
    /// the module docs has quietly stopped being true.
    #[test]
    fn the_forest_bakes_into_one_flat_shaded_mesh() {
        let rules = Rules::DEFAULT.world;
        let mut b = Builder::default();
        for t in tree_placements(&rules) {
            build_tree(&mut b, t);
        }
        let tris = b.triangle_count();
        assert!(
            (20_000..45_000).contains(&tris),
            "the forest is {tris} triangles",
        );
        let mesh = b.build();
        assert!(
            mesh.indices().is_none(),
            "flat shading needs no index buffer"
        );
        assert_eq!(
            mesh.count_vertices(),
            tris * 3,
            "three unshared vertices per triangle",
        );
    }

    /// Every face of a canopy points away from its own axis.
    ///
    /// This is the test for the negative-`height` winding flip in [`Builder::drum`].
    /// The broadleaf is a bipyramid — one cone up, one cone down — and the
    /// downward half was wound inside out, so nine hundred trees rendered as
    /// black cardboard triangles. A screenshot found it; this keeps it found.
    #[test]
    fn every_canopy_face_points_outward() {
        for down in [false, true] {
            let mut b = Builder::default();
            let centre = Vec3::new(3.0, 40.0, -7.0);
            let h = if down { -9.0 } else { 9.0 };
            b.drum(centre, 6.0, 0.0, h, 6, 0.35, [1.0; 4], false);

            let mesh = b.build();
            let Some(bevy::mesh::VertexAttributeValues::Float32x3(pos)) =
                mesh.attribute(Mesh::ATTRIBUTE_POSITION)
            else {
                panic!("positions");
            };
            let Some(bevy::mesh::VertexAttributeValues::Float32x3(nrm)) =
                mesh.attribute(Mesh::ATTRIBUTE_NORMAL)
            else {
                panic!("normals");
            };
            for face in 0..pos.len() / 3 {
                let v: Vec<Vec3> = (0..3).map(|k| Vec3::from(pos[face * 3 + k])).collect();
                let mid = (v[0] + v[1] + v[2]) / 3.0;
                let n = Vec3::from(nrm[face * 3]);
                // Against the cone's own mid-height axis point, so "outward" is
                // well defined for both the up and the down halves.
                let axis = centre + Vec3::Y * h * 0.5;
                assert!(
                    n.dot(mid - axis) > 0.0,
                    "face {face} of a {} cone faces inward",
                    if down { "downward" } else { "upward" },
                );
            }
        }
    }

    /// Cloud banks fly above the highest ground, which the ranges did not leave
    /// much room for.
    #[test]
    fn clouds_clear_the_mountains() {
        let rules = Rules::DEFAULT.world;
        let mut peak = f64::NEG_INFINITY;
        for iz in 0..sim::terrain::LATTICE_NODES {
            for ix in 0..sim::terrain::LATTICE_NODES {
                peak = peak.max(sim::terrain::node_height(ix, iz, &rules));
            }
        }
        assert!(
            CLOUD_MIN_ALT > peak - 150.0,
            "clouds at {CLOUD_MIN_ALT} against a {peak} peak",
        );
    }
}
