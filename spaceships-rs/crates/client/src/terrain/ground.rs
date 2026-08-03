//! The ground and the water: two meshes, two draw calls, and the whole look of
//! the map.
//!
//! # The mesh is the surface
//!
//! [`sim::terrain`] defines the map as a triangulated lattice rather than as a
//! smooth function sampled onto one, so this module's only job is to draw the
//! triangles that are already there. Every vertex comes from
//! [`sim::terrain::node_height`] at a lattice node, and the triangulation
//! matches [`sim::terrain::ground_height`]'s cell split exactly. There is no
//! approximation to measure and no error budget to defend — which is the whole
//! reason the heightfield was rebuilt the way it was.
//!
//! What that replaced: one `PlaneGeometry(3600, 3600, 384, 384)` at 295,000
//! triangles, chosen because that was the first resolution at which a chord
//! through a sine field stayed inside the five-unit kill clearance. The lattice
//! is 45,000 triangles and exact. `the_drawn_surface_is_the_simulation_surface`
//! is what holds the claim.
//!
//! # Flat shading, and what it costs
//!
//! The mesh is **unindexed**, so every triangle carries its own three vertices
//! and its own normal, and the facets read as facets. That is the low-poly look,
//! and it is also honest: a smooth-shaded heightfield draws a curved surface
//! that the simulation does not have, so a ship clips a hill that visibly is not
//! there.
//!
//! Unindexed costs three vertices per triangle instead of the ~1.02 an indexed
//! mesh averages. 45,000 triangles is 135,000 vertices at 40 bytes — 5.4 MB,
//! against the 9 MB the indexed 384-segment mesh uploaded. Cheaper *and*
//! faceted, because the triangle count fell by 85% and that dominates.
//!
//! It also buys **per-triangle colour**, which is the thing that makes this
//! style work: [`face_color`] reads the centroid's height and the face's own
//! slope, so a cliff is rock and the meadow below it is not, with a hard edge
//! between them. A smooth mesh cannot do that — it has to blend, and blended
//! bands on a heightfield read as contour lines.
//!
//! # Water
//!
//! One quad at [`WorldRules::water_level`], reaching past [`super::FOG_END`], and
//! it is **opaque**. Stylised water is not a transparency effect; it is a
//! reflective surface with a flat body colour, which a low roughness and the sky
//! environment map give for free. Opaque also means no transparent-pass sorting,
//! no depth-write games, and no fill cost beyond one full-screen-ish quad — on a
//! map where the sea reaches the horizon that is not a small saving.
//!
//! The terrain fades *below* the waterline at the map border rather than to it,
//! so the two surfaces are never coplanar and there is no z-fighting ring around
//! the island. That is [`sim::terrain`]'s doing, not this module's; see
//! `BORDER_SUBMERGE` there.
//!
//! [`WorldRules::water_level`]: sim::rules::WorldRules::water_level

use bevy::asset::RenderAssetUsages;
use bevy::light::NotShadowCaster;
use bevy::mesh::{Mesh, PrimitiveTopology};
use bevy::prelude::*;

use sim::rules::{Rules, WorldRules};
use sim::terrain;
use spaceships_sim as sim;

use crate::scene::MapScenery;

use super::FOG_END;

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

/// One elevation band: the height **above the waterline** at which it starts,
/// and its colour.
///
/// Above the waterline, not above zero. They happen to be the same number today
/// because [`WorldRules::water_level`] is 0, and a table written in absolute
/// heights would go on looking right until the day it moved — at which point the
/// beach band would sit somewhere in the hills. [`face_color`] is handed the
/// relative height so the table cannot be read any other way.
///
/// [`WorldRules::water_level`]: sim::rules::WorldRules::water_level
///
/// Authored in sRGB — which is how colour is picked — and converted to linear
/// once, at build time, because `Mesh::ATTRIBUTE_COLOR` is a linear multiplier
/// into `base_color` and putting sRGB bytes there washes the whole map out.
struct Band {
    /// Lowest height this band covers.
    from: f32,
    /// sRGB hex.
    rgb: u32,
}

/// The ground ramp, bottom to top: lake bed, sand, meadow, forest, upland
/// pasture, scree, bare rock, snow.
///
/// Bands are picked, not interpolated. A gradient over a heightfield draws
/// contour lines — the eye finds the iso-height curves immediately — while hard
/// bands on triangles the size of these read as terrain type, which is what they
/// are meant to be. Between two bands the boundary is a triangle edge, so it is
/// as jagged as the ground is.
const GROUND: &[Band] = &[
    Band {
        from: f32::NEG_INFINITY,
        rgb: 0x4a_5f_46,
    },
    Band {
        from: -6.0,
        rgb: 0xc8_bd_8e,
    },
    Band {
        from: 6.0,
        rgb: 0x6d_9c_4a,
    },
    Band {
        from: 52.0,
        rgb: 0x4f_82_3c,
    },
    Band {
        from: 165.0,
        rgb: 0x6d_8c_46,
    },
    Band {
        from: 268.0,
        rgb: 0x8a_84_6a,
    },
    Band {
        from: 372.0,
        rgb: 0x86_82_7e,
    },
    Band {
        from: 470.0,
        rgb: 0xe8_ee_f2,
    },
];

