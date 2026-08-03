//! The Sierras heightfield: what the ground *is*, on the terrain map.
//!
//! This replaces the seven-octave sine sum that `public/src/terrain.js` shipped
//! and that [`crate::ship`] transcribed. That surface had three problems, and
//! all three are structural rather than a matter of taste:
//!
//! 1. **It was made of `sin`.** Eleven transcendental calls per sample, on a
//!    function the kill plane is computed from, in a crate whose one rule is
//!    bit-identical output across glibc, musl, Apple's libm and WASM. It was the
//!    single largest determinism hazard left in the simulation.
//! 2. **The renderer could not draw it.** A triangulated heightfield agrees with
//!    the function it samples only at its vertices; in between it is a chord.
//!    `crates/client/src/terrain.rs` had to run 384 segments — 295,000 triangles
//!    — before the *worst* chord error fell inside the five-unit kill clearance,
//!    and it carried a table of measurements explaining why.
//! 3. **It had no places in it.** A sum of sines is uniform by construction: no
//!    lake, no river, no pass, nowhere that looks like anywhere else. The two
//!    airfields were flattened straight into it, which is what put each team's
//!    spawn at the bottom of a smooth circular pit.
//!
//! # The shape of the fix
//!
//! **The terrain is a triangulated lattice, and that lattice is the definition
//! rather than an approximation of one.** [`node_height`] evaluates the map at a
//! lattice node; [`ground_height`] at an arbitrary point is the plane of
//! whichever lattice triangle contains it. There is no smooth underlying
//! function to disagree with, so the renderer's mesh is not an approximation of
//! the surface — drawn at [`LATTICE_SEGMENTS`], it *is* the surface, to the last
//! bit at the nodes and to `f32` rounding in between.
//!
//! That collapses the whole LOD problem the client module was written around.
//! It also gets the low-poly look for free and honestly: the facets a player
//! sees are the facets a ship collides with.
//!
//! # Determinism
//!
//! No transcendentals at all. The noise is hash-based value noise — a
//! splitmix64 finalizer over the integer lattice coordinates, interpolated with
//! a quintic polynomial — so every operation is integer arithmetic, `+`, `-`,
//! `*`, `/`, or a comparison. All of those are exact or correctly rounded by
//! IEEE-754 on every platform this ships to. [`crate::math::det`] is not needed
//! here and nothing in this module should ever need it.
//!
//! # Where the numbers live
//!
//! [`crate::rules`] owns what other systems read: [`WorldRules::terrain_size`],
//! [`WorldRules::water_level`], [`WorldRules::terrain_kill_clearance`],
//! [`WorldRules::airfield_z`], [`WorldRules::airfield_half`] and
//! [`WorldRules::airfield_elevation`]. Those decide where ships spawn, where the
//! landing boxes sit, and where the kill plane is, so three crates read them.
//!
//! The map's *shape* — where the ranges run, how deep the lake is, which way the
//! rivers flow — stays here, for the same reason `ship.rs` gave for the sine
//! coefficients it replaced: changing one of them is a different map, not a
//! rebalance, and nothing outside this file has any use for them. If a second
//! terrain map is ever added, [`Layout`] is the struct that becomes a parameter.
//!
//! [`WorldRules::terrain_size`]: crate::rules::WorldRules::terrain_size
//! [`WorldRules::water_level`]: crate::rules::WorldRules::water_level
//! [`WorldRules::terrain_kill_clearance`]: crate::rules::WorldRules::terrain_kill_clearance
//! [`WorldRules::airfield_z`]: crate::rules::WorldRules::airfield_z
//! [`WorldRules::airfield_half`]: crate::rules::WorldRules::airfield_half
//! [`WorldRules::airfield_elevation`]: crate::rules::WorldRules::airfield_elevation
//!
//! # The map, in words
//!
//! An island basin 3,600 units across, in the sea. Two mountain ranges run
//! north–south down the east and west flanks and wall the arena in. A lake sits
//! just off the middle, fed by two rivers that have cut gorges through the
//! ranges, and drains south through a third gorge to the open sea.
//!
//! Across the middle of each half runs a transverse ridge — the north wall and
//! the south wall — between each team's base and the lake. Each is broken by one
//! ravine, so every sortie is a choice: over the top and visible from the whole
//! map, or along a canyon floor that hides you and pins your options.
//!
//! Each team launches from a **mesa** at `z = ∓`[`WorldRules::airfield_z`],
//! [`WorldRules::airfield_elevation`] above the sea, with the ground falling
//! away from it on three sides and the ocean behind. Height is the advantage a
//! base is supposed to confer, and the old map inverted it.
//!
//! The two halves are deliberately *not* mirror images. Team 0 gets the tighter
//! ravine and a clean line to the lake; team 1 gets the wider river gorge and
//! more cover on the approach. They are the same distance from the middle.

use crate::rules::{Rules, WorldRules};

// ---------------------------------------------------------------------------
// Lattice
// ---------------------------------------------------------------------------

/// Quads per side of the terrain lattice.
///
/// 150 over 3,600 units is a **24-unit cell**, and 45,000 triangles for the
/// whole map against the 295,000 the previous client mesh needed to represent
/// the sine field inside its error budget. Both numbers matter: the triangles
/// are what a WASM build uploads, and the cell is what the facets look like.
///
/// The cell size is also the map's resolution limit in the strict sense —
/// nothing narrower than about three cells survives being drawn — which is why
/// [`Layout`]'s channels are 76 units across at their narrowest and not the 30
/// a river would be if this were a texture.
///
/// # Why exactly 150
///
/// `terrain_size / LATTICE_SEGMENTS` must be exact in binary, and node
/// coordinates must land on integers, or [`ground_height`] cannot recover the
/// node index it was handed and the "the mesh *is* the surface" guarantee
/// weakens to "the mesh is nearly the surface". 3,600 / 150 = 24 exactly.
/// [`lattice_step`] debug-asserts the divisibility so a future
/// `terrain_size` cannot break it silently.
pub const LATTICE_SEGMENTS: u32 = 150;

/// Lattice nodes per side — one more than the quads.
pub const LATTICE_NODES: u32 = LATTICE_SEGMENTS + 1;

/// The spacing between lattice nodes, in world units.
///
/// # Panics
///
/// Debug builds assert the lattice divides the map exactly; see
/// [`LATTICE_SEGMENTS`].
#[must_use]
pub fn lattice_step(rules: &WorldRules) -> f64 {
    let step = rules.terrain_size / f64::from(LATTICE_SEGMENTS);
    debug_assert!(
        step * f64::from(LATTICE_SEGMENTS) == rules.terrain_size,
        "terrain_size must be an exact multiple of LATTICE_SEGMENTS",
    );
    step
}

/// World coordinate of lattice node `i`, on either axis.
///
/// Written as `(i - segments/2) * step` and not `-half + i * step`: the first
/// is a small exact integer times an exact step, so the result is exact and
/// `(x + half) / step` recovers `i` with no rounding. The second form rounds
/// twice and does not.
#[must_use]
pub fn node_pos(i: u32, rules: &WorldRules) -> f64 {
    let centre = f64::from(LATTICE_SEGMENTS) * 0.5;
    (f64::from(i) - centre) * lattice_step(rules)
}

// ---------------------------------------------------------------------------
// Public sampling
// ---------------------------------------------------------------------------

/// The terrain bed at a lattice node — **the** definition of the map.
///
/// May be below [`WorldRules::water_level`]: lake beds and river channels are
/// negative, which is exactly how they end up underwater.
///
/// [`WorldRules::water_level`]: crate::rules::WorldRules::water_level
#[must_use]
pub fn node_height(ix: u32, iz: u32, rules: &WorldRules) -> f64 {
    sample(node_pos(ix, rules), node_pos(iz, rules), rules)
}

/// The terrain bed at an arbitrary point: the plane of the lattice triangle
/// containing it.
///
/// Outside the map this is [`WorldRules::water_level`] — open sea.
///
/// # Which triangle
///
/// A cell splits along the diagonal from its `+x` corner to its `+z` corner,
/// matching the index order a renderer emits for `[a, c, b, b, c, d]` with
/// `a = (ix, iz)`, `b = (ix+1, iz)`, `c = (ix, iz+1)`, `d = (ix+1, iz+1)`. The
/// two halves are `u + v <= 1` and `u + v > 1`.
///
/// The barycentric form is written as `a*(1-u-v) + b*u + c*v` rather than
/// `a + (b-a)*u + (c-a)*v`, which is algebraically the same and numerically is
/// not: only the first returns the corner value *exactly* when `u` and `v` are
/// 0 or 1. That exactness is what
/// `sampling_a_node_returns_that_nodes_height_exactly` pins, and it is the
/// difference between a renderer's vertices sitting on the collision surface
/// and merely near it.
///
/// [`WorldRules::water_level`]: crate::rules::WorldRules::water_level
#[must_use]
pub fn ground_height(x: f64, z: f64, rules: &WorldRules) -> f64 {
    let half = rules.terrain_size * 0.5;
    if !(x.abs() <= half && z.abs() <= half) {
        return rules.water_level;
    }
    let step = lattice_step(rules);
    let last = LATTICE_SEGMENTS - 1;

    // `min(last)` handles the far edge, where the point sits exactly on the
    // final node and would otherwise index a cell that does not exist.
    let gx = (x + half) / step;
    let gz = (z + half) / step;
    let ix = (gx as u32).min(last);
    let iz = (gz as u32).min(last);
    let u = gx - f64::from(ix);
    let v = gz - f64::from(iz);

    let a = node_height(ix, iz, rules);
    let b = node_height(ix + 1, iz, rules);
    let c = node_height(ix, iz + 1, rules);
    if u + v <= 1.0 {
        a * (1.0 - u - v) + b * u + c * v
    } else {
        let d = node_height(ix + 1, iz + 1, rules);
        let (uu, vv) = (1.0 - u, 1.0 - v);
        d * (1.0 - uu - vv) + b * vv + c * uu
    }
}

/// The surface a ship collides with: the bed, or the water on top of it.
///
/// This is what the kill plane is measured from and what the bot's terrain
/// avoidance flies above, because from a ship's point of view a lake is as solid
/// as a hillside. [`ground_height`] is the one to draw; this is the one to fly
/// against.
#[must_use]
pub fn surface_height(x: f64, z: f64, rules: &WorldRules) -> f64 {
    ground_height(x, z, rules).max(rules.water_level)
}

/// The altitude below which a ship dies on the terrain map.
///
/// `main.js:2251` compared against `getTerrainHeight + TERRAIN_KILL_CLEARANCE`;
/// the only change is that the surface now includes the water.
#[must_use]
pub fn kill_altitude(x: f64, z: f64, rules: &WorldRules) -> f64 {
    surface_height(x, z, rules) + rules.terrain_kill_clearance
}

/// Magnitude of the surface gradient at a point — rise over run, so `1.0` is
/// 45°.
///
/// Sampled across one lattice cell rather than analytically, because the
/// surface is piecewise planar and has no gradient at the creases. Scenery
/// placement is the only caller: trees want gentle ground, boulders want steep.
#[must_use]
pub fn slope_at(x: f64, z: f64, rules: &WorldRules) -> f64 {
    let step = lattice_step(rules);
    let dx = ground_height(x + step, z, rules) - ground_height(x - step, z, rules);
    let dz = ground_height(x, z + step, rules) - ground_height(x, z - step, rules);
    let two = 2.0 * step;
    ((dx * dx + dz * dz).sqrt()) / two
}

// ---------------------------------------------------------------------------
// The layout
// ---------------------------------------------------------------------------