/// Slope above which a face is rock whatever its elevation, and the colour it
/// takes.
///
/// A gradient of 0.95 is about 44°. This is the single rule that does the most
/// for how the map reads: without it, a cliff face at meadow height is a green
/// wall, and every mountain looks like a hill wearing the wrong colour. Grass
/// does not hold on a 44° face, and neither does snow — which is why the check
/// runs after the elevation bands and overrides them.
const ROCK_SLOPE: f32 = 0.95;
/// See [`ROCK_SLOPE`].
const ROCK_RGB: u32 = 0x7a_72_66;
/// Slope above which the rock darkens again — a sheer face, in shadow more often
/// than not.
const CLIFF_SLOPE: f32 = 1.9;
/// See [`CLIFF_SLOPE`].
const CLIFF_RGB: u32 = 0x6b_63_58;

/// Peak-to-peak brightness variation between neighbouring faces.
///
/// Low-poly terrain is usually drawn with a per-face tint, and it is doing real
/// work rather than decoration: adjacent coplanar triangles are otherwise
/// literally the same colour and the same normal, so a flat plain reads as one
/// undifferentiated sheet and the facets disappear exactly where they are the
/// only thing to look at.
const FACE_JITTER: f32 = 0.085;

/// Colour for one face, from its centroid height *above the waterline* and its
/// slope.
///
/// Returns linear RGBA, ready for `Mesh::ATTRIBUTE_COLOR`.
fn face_color(above_water: f32, slope: f32, jitter: f32) -> [f32; 4] {
    let mut rgb = GROUND
        .iter()
        .rev()
        .find(|b| above_water >= b.from)
        .map_or(GROUND[0].rgb, |b| b.rgb);
    if slope >= CLIFF_SLOPE {
        rgb = CLIFF_RGB;
    } else if slope >= ROCK_SLOPE {
        rgb = ROCK_RGB;
    }
    let c = LinearRgba::from(Srgba::rgb_u8(
        (rgb >> 16) as u8,
        (rgb >> 8) as u8,
        rgb as u8,
    ));
    let k = 1.0 + jitter * FACE_JITTER;
    [c.red * k, c.green * k, c.blue * k, 1.0]
}

/// A stable, cheap `[-1, 1]` from a face index. Not [`sim::rng`]: this wants a
/// value *per face position in the buffer*, not a stream, so that inserting a
/// triangle does not recolour every triangle after it.
fn face_jitter(index: u32) -> f32 {
    let mut h = index.wrapping_mul(0x9E37_79B1);
    h ^= h >> 15;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    f32::from((h >> 8) as u16) / 32_768.0 - 1.0
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

/// Spawns the ground and the sea.
pub fn install(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    rules: &Rules,
) {
    commands.spawn((
        Mesh3d(meshes.add(ground_mesh(&rules.world))),
        MeshMaterial3d(materials.add(StandardMaterial {
            perceptual_roughness: 0.94,
            metallic: 0.0,
            ..default()
        })),
        // A 45,000-triangle mesh in the shadow pass would double the map's cost
        // to draw its own shadow onto itself. The sun's cascade is a tight box
        // around the player and only ships need to cast into it.
        NotShadowCaster,
        MapScenery,
    ));

    commands.spawn((
        Mesh3d(meshes.add(water_mesh(&rules.world))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Srgba::rgb_u8(0x1d, 0x53, 0x7a).into(),
            // Smooth and slightly metallic so the sky cubemap lands on it as a
            // sheen. This is what makes an opaque quad read as water rather than
            // as blue lino — there is no normal map and no animation, and at
            // this scale neither is missed.
            //
            // Not *mirror* smooth, though. At 0.10 roughness and 0.30 metallic
            // the grazing angles out toward the horizon returned almost the
            // whole sky, and the open sea behind the island rendered as a flat
            // white band that read as a hole in the world. Rougher and less
            // metallic keeps the near lake's colour while letting the distance
            // stay water.
            perceptual_roughness: 0.22,
            metallic: 0.12,
            reflectance: 0.5,
            ..default()
        })),
        NotShadowCaster,
        MapScenery,
    ));
}