/// One carved channel — a river, a gorge, or a ravine.
///
/// A polyline spine with a flat floor and straight walls. The profile is
/// deliberately the simplest thing that reads as a valley: floor level out to
/// `half_width`, then a constant `wall_slope` climb. Cut with `min`, so a
/// channel only ever removes rock — it can cross a mountain and produce a gorge,
/// or cross a plain and produce a ditch, without either case needing a special
/// rule.
///
/// The crease where the wall meets the surrounding terrain is not smoothed, and
/// that is the intent: it is one lattice edge wide and reads as a rim.
struct Channel {
    /// Spine, as world `(x, z)` points. Distance is to the polyline, so three
    /// or four points is enough for a river that bends.
    spine: &'static [(f64, f64)],
    /// Half-width of the flat floor. At or below `1.5 *` the lattice step a
    /// channel stops being drawable; see [`LATTICE_SEGMENTS`].
    half_width: f64,
    /// Rise per unit of horizontal distance beyond the floor.
    wall_slope: f64,
    /// Floor elevation. Below `water_level` the channel fills and becomes a
    /// river; above it, a dry ravine.
    floor: f64,
}

/// One mountain range — a spine that ridged noise is hung on.
struct Range {
    /// Crest line, as world `(x, z)` points.
    spine: &'static [(f64, f64)],
    /// Distance from the spine at which the range has fallen to nothing.
    reach: f64,
    /// Peak height added at the crest where the ridged noise is at its
    /// strongest.
    amplitude: f64,
}

/// Everything about where things *are*. See the module docs for the tour.
struct Layout;

impl Layout {
    /// Sea floor at the map border, and the slope of the coastal shelf running
    /// up from it. Together these are what makes the map an island rather than
    /// a plateau with a hole cut round it.
    const COAST_FLOOR: f64 = -72.0;
    /// Rise per unit of distance inland from the border. The shelf crosses the
    /// waterline about 115 units in and reaches the height of the inland hills
    /// about 400 in, so the coast is a band and not a wall.
    const COAST_SLOPE: f64 = 0.62;
    /// How far the shoreline wanders in or out from that nominal 115.
    ///
    /// Without this the island is a rounded square, which is the single most
    /// obvious tell that a coast was generated. With it the same shelf produces
    /// bays, spits and headlands, because the *distance inland* is what the
    /// noise perturbs rather than the height.
    const COAST_WANDER: f64 = 250.0;
    /// Feature size of that wander.
    const COAST_SCALE: f64 = 620.0;
    /// Width of the band over which the terrain is faded to sea level at the
    /// very border, so the drawn edge and the "outside the map is water" rule
    /// meet without a step.
    ///
    /// Wide, because it scales the ranges too: a 640-unit range brought to zero
    /// over a narrow band is a cliff with a gradient of ten, and
    /// `no_facet_is_a_vertical_wall` is what caught that.
    const BORDER_FADE: f64 = 240.0;

    /// How far the relief's sample point is displaced by a second noise field,
    /// and over what distance that displacement varies.
    ///
    /// Domain warping — sampling noise at a position that has itself been
    /// pushed around by noise — is what turns the rounded blobs that value
    /// noise produces into the stretched, folded shapes real ground has. It is
    /// the cheapest single thing that can be done to a heightfield, and on the
    /// plan view it is the difference between "generated" and "somewhere".
    ///
    /// The ranges are warped by it too, which is why they bend rather than
    /// running as two parallel walls down the sides of a corridor.
    const WARP: f64 = 190.0;
    /// See [`Self::WARP`].
    const WARP_SCALE: f64 = 780.0;

    /// Mean ground level before relief, ranges, and carving.
    const BASE_LEVEL: f64 = 52.0;
    /// Peak-to-trough of the rolling relief, in each direction from
    /// [`Self::BASE_LEVEL`].
    ///
    /// Deliberately capped so that open ground tops out below
    /// [`WorldRules::airfield_elevation`]: a mesa is only high ground if the
    /// hills around it are not. `each_mesa_stands_above_its_surroundings`
    /// is what holds the two numbers together.
    ///
    /// [`WorldRules::airfield_elevation`]: crate::rules::WorldRules::airfield_elevation
    const BASE_RELIEF: f64 = 130.0;
    /// Wavelength of the first relief octave.
    const BASE_SCALE: f64 = 940.0;
    /// Relief octaves. The last is at 940/16 ≈ 59 units, just above the
    /// 48-unit Nyquist limit of a 24-unit lattice; a sixth would alias.
    const BASE_OCTAVES: u32 = 5;

    /// Wavelength of the first ridged octave.
    const RIDGE_SCALE: f64 = 540.0;
    /// Ridged octaves, on the same Nyquist argument as [`Self::BASE_OCTAVES`].
    const RIDGE_OCTAVES: u32 = 4;
    /// The share of a range's amplitude that is the ridge itself, with the rest
    /// left to the ridged noise.
    ///
    /// At zero a range is pure noise inside a mask, which is what the first cut
    /// of this map did, and it read as scattered rubble along a line rather
    /// than as a mountain range — noise crosses zero, so the crest kept
    /// vanishing. A guaranteed floor along the spine is what gives a range a
    /// continuous skyline; the noise on top is what stops it being a wall.
    const RANGE_SPINE: f64 = 0.44;

    /// Centre of the lake.
    const LAKE_CENTRE: (f64, f64) = (-60.0, 100.0);
    /// Radius of the flat lake bed, before the shore starts to climb.
    const LAKE_RADIUS: f64 = 380.0;
    /// How far the lake's shoreline wanders in and out from that radius. Same
    /// argument as [`Self::COAST_WANDER`]: a circle is the giveaway.
    const LAKE_WANDER: f64 = 135.0;
    /// Feature size of the lake's shoreline wander.
    const LAKE_SCALE: f64 = 330.0;
    /// Depth of the lake bed below sea level.
    const LAKE_FLOOR: f64 = -64.0;
    /// Rise per unit of distance out from [`Self::LAKE_RADIUS`]. Gentle, so the
    /// lake has shallows and shoreline rather than walls.
    const LAKE_SHORE: f64 = 0.42;

    /// How far a channel's banks wander from its nominal half-width.
    ///
    /// Kept below the narrowest [`Channel::half_width`] on the map, so that a
    /// point on a spine is inside its own channel however the noise falls —
    /// otherwise a river would occasionally be dammed by its own banks.
    const BANK_WANDER: f64 = 34.0;
    /// Feature size of the bank wander. Short, so a river meanders over its
    /// length rather than bulging once.
    const BANK_SCALE: f64 = 240.0;

    /// The two flanking ranges and the two transverse walls.
    const RANGES: &'static [Range] = &[
        // West flank. Runs the length of the map and wobbles, so the arena is
        // not a corridor with parallel sides.
        Range {
            spine: &[
                (-1300.0, -1800.0),
                (-1140.0, -700.0),
                (-1270.0, 200.0),
                (-1010.0, 1150.0),
                (-1160.0, 1800.0),
            ],
            reach: 470.0,
            amplitude: 640.0,
        },
        // East flank. Slightly lower and set further out, which is what makes
        // the east side the open one.
        Range {
            spine: &[
                (1265.0, -1800.0),
                (1150.0, -600.0),
                (1330.0, 400.0),
                (1120.0, 1400.0),
                (1250.0, 1800.0),
            ],
            reach: 430.0,
            amplitude: 565.0,
        },
        // North wall — between team 0's mesa and the lake.
        Range {
            spine: &[(-820.0, -800.0), (-100.0, -700.0), (880.0, -830.0)],
            reach: 300.0,
            amplitude: 470.0,
        },
        // South wall — the same job on team 1's side, lower and broader, and
        // already broken by the lake's outflow gorge.
        Range {
            spine: &[(-900.0, 830.0), (60.0, 730.0), (960.0, 870.0)],
            reach: 280.0,
            amplitude: 420.0,
        },
    ];

    /// Rivers, gorges and ravines, cut in this order. Later channels win where
    /// they cross, which only matters where a river meets the lake and the
    /// deeper of the two should survive.
    const CHANNELS: &'static [Channel] = &[
        // Ravine through the north wall. Narrow and steep: this is the covered
        // route out of team 0's half, and it should feel like a commitment.
        //
        // Both ravine mouths stop clear of the mesa footprint — flat top plus
        // ramp, so ±483 in x and 393 either side of `airfield_z`. A channel
        // that reached inside it would be filled in by `plateau_blend`, which
        // runs last, and the ravine would simply not be there.
        Channel {
            spine: &[(450.0, -1000.0), (350.0, -800.0), (300.0, -560.0)],
            half_width: 44.0,
            wall_slope: 2.35,
            floor: 58.0,
        },
        // Ravine through the south wall. Wider and shallower — team 1's low
        // route is easier to fly and easier to be seen in.
        Channel {
            spine: &[(-450.0, 1000.0), (-430.0, 800.0), (-300.0, 570.0)],
            half_width: 56.0,
            wall_slope: 1.85,
            floor: 62.0,
        },
        // A pass through the west range, at mid-map. Dry, high, and the only
        // way through the west flank without climbing over it.
        Channel {
            spine: &[(-1560.0, -170.0), (-1120.0, -70.0), (-760.0, -10.0)],
            half_width: 52.0,
            wall_slope: 2.1,
            floor: 96.0,
        },
        // West river: down out of the west range into the lake. Floor below sea
        // level for its whole length, so it is water the whole way and the part
        // inside the range is a flooded gorge.
        Channel {
            spine: &[
                (-1560.0, 745.0),
                (-1030.0, 520.0),
                (-620.0, 270.0),
                (-300.0, 150.0),
            ],
            half_width: 40.0,
            wall_slope: 0.95,
            floor: -9.0,
        },
        // East river, the mirror job on the other flank and a touch wider.
        Channel {
            spine: &[
                (1560.0, -520.0),
                (1060.0, -360.0),
                (620.0, -160.0),
                (240.0, 40.0),
            ],
            half_width: 46.0,
            wall_slope: 0.9,
            floor: -9.0,
        },
        // The outflow: lake to open sea, straight through the south wall. The
        // widest channel on the map and the one genuinely flyable water route
        // between the halves.
        Channel {
            spine: &[
                (55.0, 400.0),
                (240.0, 820.0),
                (570.0, 1300.0),
                (690.0, 1800.0),
            ],
            half_width: 62.0,
            wall_slope: 1.15,
            floor: -12.0,
        },
    ];

    /// How far the dead-flat top of a mesa extends beyond the landing pad
    /// itself.
    ///
    /// Two lattice cells. The pad has to be flat *as drawn*, not just as
    /// sampled: the flat region of a continuous blend ends exactly on the pad
    /// edge, but the mesh only has vertices every 24 units, so the last row of
    /// triangles inside the pad would interpolate down toward a node already on
    /// the ramp. A two-cell apron puts the whole pad strictly inside the flat
    /// part. `both_mesas_are_flat_at_the_airfield_elevation` samples the pad
    /// through [`ground_height`], so it fails if this shrinks.
    const MESA_APRON: f64 = 48.0;
    /// Width of the ramp from the mesa top down to whatever the terrain was
    /// doing. 210 units of rise over 155 is a 54° face — steep enough to read
    /// as a mesa, shallow enough to fly up.
    const MESA_RAMP: f64 = 155.0;
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