// ---------------------------------------------------------------------------
// Meshes
// ---------------------------------------------------------------------------

/// The heightfield, one unindexed triangle at a time.
///
/// The winding — `(a, c, b)` and `(b, c, d)` — is what makes the front face
/// counter-clockwise seen from `+y` with `+x` right and `+z` away, and it is
/// also the split [`sim::terrain::ground_height`] interpolates along. Those two
/// facts have to stay together: reversing the winding here would flip the map
/// inside out, and re-splitting the cell would put the drawn surface on the
/// other side of every diagonal from the surface that kills you.
fn ground_mesh(rules: &WorldRules) -> Mesh {
    let n = terrain::LATTICE_NODES;
    let segs = terrain::LATTICE_SEGMENTS;
    let tris = (segs * segs * 2) as usize;

    // One f64 evaluation per node rather than per vertex: each node is shared by
    // up to six triangles, and `node_height` is the expensive call on this path.
    let mut node = Vec::with_capacity((n * n) as usize);
    for iz in 0..n {
        for ix in 0..n {
            node.push(terrain::node_height(ix, iz, rules));
        }
    }
    let at = |ix: u32, iz: u32| node[(iz * n + ix) as usize];
    let step = terrain::lattice_step(rules);

    let mut positions = Vec::with_capacity(tris * 3);
    let mut colors = Vec::with_capacity(tris * 3);
    let mut face = 0u32;

    for iz in 0..segs {
        for ix in 0..segs {
            let (x0, x1) = (
                terrain::node_pos(ix, rules),
                terrain::node_pos(ix + 1, rules),
            );
            let (z0, z1) = (
                terrain::node_pos(iz, rules),
                terrain::node_pos(iz + 1, rules),
            );
            let (ha, hb) = (at(ix, iz), at(ix + 1, iz));
            let (hc, hd) = (at(ix, iz + 1), at(ix + 1, iz + 1));

            let a = [x0, ha, z0];
            let b = [x1, hb, z0];
            let c = [x0, hc, z1];
            let d = [x1, hd, z1];

            for tri in [[a, c, b], [b, c, d]] {
                let mid = (tri[0][1] + tri[1][1] + tri[2][1]) / 3.0;
                // Slope from the two lattice differences the triangle spans,
                // which is the exact gradient of the plane being drawn — no
                // resampling, and no disagreement with the face's own normal.
                let (dx, dz) = if face.is_multiple_of(2) {
                    (hb - ha, hc - ha)
                } else {
                    (hd - hc, hd - hb)
                };
                let slope = ((dx * dx + dz * dz).sqrt() / step) as f32;
                let color = face_color((mid - rules.water_level) as f32, slope, face_jitter(face));
                for p in tri {
                    positions.push([p[0] as f32, p[1] as f32, p[2] as f32]);
                    colors.push(color);
                }
                face += 1;
            }
        }
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    // Unindexed, so this is `compute_flat_normals` — one normal per triangle,
    // repeated across its three vertices. That is the whole faceted look.
    mesh.compute_normals();
    mesh
}

/// The sea: one quad, out to where the fog is total.
///
/// It reaches past the island on every side, so the horizon is water fading into
/// fog rather than an edge. The old module needed a four-sided cliff plus four
/// plain strips to close the map edge, all because the terrain stopped at ±1800
/// while the height function kept going; here the terrain ends underwater and
/// the sea covers the join.
fn water_mesh(rules: &WorldRules) -> Mesh {
    let r = FOG_END;
    let y = rules.water_level as f32;
    let positions = vec![
        [-r, y, -r],
        [-r, y, r],
        [r, y, -r],
        [r, y, -r],
        [-r, y, r],
        [r, y, r],
    ];
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; 6])
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 0.0]; 6]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[1.0, 1.0, 1.0, 1.0]; 6]);
    mesh
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn positions(mesh: &Mesh) -> &[[f32; 3]] {
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(p)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the mesh must carry float positions");
        };
        p
    }

    /// **The test this module exists for**, and the one that replaced a table of
    /// error percentiles: every drawn vertex is exactly where the simulation
    /// says the ground is, to the `f32` the mesh stores it in.
    ///
    /// The old module could only assert this at its own vertices and had to
    /// *measure* the disagreement everywhere else. Here there is nowhere else —
    /// the surface between vertices is the triangle, and
    /// `sim::terrain`'s own `between_nodes_the_surface_is_the_triangles_plane`
    /// pins that half.
    #[test]
    fn the_drawn_surface_is_the_simulation_surface() {
        let rules = Rules::DEFAULT.world;
        let mesh = ground_mesh(&rules);
        for [x, y, z] in positions(&mesh) {
            assert_eq!(
                *y,
                terrain::ground_height(f64::from(*x), f64::from(*z), &rules) as f32,
                "vertex at ({x}, {z})",
            );
        }
    }

    /// Unindexed and the size the lattice says, which is what flat shading and
    /// per-face colour both depend on.
    #[test]
    fn the_ground_is_one_unindexed_triangle_per_lattice_face() {
        let mesh = ground_mesh(&Rules::DEFAULT.world);
        let segs = terrain::LATTICE_SEGMENTS as usize;
        assert!(
            mesh.indices().is_none(),
            "flat shading needs no index buffer"
        );
        assert_eq!(positions(&mesh).len(), segs * segs * 2 * 3);
    }

    /// Every triangle's normal points up, which says the winding is right. A
    /// reversed winding draws the map from underneath and is invisible from
    /// above — the single easiest thing to get wrong here.
    #[test]
    fn every_face_is_wound_upward() {
        let mesh = ground_mesh(&Rules::DEFAULT.world);
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(normals)) =
            mesh.attribute(Mesh::ATTRIBUTE_NORMAL)
        else {
            panic!("compute_normals must have run");
        };
        for n in normals {
            assert!(n[1] > 0.0, "a face points down: {n:?}");
        }
    }

    /// The mesh spans the heightfield and no more.
    #[test]
    fn the_ground_covers_the_map_exactly() {
        let mesh = ground_mesh(&Rules::DEFAULT.world);
        let half = (Rules::DEFAULT.world.terrain_size * 0.5) as f32;
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for p in positions(&mesh) {
            lo = lo.min(p[0]).min(p[2]);
            hi = hi.max(p[0]).max(p[2]);
        }
        assert_eq!(lo, -half);
        assert_eq!(hi, half);
    }

    /// The sea reaches past the fog on every side, so the map's edge is never
    /// the horizon.
    #[test]
    fn the_sea_reaches_past_the_fog() {
        let rules = Rules::DEFAULT.world;
        let mesh = water_mesh(&rules);
        for p in positions(&mesh) {
            assert_eq!(p[1], rules.water_level as f32);
            assert!(p[0].abs() >= FOG_END && p[2].abs() >= FOG_END);
        }
        assert!(FOG_END > (rules.terrain_size * 0.5) as f32);
    }

    /// The island's edge is under the sea, not level with it: coplanar surfaces
    /// z-fight, and a ring of it around the whole map is the most visible bug a
    /// flat water plane can have.
    #[test]
    fn the_map_edge_sits_below_the_waterline() {
        let rules = Rules::DEFAULT.world;
        let n = terrain::LATTICE_SEGMENTS;
        for i in 0..=n {
            for (ix, iz) in [(0, i), (n, i), (i, 0), (i, n)] {
                let h = terrain::node_height(ix, iz, &rules);
                assert!(
                    h < rules.water_level - 1.0,
                    "border node ({ix}, {iz}) is at {h}, level with the sea",
                );
            }
        }
    }

    /// Slope beats elevation: a cliff is rock at any height, and snow does not
    /// cling to a vertical face.
    #[test]
    fn a_steep_face_is_rock_whatever_its_height() {
        for h in [10.0, 120.0, 300.0, 600.0] {
            assert_eq!(face_color(h, 2.5, 0.0), face_color(h, 3.0, 0.0));
            assert_ne!(face_color(h, 0.1, 0.0), face_color(h, 2.5, 0.0));
        }
        // And a flat summit still gets its snow.
        assert_ne!(face_color(600.0, 0.1, 0.0), face_color(600.0, 1.2, 0.0));
    }

    /// The bands are ordered and total, so no height falls through the lookup.
    #[test]
    fn every_height_lands_in_a_band() {
        for w in GROUND.windows(2) {
            assert!(w[0].from < w[1].from, "bands must ascend");
        }
        for h in [-9_999.0, -50.0, 0.0, 100.0, 480.0, 9_999.0] {
            let c = face_color(h, 0.0, 0.0);
            assert!(c.iter().all(|v| v.is_finite()), "no band covers {h}");
        }
    }

    /// Neighbouring faces differ, or the facets are invisible on flat ground.
    #[test]
    fn the_face_jitter_actually_varies() {
        let spread = (0..64).map(face_jitter).fold(f32::MIN, f32::max)
            - (0..64).map(face_jitter).fold(f32::MAX, f32::min);
        assert!(spread > 1.0, "jitter spread is only {spread}");
        assert!((0..4_096)
            .map(face_jitter)
            .all(|j| (-1.0..=1.0).contains(&j)));
        // Stable: the same face index always gets the same tint, so a rebuild
        // does not reshuffle the whole map.
        assert_eq!(face_jitter(1_234), face_jitter(1_234));
    }
}