/// The map, evaluated at a point. Only [`node_height`] should call this — every
/// other reader goes through the lattice, which is what keeps the renderer and
/// the simulation looking at the same surface.
fn sample(x: f64, z: f64, rules: &WorldRules) -> f64 {
    // 0. The warped sample point. Everything that is *noise* reads this;
    //    everything that is a *placed landmark* reads the real one, so the lake
    //    stays where the layout says while the ground around it does not look
    //    machined. See `Layout::WARP`.
    let (wx, wz) = warp(x, z);

    // 1. Rolling relief: the hills, the meadows, and the general lie of the
    //    land. Everything after this either adds a landmark or removes rock.
    let mut h = Layout::BASE_LEVEL
        + fbm(
            wx / Layout::BASE_SCALE,
            wz / Layout::BASE_SCALE,
            Layout::BASE_OCTAVES,
            SALT_RELIEF,
        ) * Layout::BASE_RELIEF;

    // 2. The coast, as a `min` against a shelf climbing inland from the border,
    //    with a noisy idea of how far inland it is. This turns a square of hills
    //    into an island with bays in it.
    //
    //    **Before the ranges, not after.** A cap that ran last would be a cap on
    //    the mountains too, and the shelf has to reach a long way in — 400 units
    //    — to make a coast rather than a kerb. Running it first means a range
    //    that meets the sea plunges into it, which is both what happens and the
    //    best thing on the map to fly along.
    //
    //    Two different "distance inland" numbers are in play and they must not
    //    be confused. `inland` is geometric and drives the hard fade at the
    //    border in step 6; `wandered` is the noisy one and drives only the
    //    shelf. Using the noisy one for the fade lets the fade switch off where
    //    the noise runs positive, which leaves ground standing at the map's
    //    edge with open sea beside it.
    let half = rules.terrain_size * 0.5;
    let inland = half - x.abs().max(z.abs());
    let wandered = inland
        + Layout::COAST_WANDER
            * fbm(
                x / Layout::COAST_SCALE,
                z / Layout::COAST_SCALE,
                3,
                SALT_COAST,
            );
    h = h.min(Layout::COAST_FLOOR + wandered * Layout::COAST_SLOPE);

    // How much of the map's own terrain survives at this point: 1 inland, 0 at
    // the border. Applied to the ranges as well as to the final height, because
    // a 640-unit range that met the border unattenuated would be faded away
    // over one band-width and produce a gradient no ship could see coming.
    let edge = smoothstep(inland / Layout::BORDER_FADE);

    // 3. Ranges. Ridged noise (`1 - |n|`, squared) is what gives a mountain a
    //    crest line instead of a dome; the spine mask is what stops the crests
    //    wandering off across the middle of the arena; and `RANGE_SPINE` is what
    //    keeps the skyline continuous where the noise happens to cross zero.
    for range in Layout::RANGES {
        let d = distance_to_polyline(wx, wz, range.spine);
        if d >= range.reach {
            continue;
        }
        let mask = smoothstep(1.0 - d / range.reach);
        let crest = ridged(
            wx / Layout::RIDGE_SCALE,
            wz / Layout::RIDGE_SCALE,
            Layout::RIDGE_OCTAVES,
            SALT_RIDGE,
        );
        h += edge
            * mask
            * range.amplitude
            * (Layout::RANGE_SPINE + (1.0 - Layout::RANGE_SPINE) * crest);
    }

    // 4. The lake basin, then the channels. All of these are `min` cuts, so
    //    order between them only decides which wins where two overlap, and
    //    never whether the terrain is raised somewhere it should not be.
    //
    //    Both subtract a noise field from the *distance* rather than from the
    //    height: a wandering distance moves a bank sideways, which is what a
    //    river does, while a wandering height would put lumps in the water.
    let (lx, lz) = Layout::LAKE_CENTRE;
    let lake_d = ((x - lx) * (x - lx) + (z - lz) * (z - lz)).sqrt()
        - Layout::LAKE_WANDER
            * fbm(x / Layout::LAKE_SCALE, z / Layout::LAKE_SCALE, 3, SALT_LAKE);
    h = cut(
        h,
        lake_d,
        Layout::LAKE_RADIUS,
        Layout::LAKE_SHORE,
        Layout::LAKE_FLOOR,
    );
    let bank = Layout::BANK_WANDER
        * fbm(x / Layout::BANK_SCALE, z / Layout::BANK_SCALE, 3, SALT_BANK);
    for ch in Layout::CHANNELS {
        h = cut(
            h,
            distance_to_polyline(x, z, ch.spine) - bank,
            ch.half_width,
            ch.wall_slope,
            ch.floor,
        );
    }

    // 5. Fade the natural terrain to sea level at the border, so the drawn
    //    edge meets the open water `ground_height` returns beyond it with no
    //    step. This runs *before* the mesas and not after: a fade applied last
    //    would drag a landing pad down toward the waterline in proportion to
    //    how close to the edge it sits, and a pad has to be exactly level.
    h *= edge;

    // 6. The two mesas, last, because a base has to exist whatever the terrain
    //    under it was doing. `airfield_z` is chosen so a mesa's outer ramp
    //    finishes inside the map — see that field — and the ramp therefore runs
    //    down into the sea rather than off a cliff at the border.
    let blend = plateau_blend(x, z, rules);
    h = h * (1.0 - blend) + rules.airfield_elevation * blend;
    // `+ 0.0` normalises `-0.0` to `+0.0` and is the identity on everything
    // else. The fade above multiplies a negative sea floor by zero at the very
    // border, and a signed zero there is enough to make a lattice node and the
    // same point read through `ground_height` differ *in their bits* while
    // being equal as numbers — which is exactly the guarantee
    // `sampling_a_node_returns_that_nodes_height_exactly` exists to hold.
    h + 0.0
}

/// How strongly the terrain is forced to [`WorldRules::airfield_elevation`] at a
/// point: 1 on a landing pad, 0 clear of the mesa.
///
/// [`WorldRules::airfield_elevation`]: crate::rules::WorldRules::airfield_elevation
/// The distance is to the *rectangle*, not to its centre: zero anywhere on the
/// flat top, then growing outward, with the corners rounded because the two
/// excesses combine under a square root. A Chebyshev max would give square
/// corners and a visible cross-shaped rim.
fn plateau_blend(x: f64, z: f64, rules: &WorldRules) -> f64 {
    let hw = rules.airfield_half.x + Layout::MESA_APRON;
    let hd = rules.airfield_half.z + Layout::MESA_APRON;
    let mut best = 0.0f64;
    for cz in [-rules.airfield_z, rules.airfield_z] {
        let dx = (x.abs() - hw).max(0.0);
        let dz = ((z - cz).abs() - hd).max(0.0);
        let d = (dx * dx + dz * dz).sqrt();
        best = best.max(1.0 - smoothstep(d / Layout::MESA_RAMP));
    }
    best
}

/// A flat-floored, straight-walled channel, cut into `h` by `min`.
///
/// Never raises the terrain: a channel is rock removed, so crossing a mountain
/// makes a gorge and crossing a plain makes a ditch, with no branch between the
/// two cases.
fn cut(h: f64, dist: f64, half_width: f64, wall_slope: f64, floor: f64) -> f64 {
    h.min(floor + (dist - half_width).max(0.0) * wall_slope)
}

// ---------------------------------------------------------------------------
// Geometry helpers
// ---------------------------------------------------------------------------

/// Distance from `(x, z)` to a polyline, in the plane.
///
/// `+ - * /` and one `sqrt`, so it is exact-or-correctly-rounded throughout. A
/// degenerate segment (two identical points) falls out as the distance to the
/// point, because `t` clamps to zero when the length is zero.
fn distance_to_polyline(x: f64, z: f64, spine: &[(f64, f64)]) -> f64 {
    let mut best = f64::INFINITY;
    for pair in spine.windows(2) {
        let (ax, az) = pair[0];
        let (bx, bz) = pair[1];
        let (ex, ez) = (bx - ax, bz - az);
        let len2 = ex * ex + ez * ez;
        let t = if len2 > 0.0 {
            (((x - ax) * ex + (z - az) * ez) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (px, pz) = (ax + ex * t, az + ez * t);
        let d2 = (x - px) * (x - px) + (z - pz) * (z - pz);
        if d2 < best {
            best = d2;
        }
    }
    best.sqrt()
}

/// The cubic smoothstep, clamped. Same function `terrain.js:9` used.
fn smoothstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// ---------------------------------------------------------------------------
// Noise
// ---------------------------------------------------------------------------

/// Salt for the rolling relief octaves.
const SALT_RELIEF: u64 = 0x5361_6C74_5265_6C69;
/// Salt for the ridged mountain octaves. Different salt, so a peak and a hill
/// never line up just because they share a lattice cell.
const SALT_RIDGE: u64 = 0x5361_6C74_5269_6467;
/// Salts for the two components of the domain warp. Independent fields, or the
/// displacement would be along one diagonal everywhere.
const SALT_WARP_X: u64 = 0x5361_6C74_5761_7078;
/// See [`SALT_WARP_X`].
const SALT_WARP_Z: u64 = 0x5361_6C74_5761_707A;
/// Salt for the wander in the coastline's distance-inland.
const SALT_COAST: u64 = 0x5361_6C74_436F_6173;
/// Salt for the lake's shoreline wander.
const SALT_LAKE: u64 = 0x5361_6C74_4C61_6B65;
/// Salt for the channel bank wander.
const SALT_BANK: u64 = 0x5361_6C74_4261_6E6B;

/// The domain warp: displace a sample point by two independent noise fields.
/// See [`Layout::WARP`].
fn warp(x: f64, z: f64) -> (f64, f64) {
    let u = x / Layout::WARP_SCALE;
    let v = z / Layout::WARP_SCALE;
    (
        x + Layout::WARP * fbm(u, v, 3, SALT_WARP_X),
        z + Layout::WARP * fbm(u, v, 3, SALT_WARP_Z),
    )
}

/// Lacunarity — the frequency step between octaves.
const LACUNARITY: f64 = 2.0;
/// Gain — the amplitude step between octaves.
const GAIN: f64 = 0.5;

/// Fractional Brownian motion over [`value_noise`], normalised to `[-1, 1]`.
///
/// The normaliser is the geometric sum of the octave amplitudes, computed
/// alongside them rather than hardcoded, so changing [`Layout::BASE_OCTAVES`]
/// cannot silently change the map's overall height.
fn fbm(x: f64, z: f64, octaves: u32, salt: u64) -> f64 {
    let (mut sum, mut norm) = (0.0, 0.0);
    let (mut freq, mut amp) = (1.0, 1.0);
    for o in 0..octaves {
        sum += value_noise(x * freq, z * freq, salt ^ u64::from(o)) * amp;
        norm += amp;
        freq *= LACUNARITY;
        amp *= GAIN;
    }
    sum / norm
}

/// Ridged multifractal, in `[0, 1]`.
///
/// `1 - |n|` folds the noise about zero, which turns its zero crossings into
/// creases; squaring sharpens them into crests and pushes the flanks down. This
/// is the whole difference between terrain that looks like mountains and terrain
/// that looks like dunes.
fn ridged(x: f64, z: f64, octaves: u32, salt: u64) -> f64 {
    let (mut sum, mut norm) = (0.0, 0.0);
    let (mut freq, mut amp) = (1.0, 1.0);
    for o in 0..octaves {
        let n = 1.0 - value_noise(x * freq, z * freq, salt ^ u64::from(o)).abs();
        sum += n * n * amp;
        norm += amp;
        freq *= LACUNARITY;
        amp *= GAIN;
    }
    sum / norm
}

/// Value noise in `[-1, 1]`: hash the four surrounding integer lattice points
/// and interpolate with a quintic fade.
///
/// Quintic rather than the cubic [`smoothstep`] because value noise's second
/// derivative is visible as banding along the lattice lines at this amplitude,
/// and `6t^5 - 15t^4 + 10t^3` is the standard polynomial with a zero second
/// derivative at both ends.
fn value_noise(x: f64, z: f64, salt: u64) -> f64 {
    let fx = x.floor();
    let fz = z.floor();
    let (ix, iz) = (fx as i64, fz as i64);
    let (tx, tz) = (x - fx, z - fz);
    let u = fade(tx);
    let v = fade(tz);

    let n00 = hash_to_unit(ix, iz, salt);
    let n10 = hash_to_unit(ix + 1, iz, salt);
    let n01 = hash_to_unit(ix, iz + 1, salt);
    let n11 = hash_to_unit(ix + 1, iz + 1, salt);

    let a = n00 + (n10 - n00) * u;
    let b = n01 + (n11 - n01) * u;
    a + (b - a) * v
}

/// The quintic fade curve. See [`value_noise`].
fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// A lattice point's noise value, in `[-1, 1)`.
///
/// The mixing is splitmix64's finalizer, which is what [`crate::rng`] already
/// trusts for its output stage. Integers are folded in with the two odd
/// constants below so that `(ix, iz)` and `(iz, ix)` differ — a symmetric mix
/// puts a visible diagonal through the whole map.
///
/// The float conversion takes the top 53 bits, the exact width of an `f64`
/// mantissa, so it is a division by a power of two and therefore exact.
fn hash_to_unit(ix: i64, iz: i64, salt: u64) -> f64 {
    let mixed = (ix as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (iz as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ salt;
    let h = splitmix(mixed);
    // [0, 1) -> [-1, 1).
    ((h >> 11) as f64) * (1.0 / 9_007_199_254_740_992.0) * 2.0 - 1.0
}

/// splitmix64's finalizer.
const fn splitmix(mut h: u64) -> u64 {
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 31;
    h
}

// ---------------------------------------------------------------------------
// Convenience
// ---------------------------------------------------------------------------

/// [`surface_height`] against a whole [`Rules`], for callers that hold one.
#[must_use]
pub fn surface_height_rules(x: f64, z: f64, rules: &Rules) -> f64 {
    surface_height(x, z, &rules.world)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    fn world() -> WorldRules {
        WorldRules::DEFAULT
    }

    /// The guarantee the whole module is built around: a renderer that puts a
    /// vertex at every lattice node draws the collision surface, not an
    /// approximation of it.
    #[test]
    fn sampling_a_node_returns_that_nodes_height_exactly() {
        let w = world();
        for iz in 0..LATTICE_NODES {
            for ix in 0..LATTICE_NODES {
                let (x, z) = (node_pos(ix, &w), node_pos(iz, &w));
                assert_eq!(
                    ground_height(x, z, &w).to_bits(),
                    node_height(ix, iz, &w).to_bits(),
                    "node ({ix}, {iz}) at ({x}, {z})",
                );
            }
        }
    }

    /// Between nodes the surface is the plane of a lattice triangle. Checked
    /// against an independently computed plane rather than against itself: pick
    /// a cell, pick a point in it, work out which triangle it is in by hand,
    /// and solve the plane through those three corners.
    #[test]
    fn between_nodes_the_surface_is_the_triangles_plane() {
        let w = world();
        let step = lattice_step(&w);
        let mut rng = Rng::new(0x7EA1_4A11);
        for _ in 0..20_000 {
            let ix = rng.bounded_u32(LATTICE_SEGMENTS);
            let iz = rng.bounded_u32(LATTICE_SEGMENTS);
            let u = rng.next_f64();
            let v = rng.next_f64();
            let x = node_pos(ix, &w) + u * step;
            let z = node_pos(iz, &w) + v * step;

            let a = node_height(ix, iz, &w);
            let b = node_height(ix + 1, iz, &w);
            let c = node_height(ix, iz + 1, &w);
            let want = if u + v <= 1.0 {
                a + (b - a) * u + (c - a) * v
            } else {
                let d = node_height(ix + 1, iz + 1, &w);
                d + (b - d) * (1.0 - v) + (c - d) * (1.0 - u)
            };
            let got = ground_height(x, z, &w);
            assert!(
                (got - want).abs() < 1e-6,
                "at ({x}, {z}) cell ({ix}, {iz}) u={u} v={v}: {got} vs {want}",
            );
        }
    }

    /// Adjacent cells agree along their shared edge, which is what says the
    /// drawn mesh has no cracks in it.
    #[test]
    fn the_surface_is_continuous_across_cell_edges() {
        let w = world();
        let step = lattice_step(&w);
        let mut rng = Rng::new(0x0EDE_0EDE);
        for _ in 0..5_000 {
            let ix = rng.bounded_u32(LATTICE_SEGMENTS - 1) + 1;
            let iz = rng.bounded_u32(LATTICE_SEGMENTS);
            let edge_x = node_pos(ix, &w);
            let z = node_pos(iz, &w) + rng.next_f64() * step;
            let left = ground_height(edge_x - 1e-7, z, &w);
            let right = ground_height(edge_x + 1e-7, z, &w);
            assert!(
                (left - right).abs() < 1e-3,
                "seam at x={edge_x}, z={z}: {left} vs {right}",
            );
        }
    }

    /// Same value from the same input, every time, with no dependence on the
    /// order things were asked for.
    #[test]
    fn the_surface_is_deterministic() {
        let w = world();
        let mut rng = Rng::new(0xD37E_D37E);
        let mut pts = Vec::new();
        for _ in 0..4_000 {
            let x = rng.range_f64(-1_900.0, 1_900.0);
            let z = rng.range_f64(-1_900.0, 1_900.0);
            pts.push((x, z, ground_height(x, z, &w)));
        }
        for (x, z, h) in pts.iter().rev() {
            assert_eq!(ground_height(*x, *z, &w).to_bits(), h.to_bits());
        }
    }

    /// Outside the map is open sea, and the drawn edge fades to meet it.
    #[test]
    fn the_map_ends_in_water() {
        let w = world();
        let half = w.terrain_size * 0.5;
        assert_eq!(ground_height(half + 1.0, 0.0, &w), w.water_level);
        assert_eq!(ground_height(0.0, -half - 1.0, &w), w.water_level);
        // The last ring of nodes is at or below the waterline all the way
        // round, so nothing pokes through the seam.
        for i in 0..LATTICE_NODES {
            for (ix, iz) in [
                (0, i),
                (LATTICE_SEGMENTS, i),
                (i, 0),
                (i, LATTICE_SEGMENTS),
            ] {
                let h = node_height(ix, iz, &w);
                assert!(h <= w.water_level, "node ({ix}, {iz}) is {h} above water");
            }
        }
    }

    /// Both landing pads are dead flat at the elevation the spawn logic assumes,
    /// across the full footprint of the collision box that sits on them.
    #[test]
    fn both_mesas_are_flat_at_the_airfield_elevation() {
        let w = world();
        let (hw, hd) = (w.airfield_half.x, w.airfield_half.z);
        for cz in [-w.airfield_z, w.airfield_z] {
            for i in 0..=20 {
                for j in 0..=20 {
                    let x = -hw + (f64::from(i) / 20.0) * 2.0 * hw;
                    let z = cz - hd + (f64::from(j) / 20.0) * 2.0 * hd;
                    let h = ground_height(x, z, &w);
                    assert!(
                        (h - w.airfield_elevation).abs() < 1e-9,
                        "pad at ({x}, {z}) is {h}, not {}",
                        w.airfield_elevation,
                    );
                }
            }
        }
    }

    /// A mesa stands above what surrounds it. This is the whole complaint about
    /// the old map — the spawn sat in a pit — expressed as an assertion.
    #[test]
    fn each_mesa_stands_above_its_surroundings() {
        let w = world();
        for cz in [-w.airfield_z, w.airfield_z] {
            // Sample a ring well outside the rim and require the pad to be the
            // high ground on the great majority of bearings. Not all of them:
            // the ranges are 600 units tall and one of them is allowed to be
            // higher than a 150-unit mesa 800 units away.
            let mut above = 0;
            let mut total = 0;
            for k in 0..72 {
                let ang = f64::from(k) / 72.0;
                // A deterministic circle without trigonometry: walk the
                // perimeter of a square and normalise it out to the radius.
                let (dx, dz) = square_bearing(ang);
                let r = 620.0;
                let (x, z) = (dx * r, cz + dz * r);
                if x.abs() > 1_700.0 || z.abs() > 1_700.0 {
                    continue;
                }
                total += 1;
                if ground_height(x, z, &w) < w.airfield_elevation {
                    above += 1;
                }
            }
            assert!(
                above * 4 >= total * 3,
                "mesa at z={cz} is above only {above}/{total} of its surroundings",
            );
        }
    }

    /// A point on the unit "circle", built from `+ - * /` only.
    fn square_bearing(t: f64) -> (f64, f64) {
        let s = t * 4.0;
        let (x, z) = match s as u32 {
            0 => (1.0, s - 0.5),
            1 => (1.5 - s, 1.0),
            2 => (-1.0, 2.5 - s),
            _ => (s - 3.5, -1.0),
        };
        let len = (x * x + z * z).sqrt();
        (x / len, z / len)
    }

    /// The lake holds water, the rivers reach it, and the ravines do not flood.
    #[test]
    fn the_water_features_are_where_the_layout_says() {
        let w = world();
        let (lx, lz) = Layout::LAKE_CENTRE;
        assert!(
            ground_height(lx, lz, &w) < w.water_level - 40.0,
            "the lake centre is not underwater",
        );
        // A point on each river's spine, upstream of the lake and inside the
        // range it cuts through.
        for (x, z) in [(-1_030.0, 520.0), (1_060.0, -360.0), (570.0, 1_300.0)] {
            assert!(
                ground_height(x, z, &w) < w.water_level,
                "the channel at ({x}, {z}) is dry",
            );
        }
        // Ravine floors are above the waterline, or they would be rivers.
        for (x, z) in [(350.0, -800.0), (-430.0, 800.0), (-1_120.0, -70.0)] {
            let h = ground_height(x, z, &w);
            assert!(
                h > w.water_level + 20.0,
                "the ravine at ({x}, {z}) has flooded to {h}",
            );
        }
    }

    /// Each ravine is genuinely a way through the wall it crosses: its floor is
    /// far below the crest either side of it.
    #[test]
    fn the_ravines_cut_below_the_walls_they_cross() {
        let w = world();
        for (x, z, across) in [(350.0, -800.0, 270.0), (-430.0, 800.0, 250.0)] {
            let floor = ground_height(x, z, &w);
            let left = ground_height(x - across, z, &w);
            let right = ground_height(x + across, z, &w);
            assert!(
                floor + 120.0 < left.max(right),
                "the ravine at ({x}, {z}) floor {floor} is not below its walls \
                 ({left}, {right})",
            );
        }
    }

    /// Nothing is absurd: the map has real mountains and no spires.
    #[test]
    fn the_relief_stays_in_a_sane_band() {
        let w = world();
        let mut peak = f64::NEG_INFINITY;
        let mut trough = f64::INFINITY;
        for iz in 0..LATTICE_NODES {
            for ix in 0..LATTICE_NODES {
                let h = node_height(ix, iz, &w);
                assert!(h.is_finite(), "node ({ix}, {iz}) is not finite");
                peak = peak.max(h);
                trough = trough.min(h);
            }
        }
        assert!(
            (400.0..=900.0).contains(&peak),
            "peak elevation {peak} is outside the intended band",
        );
        assert!(
            (-120.0..=0.0).contains(&trough),
            "the deepest point {trough} is outside the intended band",
        );
    }

    /// Neighbouring lattice nodes never differ by so much that the surface
    /// becomes a wall a ship cannot see coming. One cell is 24 units across, so
    /// this caps the steepest facet at about 76°.
    #[test]
    fn no_facet_is_a_vertical_wall() {
        let w = world();
        let step = lattice_step(&w);
        let mut worst = 0.0f64;
        for iz in 0..LATTICE_NODES {
            for ix in 0..LATTICE_SEGMENTS {
                let d = (node_height(ix + 1, iz, &w) - node_height(ix, iz, &w)).abs();
                worst = worst.max(d / step);
                let d = (node_height(iz, ix + 1, &w) - node_height(iz, ix, &w)).abs();
                worst = worst.max(d / step);
            }
        }
        assert!(worst < 6.0, "steepest facet is a gradient of {worst}");
    }

    /// The kill plane sits above the water, not on the lake bed.
    #[test]
    fn water_is_solid() {
        let w = world();
        let (lx, lz) = Layout::LAKE_CENTRE;
        assert_eq!(surface_height(lx, lz, &w), w.water_level);
        assert_eq!(
            kill_altitude(lx, lz, &w),
            w.water_level + w.terrain_kill_clearance,
        );
    }

    /// Slope is a gradient magnitude: flat ground reads zero, a mesa rim does
    /// not.
    #[test]
    fn slope_reads_flat_ground_as_flat() {
        let w = world();
        assert!(slope_at(0.0, -w.airfield_z, &w) < 1e-9);
        // Halfway down the ramp off the pad's north edge.
        let rim = w.airfield_half.z + Layout::MESA_APRON + Layout::MESA_RAMP * 0.5;
        assert!(slope_at(0.0, -w.airfield_z + rim, &w) > 0.05);
    }
}
