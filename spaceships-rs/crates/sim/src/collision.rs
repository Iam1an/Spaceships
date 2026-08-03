//! Swept (continuous) collision primitives.
//!
//! # Why swept, and not the point tests the JS uses
//!
//! Every collision query in the JS client is a point-in-sphere test evaluated
//! once per frame, *after* the projectile has already been moved:
//!
//! ```text
//! b.mesh.position.addScaledVector(b.vel, dt);           // bullets.js:92
//! ...
//! if (dx * dx + dy * dy + dz * dz < r * r) { ... }      // bullets.js:102
//! ```
//!
//! Bullets travel at `BULLET_SPEED = 780` units/second (`bullets.js:2`, and
//! again in `bot.js:26`). At the 0.05 s step the shadow bot simulation uses,
//! one frame moves a bullet **39 units**. The things it is tested against are
//! far smaller than that: ships are radius 6.0–7.0 (`main.js`, the
//! `shipHitRadius` passed into `createBullets`), missiles use 6.0
//! (`missiles.js:5`), and "small" asteroids are radius 5–7
//! (`asteroids.js:15`). A 6-unit sphere occupies 13 of the 39 units the bullet
//! crosses, so the sampled point lands inside it roughly a third of the time
//! and the bullet passes clean through on the other two thirds. Nothing is
//! wrong with the aim; the collision test simply never looks between frames.
//!
//! `bullets.js:75` does contain a swept test, but it is wired up only for the
//! `obstacles` list (the moon), never for ships or asteroids.
//!
//! The fix is to treat the frame as a *segment* rather than two samples: solve
//! for the fraction of the step at which the moving sphere first touches the
//! target. That fraction is the return value throughout this module.
//!
//! # Conventions
//!
//! - The moving body is given as `origin` (position at the start of the step),
//!   `motion` (the displacement over the whole step, i.e. `velocity * dt`) and
//!   `radius`.
//! - The return is `Option<f64>`: `None` for no contact during the step, or
//!   `Some(t)` with `t` in `[0, 1]`, the fraction of the step at which contact
//!   first occurs. The impact position is `origin + motion * t`.
//! - Returning `t` rather than `bool` is what lets a caller resolve a frame in
//!   which one bullet crosses several targets: collect the candidate `t`s and
//!   apply the smallest, instead of letting iteration order pick the victim.
//!   [`sweep_first_hit`] does exactly that for a slice of spheres.
//! - Contact is *inclusive*: a sphere exactly tangent to its target counts as a
//!   hit, and `t == 0` and `t == 1` are both valid answers. The static
//!   [`sphere_overlaps_sphere`] / [`sphere_overlaps_aabb`] helpers use the same
//!   convention, so `overlap(a, b) == (swept(a, ZERO_MOTION, b) == Some(0.0))`.
//! - Radii are expected to be non-negative. Negative radii are not meaningful
//!   and produce unspecified (but still finite and deterministic) results.
//! - Any non-finite input yields `None` rather than a garbage `t`. One `NaN`
//!   position that registers as a hit would corrupt a whole match.
//!
//! # Float width
//!
//! `f64`, matching [`Vec3`]. See the "Why `f64`" section in [`crate::math`]:
//! the wire format is JSON, JSON numbers are doubles, and the JS client this
//! must agree with computes in doubles. Narrowing to `f32` here would introduce
//! a rounding step that exists on one side of the network and not the other.
//!
//! # Determinism
//!
//! This module is built exclusively from `+`, `-`, `*`, `/`, `sqrt`, `abs`,
//! `min`, `max` and comparisons. IEEE-754 requires all of those to be correctly
//! rounded, so they are bit-identical on x86-64, aarch64 and wasm32. There are
//! no transcendental calls.
//!
//! Two specific hazards are avoided on purpose:
//!
//! - **No `f64::mul_add`.** A fused multiply-add rounds once where `a * b + c`
//!   rounds twice, so the two disagree in the last bit — and whether a target
//!   has hardware FMA is a property of the machine, not of the simulation. Rust
//!   never contracts `a * b + c` into an FMA on its own (unlike C compilers with
//!   `-ffp-contract=fast`), so simply not calling `mul_add` is sufficient.
//! - **No `sqrt` on a quantity that is then compared against a threshold** when
//!   the squared comparison will do. Squared comparisons are exact.
//!
//! # Precision, and the one tradeoff made for determinism
//!
//! The quadratic solver ([`quadratic_interval`]) uses the numerically stable
//! form `t = c / (-b + sqrt(D))` rather than the textbook
//! `t = (-b - sqrt(D)) / a`. The two are algebraically identical, but the
//! textbook form subtracts two nearly equal positive numbers in the case that
//! matters most — a body starting just outside contact — which amplifies
//! whatever rounding error `b` and `sqrt(D)` already carry, and can drive the
//! result across zero. `bullets.js:86` then rejects it (`return t >= 0 && t <=
//! 1`) and a real contact is reported as a miss on some frames and not others.
//! The stable form adds only like-signed quantities, so with `c > 0` and
//! `-b > 0` the entry root is *provably* positive no matter how the rounding
//! falls. That is a structural guarantee, not a tolerance.
//!
//! What the stable form cannot fix is that `c` — the gap, computed as a
//! difference of two squared lengths — is itself ill-conditioned when a body
//! starts very close to contact: at a gap of `g` from a target of combined
//! radius `R`, the relative error in `c` is about `R * 2^-53 / g`. That is
//! inherent to the formulation and every solver of this shape shares it. It is
//! also harmless at game scale (a gap of 1e-9 units still resolves to seven
//! significant digits), and, crucially, it is *deterministic*: both sides of
//! the network make the identical error. A more accurate formulation that
//! branched on magnitude would risk branching differently on two targets, which
//! is the one thing that must not happen.
//!
//! ```
//! use spaceships_sim::collision::{swept_sphere_sphere, Sphere};
//! use spaceships_sim::math::Vec3;
//!
//! // A bullet crossing 39 units in one step, and a ship 100 units downrange.
//! let origin = Vec3::new(0.0, 0.0, 78.0);
//! let motion = Vec3::new(0.0, 0.0, 39.0);
//! let ship = Sphere::new(Vec3::new(0.0, 0.0, 100.0), 6.0);
//!
//! // The end-of-frame point is at z = 117, nowhere near the ship — the naive
//! // test misses. The swept test finds the crossing at z = 93.5.
//! let t = swept_sphere_sphere(origin, motion, 0.5, ship).expect("hit");
//! assert!(((origin + motion * t).z - 93.5).abs() < 1e-9);
//! ```
//!
//! [`Vec3`]: crate::math::Vec3

use crate::math::Vec3;

/// A sphere: the collision shape of ships, bullets, missiles and asteroids.
///
/// Asteroids in `asteroids.js:184` use `radius = size * 0.95`; ships use the
/// `shipHitRadius` handed to `createBullets`. Which number goes in here is a
/// rules question, not a geometry one — this module only consumes it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Sphere {
    /// Center position.
    pub center: Vec3,
    /// Radius. Expected to be non-negative.
    pub radius: f64,
}

impl Sphere {
    /// Builds a sphere from a center and a radius.
    #[inline]
    #[must_use]
    pub const fn new(center: Vec3, radius: f64) -> Self {
        Sphere { center, radius }
    }
}

/// An axis-aligned box, stored as a center and half-extents.
///
/// This is the shape the JS's solid hulls use: `main.js` builds them as
/// `{ pos, halfSize }` with `MOTHERSHIP_HALF = new THREE.Vector3(45, 18, 35)`,
/// and both `collideSphereWithBox` (ship push-out) and `clipsAvoidance` in
/// `asteroids.js:114` (spawn placement rejection) consume that pair directly.
/// The motherships are gone from this port ([`crate::rules::WorldRules`]); the
/// airfields are the same pair of numbers in a different place.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Aabb {
    /// Box center.
    pub center: Vec3,
    /// Half-width along each axis. Expected to be non-negative.
    pub half_extents: Vec3,
}

impl Aabb {
    /// Builds a box from a center and half-extents.
    #[inline]
    #[must_use]
    pub const fn new(center: Vec3, half_extents: Vec3) -> Self {
        Aabb {
            center,
            half_extents,
        }
    }

    /// Builds a box from opposite corners. `min` must not exceed `max` on any
    /// axis.
    #[inline]
    #[must_use]
    pub fn from_min_max(min: Vec3, max: Vec3) -> Self {
        Aabb::new((min + max) * 0.5, (max - min) * 0.5)
    }

    /// The corner with the smallest coordinate on every axis.
    #[inline]
    #[must_use]
    pub fn min(self) -> Vec3 {
        self.center - self.half_extents
    }

    /// The corner with the largest coordinate on every axis.
    #[inline]
    #[must_use]
    pub fn max(self) -> Vec3 {
        self.center + self.half_extents
    }

    /// The box grown by `amount` on every side.
    #[inline]
    #[must_use]
    pub fn expanded(self, amount: f64) -> Self {
        Aabb::new(self.center, self.half_extents + Vec3::splat(amount))
    }

    /// The point of the box (surface or interior) closest to `p`. Returns `p`
    /// itself when `p` is inside.
    #[inline]
    #[must_use]
    pub fn closest_point(self, p: Vec3) -> Vec3 {
        p.clamp(self.min(), self.max())
    }

    /// True if `p` lies inside or on the boundary.
    #[inline]
    #[must_use]
    pub fn contains_point(self, p: Vec3) -> bool {
        let d = p - self.center;
        d.x.abs() <= self.half_extents.x
            && d.y.abs() <= self.half_extents.y
            && d.z.abs() <= self.half_extents.z
    }
}

/// Sweeps a moving sphere against a stationary sphere.
///
/// This is the workhorse: bullet-vs-ship, bullet-vs-asteroid,
/// missile-vs-asteroid. `motion` is the displacement over the whole step
/// (`velocity * dt`), and the returned `t` is the fraction of that step at
/// which the surfaces first touch.
///
/// Solved as a ray against the Minkowski sum — a point against a sphere of
/// radius `radius + target.radius` — which is exact, not an approximation.
///
/// Degenerate cases, all deliberate:
///
/// - Already overlapping (or exactly tangent) at the start: `Some(0.0)`.
/// - No motion and not already overlapping: `None`. There is no `t` at which
///   anything happens, and reporting a hit would make a parked bullet lethal.
/// - Moving away, or moving exactly perpendicular to the separation: `None`.
/// - Exactly tangent mid-step (zero discriminant): `Some(t)` at the single
///   point of contact. A touch is a hit.
/// - Contact after the end of the step: `None`. The caller advances the body
///   and the next step's sweep will find it.
///
/// ```
/// use spaceships_sim::collision::{swept_sphere_sphere, Sphere};
/// use spaceships_sim::math::Vec3;
///
/// let target = Sphere::new(Vec3::new(0.0, 0.0, 20.0), 6.0);
/// let t = swept_sphere_sphere(Vec3::ZERO, Vec3::new(0.0, 0.0, 39.0), 0.5, target);
/// // Contact 13.5 units in, 13.5 / 39 of the way through the step.
/// assert_eq!(t, Some(13.5 / 39.0));
/// ```
#[inline]
#[must_use]
pub fn swept_sphere_sphere(origin: Vec3, motion: Vec3, radius: f64, target: Sphere) -> Option<f64> {
    let f = origin - target.center;
    let reach = radius + target.radius;
    // `c` is the signed "squared gap" at t = 0: negative inside, zero on the
    // surface, positive outside.
    let c = f.length_squared() - reach * reach;
    if c <= 0.0 {
        // Already interpenetrating, or exactly touching. Both are contact now.
        return Some(0.0);
    }
    let a = motion.length_squared();
    if a == 0.0 || !a.is_finite() {
        // Zero relative velocity and a positive gap: nothing can happen. The
        // finiteness check rides along here rather than as a separate guard on
        // every input — `length_squared` is NaN if any component is, and
        // infinite if the motion is, so one test on `a` covers the whole
        // displacement for the price of one comparison in the hot loop. A NaN
        // in `origin` or in the target is caught further down, where it turns
        // the discriminant NaN and fails the final range check.
        return None;
    }
    let b = f.dot(motion);
    if b >= 0.0 {
        // The gap is already positive and not shrinking. Bailing here also
        // guarantees the root computed below is strictly positive, so the sign
        // of `t` never depends on rounding.
        return None;
    }
    let (entry, _exit) = quadratic_interval(a, b, c)?;
    // Written as `entry <= 1.0` rather than `entry > 1.0` so a NaN entry (only
    // reachable from NaN input) falls through to `None`.
    if entry <= 1.0 {
        Some(entry)
    } else {
        None
    }
}

/// Sweeps a moving sphere against another moving sphere.
///
/// Both bodies are displaced by their `motion` over the step. Used wherever
/// neither side can be treated as parked: missile-vs-ship, ship-vs-ship,
/// bot-vs-bot.
///
/// Solved in relative-velocity space: the target is frozen and the mover
/// carries the difference of the two motions, which is exact for the constant
/// velocities a single fixed step assumes. The reduction costs one subtraction
/// and nothing else, so a zero `target_motion` reproduces
/// [`swept_sphere_sphere`] bit-for-bit — `x - 0.0` is exactly `x` for every
/// finite `x`, so the two paths do not merely agree closely, they run the same
/// arithmetic.
///
/// The returned `t` is still a fraction of the step, so the impact positions
/// are `origin + motion * t` and `target.center + target_motion * t`.
#[inline]
#[must_use]
pub fn swept_sphere_vs_moving_sphere(
    origin: Vec3,
    motion: Vec3,
    radius: f64,
    target: Sphere,
    target_motion: Vec3,
) -> Option<f64> {
    // In the target's frame the target is parked at its own center and the
    // mover starts where it started, carrying the difference of the two
    // motions. `motion - Vec3::ZERO` is exactly `motion`, which is what makes
    // the stationary case reduce bit-for-bit.
    swept_sphere_sphere(origin, motion - target_motion, radius, target)
}

/// Sweeps a moving sphere against an axis-aligned box.
///
/// Needed for the airfield volumes and for rejecting asteroid spawn positions
/// that clip fixed geometry.
///
/// The test is *exact*, including at edges and corners. The naive version of
/// this test — grow the box by the radius and cast a ray at it — reports hits
/// in the corner regions that never happen, because the true swept volume
/// (a Minkowski sum) has rounded edges and corners, not square ones. The
/// difference is up to `radius * (sqrt(3) - 1)` of phantom box, which for a
/// ship of radius 3.3 is over 2 units of invisible wall on every corner of
/// every hull.
///
/// The expanded box is still used, as a conservative first pass: it contains
/// the true swept volume, so missing it means missing the box, and its entry
/// point tells us which feature — face, edge or corner — to solve exactly. Face
/// approaches (the overwhelming majority) finish in that first pass; only edge
/// and corner approaches pay for the 12 edge cylinders and 8 corner spheres.
#[must_use]
pub fn swept_sphere_aabb(origin: Vec3, motion: Vec3, radius: f64, target: Aabb) -> Option<f64> {
    // The slab test below mixes `min`/`max` with divisions, and `f64::min`
    // silently ignores a NaN operand, so a NaN could survive to the result.
    // Reject it up front instead.
    if !origin.is_finite()
        || !motion.is_finite()
        || !radius.is_finite()
        || !target.center.is_finite()
        || !target.half_extents.is_finite()
    {
        return None;
    }
    if sphere_overlaps_aabb(Sphere::new(origin, radius), target) {
        return Some(0.0);
    }
    // Conservative bound: the true swept volume is contained in the box grown
    // by `radius`, so failing this is a definite miss.
    let entry = ray_aabb_entry(origin, motion, target.expanded(radius))?;
    let contact = origin + motion * entry;
    // How many axes is the entry point outside the *original* box on? One means
    // it lies on a flat face of the swept volume, where the expanded box and the
    // true shape coincide, so `entry` is already the exact answer. (Zero is only
    // reachable with `radius == 0`, where the two shapes are the same box.)
    if outside_axis_count(contact, target) <= 1 {
        return Some(entry);
    }
    // Edge or corner approach: solve the rounded part exactly.
    swept_sphere_box_features(origin, motion, radius, target)
}

/// How far a point inside `target` must travel along `dir` to reach its
/// surface, or `None` if the point is not strictly inside.
///
/// The escape hatch for a body that is *already* interpenetrating and cannot be
/// resolved by pushing along one surface normal — a ship in the crevice where
/// two overlapping asteroids meet, where each rock's push-out lands it inside
/// the other. Moving along a single direction until every body is behind it
/// always terminates, because the bodies are bounded; this is the "how far"
/// half of that. See `slide_clear` in [`crate::ship`].
///
/// `dir` must be unit length. The answer is a distance in world units, not a
/// fraction of a step — there is no step here, only a displacement to apply.
///
/// Solved as the far root of the same quadratic [`swept_sphere_sphere`] uses,
/// with `a == 1` because `dir` is normalized. Strictly inside means the
/// constant term is negative, so the discriminant is positive and the root is
/// real without a branch on it.
///
/// ```
/// use spaceships_sim::collision::{sphere_exit_distance, Sphere};
/// use spaceships_sim::math::Vec3;
///
/// // Five units inside a radius-10 sphere, heading out along +z: 15 to go.
/// let rock = Sphere::new(Vec3::ZERO, 10.0);
/// let d = sphere_exit_distance(Vec3::new(0.0, 0.0, -5.0), Vec3::Z, rock);
/// assert_eq!(d, Some(15.0));
/// // Outside it already: nothing to do.
/// assert_eq!(sphere_exit_distance(Vec3::new(0.0, 0.0, -20.0), Vec3::Z, rock), None);
/// ```
#[inline]
#[must_use]
pub fn sphere_exit_distance(origin: Vec3, dir: Vec3, target: Sphere) -> Option<f64> {
    let f = origin - target.center;
    let c = f.length_squared() - target.radius * target.radius;
    if c >= 0.0 || c.is_nan() {
        // The NaN arm is explicit: `c >= 0.0` is false for a NaN, and letting
        // one through would reach the square root and return a NaN distance.
        return None;
    }
    let b = f.dot(dir);
    let disc = b * b - c;
    if disc < 0.0 {
        // Unreachable for a unit `dir` and `c < 0`; a non-unit or non-finite
        // one lands here instead of returning a NaN distance.
        return None;
    }
    let s = -b + disc.sqrt();
    if s.is_finite() && s > 0.0 {
        Some(s)
    } else {
        None
    }
}

/// True if two spheres share any point. Tangent spheres count as overlapping.
///
/// The cheap static test for spawn placement: reject a candidate asteroid or
/// respawn position that would appear already touching something.
#[inline]
#[must_use]
pub fn sphere_overlaps_sphere(a: Sphere, b: Sphere) -> bool {
    let reach = a.radius + b.radius;
    // Squared comparison: exact, and no `sqrt`.
    a.center.distance_squared(b.center) <= reach * reach
}

/// True if a sphere shares any point with a box. Tangent counts as overlapping.
///
/// This is the correct form of the placement rejection in `asteroids.js:114`,
/// which compares per-axis distances against `halfSize + margin` and therefore
/// rejects a box-shaped region around each volume rather than the true rounded
/// one.
#[inline]
#[must_use]
pub fn sphere_overlaps_aabb(sphere: Sphere, aabb: Aabb) -> bool {
    let closest = aabb.closest_point(sphere.center);
    sphere.center.distance_squared(closest) <= sphere.radius * sphere.radius
}

/// Sweeps one moving sphere against a slice of stationary spheres and returns
/// the **earliest** contact as `(index, t)`.
///
/// This is the bullet-vs-60-asteroids inner loop. The JS equivalent
/// (`bullets.js:96`) walks the asteroid list backwards and takes the *first*
/// match it stumbles on, which is whichever rock happens to sit later in the
/// array — so a bullet fired through two overlapping asteroids damages an
/// arbitrary one of them. This returns the one actually hit first.
///
/// Ties go to the lowest index, so the result depends only on the contents of
/// the slice and never on iteration luck.
///
/// Allocation-free, and cheap per candidate: targets whose center is too far
/// from the midpoint of the swept segment to possibly touch it are rejected
/// with one subtraction, one dot product and one comparison, before the
/// quadratic is set up. That rejection is conservative — it can only skip
/// candidates that provably cannot be hit — so it does not change the answer.
#[must_use]
pub fn sweep_first_hit(
    origin: Vec3,
    motion: Vec3,
    radius: f64,
    targets: &[Sphere],
) -> Option<(usize, f64)> {
    let mid = origin + motion * 0.5;
    let half_len = motion.length() * 0.5;
    let mut best: Option<(usize, f64)> = None;
    for (i, target) in targets.iter().enumerate() {
        // Everything the moving sphere touches this step lies within
        // `half_len + radius` of `mid`; a target further away than that plus its
        // own radius cannot be reached.
        let reach = half_len + radius + target.radius;
        if mid.distance_squared(target.center) > reach * reach {
            continue;
        }
        if let Some(t) = swept_sphere_sphere(origin, motion, radius, *target) {
            match best {
                // Strict `<`: an exact tie keeps the earlier index.
                Some((_, best_t)) if t >= best_t => {}
                _ => best = Some((i, t)),
            }
        }
    }
    best
}

/// Solves `a * t^2 + 2 * b * t + c <= 0` and returns the closed interval of `t`
/// where it holds, or `None` if it never does.
///
/// Note the factor of two: callers pass the *half* linear coefficient, which is
/// what falls out of `f.dot(motion)` naturally and keeps the discriminant at
/// `b * b - a * c` instead of `b * b - 4 * a * c`.
///
/// `a` must be strictly positive (it is always a squared length here, checked
/// for zero by the caller).
///
/// Uses the stable quadratic formula: compute the root that has no cancellation
/// directly, and get the other one from the fact that the roots multiply to
/// `c / a`. See the module docs on precision for why the textbook form is not
/// good enough for grazing contacts.
#[inline]
fn quadratic_interval(a: f64, b: f64, c: f64) -> Option<(f64, f64)> {
    debug_assert!(a > 0.0, "quadratic_interval needs a positive leading term");
    let disc = b * b - a * c;
    if disc < 0.0 {
        // Also the NaN path: `NaN < 0.0` is false, so a NaN discriminant flows
        // on and produces NaN roots, which every downstream range check
        // rejects.
        return None;
    }
    let root = disc.sqrt();
    // `q` adds two quantities of the same sign, so it never cancels.
    let q = if b >= 0.0 { -(b + root) } else { -b + root };
    if q == 0.0 {
        // Reachable only when b == 0 and disc == 0, which forces c == 0: a
        // double root at zero.
        return Some((0.0, 0.0));
    }
    let r0 = q / a;
    let r1 = c / q;
    Some(if r0 <= r1 { (r0, r1) } else { (r1, r0) })
}

/// The interval of `t` for which `o + m * t` lies within `[lo, hi]`.
///
/// Returns `None` when the axis is stationary outside the slab. The infinite
/// bounds for a stationary in-slab axis are intentional: they are always
/// intersected against `[0, 1]` before use, so no infinity reaches a caller.
#[inline]
fn slab_interval(o: f64, m: f64, lo: f64, hi: f64) -> Option<(f64, f64)> {
    if m == 0.0 {
        // Tested explicitly rather than letting the division produce an
        // infinity, because `0.0 * inf` is NaN and would poison the min/max
        // chain in `ray_aabb_entry`.
        if o < lo || o > hi {
            return None;
        }
        return Some((f64::NEG_INFINITY, f64::INFINITY));
    }
    let inv = 1.0 / m;
    let t_lo = (lo - o) * inv;
    let t_hi = (hi - o) * inv;
    Some(if t_lo <= t_hi {
        (t_lo, t_hi)
    } else {
        (t_hi, t_lo)
    })
}

/// Earliest `t` in `[0, 1]` at which the point `origin + motion * t` is inside
/// `aabb`. Returns `Some(0.0)` when it starts inside.
#[inline]
fn ray_aabb_entry(origin: Vec3, motion: Vec3, aabb: Aabb) -> Option<f64> {
    let min = aabb.min();
    let max = aabb.max();
    let mut enter = 0.0f64;
    let mut exit = 1.0f64;
    for i in 0..3 {
        let (lo, hi) = slab_interval(axis(origin, i), axis(motion, i), axis(min, i), axis(max, i))?;
        if lo > enter {
            enter = lo;
        }
        if hi < exit {
            exit = hi;
        }
        if enter > exit {
            return None;
        }
    }
    Some(enter)
}

/// How many of the three axes `p` lies strictly outside `aabb` on.
#[inline]
fn outside_axis_count(p: Vec3, aabb: Aabb) -> u32 {
    let d = p - aabb.center;
    u32::from(d.x.abs() > aabb.half_extents.x)
        + u32::from(d.y.abs() > aabb.half_extents.y)
        + u32::from(d.z.abs() > aabb.half_extents.z)
}

/// Component `i` of `v`, for the axis loops. `Vec3` is a named-field struct by
/// design (it is what the rest of the simulation reads), so indexed access is
/// spelled out here rather than adding an `Index` impl to the public API.
#[inline]
fn axis(v: Vec3, i: usize) -> f64 {
    match i {
        0 => v.x,
        1 => v.y,
        _ => v.z,
    }
}

/// The two axes perpendicular to `k`.
#[inline]
fn perpendicular_axes(k: usize) -> (usize, usize) {
    match k {
        0 => (1, 2),
        1 => (2, 0),
        _ => (0, 1),
    }
}

/// Exact sweep against the rounded parts of the box's swept volume: the 12 edge
/// cylinders and the 8 corner spheres.
///
/// Together with the flat faces (handled by the expanded-box pass in
/// [`swept_sphere_aabb`]) these cover the whole Minkowski sum of the box and the
/// sphere, so the union of the tests below is exact rather than conservative.
///
/// All 20 features are tested and the smallest `t` wins. Restricting the set to
/// the features adjacent to the entry point would be faster, but this path only
/// runs for the minority of approaches that arrive at an edge or corner, and a
/// correct answer that is easy to check is worth more here than a branchier one.
fn swept_sphere_box_features(origin: Vec3, motion: Vec3, radius: f64, aabb: Aabb) -> Option<f64> {
    let min = aabb.min();
    let max = aabb.max();
    let mut best: Option<f64> = None;

    // 12 edges: for each axis, the four parallel edges at the corners of the
    // other two axes.
    for k in 0..3 {
        let (i, j) = perpendicular_axes(k);
        for &pick_i in &[false, true] {
            for &pick_j in &[false, true] {
                let ci = axis(if pick_i { max } else { min }, i);
                let cj = axis(if pick_j { max } else { min }, j);
                let t = ray_axis_cylinder(
                    origin,
                    motion,
                    (k, i, j),
                    (ci, cj),
                    (axis(min, k), axis(max, k)),
                    radius,
                );
                best = keep_earlier(best, t);
            }
        }
    }

    // 8 corners: a ray against a sphere is a zero-radius sphere sweep.
    for &cx in &[min.x, max.x] {
        for &cy in &[min.y, max.y] {
            for &cz in &[min.z, max.z] {
                let corner = Sphere::new(Vec3::new(cx, cy, cz), radius);
                let t = swept_sphere_sphere(origin, motion, 0.0, corner);
                best = keep_earlier(best, t);
            }
        }
    }

    best
}

/// Earliest `t` in `[0, 1]` at which the point `origin + motion * t` is inside
/// the finite, axis-aligned cylinder of radius `r` whose axis runs along `k`
/// from `k_lo` to `k_hi` at perpendicular position `(ci, cj)`.
///
/// Caps are deliberately absent: the corner spheres in
/// [`swept_sphere_box_features`] already cover the ends of every edge, and
/// duplicating them here would be dead arithmetic.
#[inline]
fn ray_axis_cylinder(
    origin: Vec3,
    motion: Vec3,
    axes: (usize, usize, usize),
    center: (f64, f64),
    span: (f64, f64),
    r: f64,
) -> Option<f64> {
    let (k, i, j) = axes;
    let (ci, cj) = center;
    let (k_lo, k_hi) = span;

    let du = axis(origin, i) - ci;
    let dv = axis(origin, j) - cj;
    let mi = axis(motion, i);
    let mj = axis(motion, j);

    // Radial: the interval where the point is within `r` of the axis.
    let a = mi * mi + mj * mj;
    let c = du * du + dv * dv - r * r;
    let (mut enter, mut exit) = if a == 0.0 {
        if c > 0.0 {
            return None;
        }
        (f64::NEG_INFINITY, f64::INFINITY)
    } else {
        quadratic_interval(a, du * mi + dv * mj, c)?
    };

    // Axial: the interval where the point is between the cylinder's ends.
    let (k_enter, k_exit) = slab_interval(axis(origin, k), axis(motion, k), k_lo, k_hi)?;
    if k_enter > enter {
        enter = k_enter;
    }
    if k_exit < exit {
        exit = k_exit;
    }
    if enter < 0.0 {
        enter = 0.0;
    }
    if exit > 1.0 {
        exit = 1.0;
    }
    if enter <= exit {
        Some(enter)
    } else {
        None
    }
}

/// Keeps whichever of two optional impact times is earlier.
#[inline]
fn keep_earlier(best: Option<f64>, candidate: Option<f64>) -> Option<f64> {
    match (best, candidate) {
        (Some(b), Some(c)) => Some(if c < b { c } else { b }),
        (None, c) => c,
        (b, None) => b,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        quadratic_interval, ray_aabb_entry, sphere_exit_distance, sphere_overlaps_aabb,
        sphere_overlaps_sphere, sweep_first_hit, swept_sphere_aabb, swept_sphere_sphere,
        swept_sphere_vs_moving_sphere, Aabb, Sphere,
    };
    use crate::math::Vec3;
    use crate::rng::Rng;

    /// `BULLET_SPEED` from `bullets.js:2` (and again from `bot.js:26`).
    const BULLET_SPEED: f64 = 780.0;
    /// The step the bot shadow simulation runs at, and the frame budget the
    /// tunneling analysis is based on.
    const FRAME_DT: f64 = 0.05;
    /// `RADIUS` from `bullets.js:8`.
    const BULLET_RADIUS: f64 = 0.5;
    /// The larger `shipHitRadius` `createBullets` is constructed with.
    const SHIP_HIT_RADIUS: f64 = 6.0;
    /// `MOTHERSHIP_HALF` from `main.js`. The hulls it described are gone from
    /// the world, but it is still the box these tests are written against —
    /// three unequal half-extents, none of them degenerate.
    const HULL_HALF: Vec3 = Vec3::new(45.0, 18.0, 35.0);

    fn v(x: f64, y: f64, z: f64) -> Vec3 {
        Vec3::new(x, y, z)
    }

    // ---------------------------------------------------------------------
    // The regression this module exists for.
    // ---------------------------------------------------------------------

    #[test]
    fn bullet_tunnels_through_a_ship_that_the_per_frame_point_test_never_sees() {
        // A bullet fired straight at a ship 100 units downrange, stepped at
        // 0.05 s exactly as `bullets.js:92` does.
        let step = Vec3::Z * (BULLET_SPEED * FRAME_DT);
        let ship = Sphere::new(v(0.0, 0.0, 100.0), SHIP_HIT_RADIUS);
        let reach = SHIP_HIT_RADIUS + BULLET_RADIUS;

        // One frame covers ~39 units; the ship's diameter is 13.
        assert!((step.length() - 39.0).abs() < 1e-9);
        assert!(step.length() > 2.0 * reach);

        let mut pos = Vec3::ZERO;
        let mut naive_hits = 0;
        let mut swept_hit: Option<(usize, f64, Vec3)> = None;
        for frame in 0..6 {
            let next = pos + step;

            // What `bullets.js` does today: move, then test the new point.
            if next.distance_squared(ship.center) < reach * reach {
                naive_hits += 1;
            }

            if swept_hit.is_none() {
                if let Some(t) = swept_sphere_sphere(pos, step, BULLET_RADIUS, ship) {
                    swept_hit = Some((frame, t, pos + step * t));
                }
            }
            pos = next;
        }

        // The bullet's sampled positions are 0, 39, 78, 117, ... The ship sits
        // at 100 with a 6.5-unit reach, i.e. it spans 93.5 to 106.5. No sample
        // ever lands in that band, so the point test scores nothing at all.
        assert_eq!(
            naive_hits, 0,
            "per-frame point test should miss entirely — that is the bug"
        );

        let (frame, t, impact) = swept_hit.expect("swept test must catch the crossing");
        // Frames 0..1 cover z in [0, 39], frame 1 [39, 78], frame 2 [78, 117].
        assert_eq!(frame, 2);
        // Contact at z = 93.5, which is 15.5 units into a 39-unit step.
        assert!((impact.z - 93.5).abs() < 1e-9, "impact at z = {}", impact.z);
        assert!((t - 15.5 / 39.0).abs() < 1e-9, "t = {t}");
        assert!((0.0..=1.0).contains(&t));
        // Sanity: the impact point really is on the ship's surface.
        assert!((impact.distance(ship.center) - reach).abs() < 1e-9);
    }

    #[test]
    fn a_bullet_passing_just_outside_the_radius_does_not_hit() {
        let step = v(0.0, 0.0, 39.0);
        let ship = Sphere::new(v(0.0, 0.0, 20.0), SHIP_HIT_RADIUS);
        let reach = SHIP_HIT_RADIUS + BULLET_RADIUS; // 6.5

        // Dead centre: hit.
        assert!(swept_sphere_sphere(Vec3::ZERO, step, BULLET_RADIUS, ship).is_some());
        // A hair inside the combined radius: still a hit.
        let inside = v(reach - 1e-6, 0.0, 0.0);
        assert!(swept_sphere_sphere(inside, step, BULLET_RADIUS, ship).is_some());
        // A hair outside: clean miss.
        let outside = v(reach + 1e-6, 0.0, 0.0);
        assert!(swept_sphere_sphere(outside, step, BULLET_RADIUS, ship).is_none());
        // Comfortably outside: miss.
        let clear = v(7.0, 0.0, 0.0);
        assert!(swept_sphere_sphere(clear, step, BULLET_RADIUS, ship).is_none());
    }

    // ---------------------------------------------------------------------
    // Degenerate cases.
    // ---------------------------------------------------------------------

    #[test]
    fn zero_motion_hits_only_when_already_overlapping() {
        let target = Sphere::new(v(0.0, 0.0, 10.0), 6.0);
        // Parked outside: no contact, ever.
        assert_eq!(
            swept_sphere_sphere(Vec3::ZERO, Vec3::ZERO, 0.5, target),
            None
        );
        // Parked inside: contact at t = 0.
        assert_eq!(
            swept_sphere_sphere(v(0.0, 0.0, 8.0), Vec3::ZERO, 0.5, target),
            Some(0.0)
        );
    }

    #[test]
    fn already_overlapping_reports_impact_at_the_start_of_the_step() {
        let target = Sphere::new(Vec3::ZERO, 10.0);
        // Deeply inside, moving further in.
        assert_eq!(
            swept_sphere_sphere(v(1.0, 0.0, 0.0), v(0.0, 0.0, 5.0), 1.0, target),
            Some(0.0)
        );
        // Inside, moving out: still touching now, so still a hit now.
        assert_eq!(
            swept_sphere_sphere(v(9.0, 0.0, 0.0), v(100.0, 0.0, 0.0), 1.0, target),
            Some(0.0)
        );
    }

    #[test]
    fn exact_tangency_counts_as_a_hit() {
        // Offset laterally by exactly the combined radius. Every quantity here
        // is a dyadic rational, so the discriminant comes out exactly zero and
        // the tangency is not a matter of rounding.
        let ship = Sphere::new(v(0.0, 0.0, 20.0), 6.0);
        let t = swept_sphere_sphere(v(6.5, 0.0, 0.0), v(0.0, 0.0, 39.0), 0.5, ship);
        let t = t.expect("a tangent grazing contact is a contact");
        // Closest approach is at the sphere's z, i.e. 20 of the 39 units.
        assert!((t - 20.0 / 39.0).abs() < 1e-12, "t = {t}");

        // And the discriminant really was exactly zero.
        let f = v(6.5, 0.0, -20.0);
        let m = v(0.0, 0.0, 39.0);
        let reach = 6.5f64;
        let a = m.length_squared();
        let b = f.dot(m);
        let c = f.length_squared() - reach * reach;
        assert_eq!(b * b - a * c, 0.0);
    }

    #[test]
    fn tangency_at_the_start_of_the_step_counts_as_a_hit() {
        // Surfaces exactly touching at t = 0 while moving apart.
        let target = Sphere::new(v(0.0, 0.0, 10.0), 6.0);
        assert_eq!(
            swept_sphere_sphere(v(0.0, 0.0, 3.5), v(0.0, 0.0, -50.0), 0.5, target),
            Some(0.0)
        );
    }

    #[test]
    fn grazing_contacts_do_not_flicker_between_hit_and_miss() {
        // Walk the lateral offset across the exact tangency point at 6.5 in
        // 2500 steps. Hits must form a clean prefix: once the sweep starts
        // missing it must never claim a hit again. A solver that loses
        // precision near tangency produces isolated misses inside the hit
        // region (and vice versa), which on screen is a weapon that randomly
        // fails at the edge of the hitbox.
        let ship = Sphere::new(v(0.0, 0.0, 20.0), 6.0);
        let step = v(0.0, 0.0, 39.0);
        let mut missed = false;
        for i in 0..2500 {
            let offset = 6.0 + f64::from(i) * 0.0004;
            let hit = swept_sphere_sphere(v(offset, 0.0, 0.0), step, 0.5, ship).is_some();
            assert!(
                !(hit && missed),
                "hit reappeared after a miss at offset {offset}"
            );
            if !hit {
                missed = true;
            }
            // The boundary is 6.5; stay honest about which side we are on.
            if offset < 6.4999 {
                assert!(hit, "clear hit reported as a miss at offset {offset}");
            }
            if offset > 6.5001 {
                assert!(!hit, "clear miss reported as a hit at offset {offset}");
            }
        }
        assert!(missed, "the sweep never crossed the boundary");
    }

    #[test]
    fn contact_from_just_outside_the_surface_stays_positive_and_accurate() {
        // A body starting a hair outside contact and crawling inward. This is
        // where the textbook root formula subtracts two nearly equal numbers,
        // amplifying their rounding error and — at the extreme — pushing the
        // root below zero, which `bullets.js:86` would then reject outright.
        // The form used here builds the entry root from two same-signed
        // quantities, so it cannot change sign no matter how the rounding lands.
        let reach = 6.5f64;
        let target = Sphere::new(Vec3::ZERO, 6.0);
        let speed = 1e-3;

        // Accuracy, at gaps where the gap itself is still well-conditioned.
        for &gap in &[1e-6, 1e-7, 1e-8, 1e-9] {
            let start = -(reach + gap);
            let origin = v(0.0, 0.0, start);
            let t = swept_sphere_sphere(origin, v(0.0, 0.0, speed), 0.5, target).expect("hit");
            let expected = (-start - reach) / speed;
            assert!(
                (t - expected).abs() / expected < 1e-5,
                "gap {gap}: t = {t}, expected {expected}"
            );
        }

        // Sign, all the way down to gaps that are only a few ulps wide, where
        // the magnitude of `t` can no longer be trusted but a hit must still
        // read as a hit.
        for i in 0..64 {
            let gap = 1e-9 * 0.5f64.powi(i % 32);
            let origin = v(0.0, 0.0, -(reach + gap));
            match swept_sphere_sphere(origin, v(0.0, 0.0, speed), 0.5, target) {
                Some(t) => assert!(t >= 0.0, "negative t at gap {gap}: {t}"),
                // Once the gap falls below an ulp of `reach` the start point is
                // indistinguishable from touching, which reports Some(0.0)
                // above; a None here would mean a contact was dropped.
                None => panic!("dropped a contact at gap {gap}"),
            }
        }
    }

    #[test]
    fn contact_after_the_end_of_the_step_is_not_reported() {
        let target = Sphere::new(v(0.0, 0.0, 100.0), 6.0);
        // Reaches z = 39 this step; contact would be at 93.5.
        assert_eq!(
            swept_sphere_sphere(Vec3::ZERO, v(0.0, 0.0, 39.0), 0.5, target),
            None
        );
        // Exactly reaching the surface at the end of the step does count.
        let t = swept_sphere_sphere(Vec3::ZERO, v(0.0, 0.0, 93.5), 0.5, target);
        assert_eq!(t, Some(1.0));
    }

    #[test]
    fn moving_away_never_reports_a_hit() {
        let target = Sphere::new(Vec3::ZERO, 6.0);
        assert_eq!(
            swept_sphere_sphere(v(0.0, 0.0, 10.0), v(0.0, 0.0, 500.0), 0.5, target),
            None
        );
        // Moving exactly perpendicular to the separation, outside reach.
        assert_eq!(
            swept_sphere_sphere(v(0.0, 0.0, 10.0), v(500.0, 0.0, 0.0), 0.5, target),
            None
        );
    }

    #[test]
    fn non_finite_input_never_reports_a_hit() {
        // Deliberately downrange, so that a reported hit can only come from the
        // bad input and not from a legitimate overlap at t = 0.
        let target = Sphere::new(v(0.0, 0.0, 100.0), 6.0);
        let nan = v(f64::NAN, 0.0, 0.0);
        let inf = v(f64::INFINITY, 0.0, 0.0);
        assert_eq!(
            swept_sphere_sphere(nan, v(0.0, 0.0, 39.0), 0.5, target),
            None
        );
        assert_eq!(swept_sphere_sphere(Vec3::ZERO, nan, 0.5, target), None);
        assert_eq!(swept_sphere_sphere(Vec3::ZERO, inf, 0.5, target), None);
        assert_eq!(
            swept_sphere_sphere(Vec3::ZERO, v(0.0, 0.0, 39.0), f64::NAN, target),
            None
        );
        assert_eq!(
            swept_sphere_sphere(Vec3::ZERO, v(0.0, 0.0, 200.0), 0.5, Sphere::new(nan, 6.0)),
            None
        );

        let box_ = Aabb::new(v(0.0, 0.0, 200.0), HULL_HALF);
        assert_eq!(swept_sphere_aabb(nan, v(1.0, 0.0, 0.0), 3.3, box_), None);
        assert_eq!(swept_sphere_aabb(inf, v(-1.0, 0.0, 0.0), 3.3, box_), None);
        assert_eq!(swept_sphere_aabb(Vec3::ZERO, nan, 3.3, box_), None);
        assert_eq!(
            swept_sphere_aabb(v(100.0, 0.0, 0.0), v(-100.0, 0.0, 0.0), f64::NAN, box_),
            None
        );

        assert!(!sphere_overlaps_sphere(
            Sphere::new(nan, 1.0),
            Sphere::new(Vec3::ZERO, 1.0)
        ));
        assert!(!sphere_overlaps_aabb(Sphere::new(nan, 1.0), box_));
    }

    #[test]
    fn zero_radius_bodies_behave_like_rays_and_points() {
        // A radius-zero sweep is a ray cast, which is what `raySphereDist` in
        // main.js does for beams.
        let target = Sphere::new(v(0.0, 0.0, 50.0), 10.0);
        let t = swept_sphere_sphere(Vec3::ZERO, v(0.0, 0.0, 100.0), 0.0, target).unwrap();
        assert!((t - 0.4).abs() < 1e-12);
        // Two zero-radius spheres only ever meet exactly.
        assert!(!sphere_overlaps_sphere(
            Sphere::new(Vec3::ZERO, 0.0),
            Sphere::new(v(1e-12, 0.0, 0.0), 0.0)
        ));
        assert!(sphere_overlaps_sphere(
            Sphere::new(Vec3::ZERO, 0.0),
            Sphere::new(Vec3::ZERO, 0.0)
        ));
    }

    // ---------------------------------------------------------------------
    // Invariants.
    // ---------------------------------------------------------------------

    #[test]
    fn translating_both_bodies_does_not_change_the_impact_time() {
        let origin = v(0.0, 0.0, 0.0);
        let motion = v(3.0, -1.5, 39.0);
        let target = Sphere::new(v(2.0, -1.0, 20.0), 6.0);
        let base = swept_sphere_sphere(origin, motion, 0.5, target).expect("hit");

        // A translation by exactly representable values: the intermediate
        // differences are computed exactly, so the answer is bit-identical.
        let d = v(1024.0, -2048.0, 512.0);
        let shifted = swept_sphere_sphere(
            origin + d,
            motion,
            0.5,
            Sphere::new(target.center + d, target.radius),
        )
        .expect("hit");
        assert_eq!(base.to_bits(), shifted.to_bits());

        // A far-away, less friendly translation: still the same answer to well
        // within any tolerance the game cares about.
        let far = v(1.0e6, -3.5e5, 7.25e5);
        let shifted_far = swept_sphere_sphere(
            origin + far,
            motion,
            0.5,
            Sphere::new(target.center + far, target.radius),
        )
        .expect("hit");
        assert!((shifted_far - base).abs() < 1e-9, "{shifted_far} vs {base}");

        // The same for the box sweep — but only to a tolerance, not bit for
        // bit. The slab test works in absolute coordinates (`center -
        // half_extents`), and a half-extent grown by a radius is generally not
        // exactly representable, so translating the box re-rounds its faces.
        // The error is proportional to the distance from the origin, which is
        // why match space is centred on the action rather than at some far-off
        // world origin.
        let box_ = Aabb::new(v(0.0, 0.0, 60.0), HULL_HALF);
        let b0 = swept_sphere_aabb(origin, motion, 3.3, box_).expect("hit");
        let b1 = swept_sphere_aabb(
            origin + d,
            motion,
            3.3,
            Aabb::new(box_.center + d, box_.half_extents),
        )
        .expect("hit");
        assert!((b1 - b0).abs() < 1e-12, "{b1} vs {b0}");
    }

    #[test]
    fn moving_vs_moving_reduces_to_the_stationary_case() {
        let origin = v(1.0, 2.0, 0.0);
        let motion = v(0.0, 0.0, 39.0);
        let target = Sphere::new(v(0.0, 0.0, 20.0), 6.0);

        let stationary = swept_sphere_sphere(origin, motion, 0.5, target);
        let moving = swept_sphere_vs_moving_sphere(origin, motion, 0.5, target, Vec3::ZERO);
        assert_eq!(stationary, moving);
        assert_eq!(
            stationary.unwrap().to_bits(),
            moving.unwrap().to_bits(),
            "the reduction must be bit-exact, not merely close"
        );

        // And a miss stays a miss.
        let far = Sphere::new(v(500.0, 0.0, 20.0), 6.0);
        assert_eq!(
            swept_sphere_sphere(origin, motion, 0.5, far),
            swept_sphere_vs_moving_sphere(origin, motion, 0.5, far, Vec3::ZERO)
        );
    }

    #[test]
    fn head_on_closers_meet_sooner_than_either_alone() {
        // Two ships 100 apart closing at 50 each over the step. Combined reach
        // 12, so they touch after closing 88 units of the 100.
        let a_origin = Vec3::ZERO;
        let a_motion = v(0.0, 0.0, 50.0);
        let b = Sphere::new(v(0.0, 0.0, 100.0), 6.0);
        let b_motion = v(0.0, 0.0, -50.0);

        let t = swept_sphere_vs_moving_sphere(a_origin, a_motion, 6.0, b, b_motion).expect("hit");
        assert!((t - 88.0 / 100.0).abs() < 1e-12, "t = {t}");
        // The two impact positions really are 12 apart.
        let pa = a_origin + a_motion * t;
        let pb = b.center + b_motion * t;
        assert!((pa.distance(pb) - 12.0).abs() < 1e-9);

        // With the target parked, the same closing distance takes the whole
        // step and then some, so it does not land.
        assert_eq!(
            swept_sphere_vs_moving_sphere(a_origin, a_motion, 6.0, b, Vec3::ZERO),
            None
        );
    }

    #[test]
    fn bodies_moving_in_convoy_never_touch() {
        // Identical motion: zero relative velocity. Formations must not
        // self-destruct.
        let motion = v(10.0, 0.0, 400.0);
        let target = Sphere::new(v(0.0, 0.0, 20.0), 6.0);
        assert_eq!(
            swept_sphere_vs_moving_sphere(Vec3::ZERO, motion, 0.5, target, motion),
            None
        );
        // Unless they were already touching.
        let touching = Sphere::new(v(0.0, 0.0, 3.0), 6.0);
        assert_eq!(
            swept_sphere_vs_moving_sphere(Vec3::ZERO, motion, 0.5, touching, motion),
            Some(0.0)
        );
    }

    #[test]
    fn the_exit_distance_lands_exactly_on_the_surface() {
        let rock = Sphere::new(v(3.0, -2.0, 10.0), 12.0);
        // From the centre, every bearing exits at the radius.
        for dir in [Vec3::X, -Vec3::Y, Vec3::Z] {
            let d = sphere_exit_distance(rock.center, dir, rock).expect("inside");
            assert!((d - 12.0).abs() < 1e-12, "{d}");
        }
        // From an off-centre point, the exit really is on the surface.
        let inside = rock.center + v(4.0, 3.0, -2.0);
        let dir = v(1.0, 2.0, 3.0).normalize();
        let d = sphere_exit_distance(inside, dir, rock).expect("inside");
        let out = inside + dir * d;
        assert!((out.distance(rock.center) - rock.radius).abs() < 1e-9);
        // And one step further really is outside.
        assert!(sphere_exit_distance(out + dir * 1e-6, dir, rock).is_none());
    }

    #[test]
    fn the_exit_distance_is_none_from_outside_and_from_nowhere() {
        let rock = Sphere::new(Vec3::ZERO, 10.0);
        // Outside, whichever way it is pointing.
        assert_eq!(sphere_exit_distance(v(0.0, 0.0, 40.0), Vec3::Z, rock), None);
        assert_eq!(
            sphere_exit_distance(v(0.0, 0.0, 40.0), -Vec3::Z, rock),
            None
        );
        // Exactly on the surface is not inside.
        assert_eq!(sphere_exit_distance(v(0.0, 0.0, 10.0), Vec3::Z, rock), None);
        // And nothing non-finite escapes as a distance.
        let nan = v(f64::NAN, 0.0, 0.0);
        assert_eq!(sphere_exit_distance(nan, Vec3::Z, rock), None);
        assert_eq!(sphere_exit_distance(Vec3::ZERO, nan, rock), None);
        assert_eq!(
            sphere_exit_distance(Vec3::ZERO, Vec3::Z, Sphere::new(nan, 10.0)),
            None
        );
    }

    #[test]
    fn a_fixed_bearing_leaves_a_knot_of_spheres_for_good() {
        // The property `ship::slide_clear` rests on: pick a bearing, step to the
        // far side of each sphere still containing the point, and every sphere
        // left behind stays behind — spheres are convex, so a ray leaves each of
        // them exactly once. Without that, the escape can cycle forever.
        let mut rng = Rng::new(0x5111_DE01);
        for _ in 0..400 {
            let field: Vec<Sphere> = (0..5)
                .map(|_| {
                    Sphere::new(
                        v(
                            rng.range_f64(-25.0, 25.0),
                            rng.range_f64(-25.0, 25.0),
                            rng.range_f64(-25.0, 25.0),
                        ),
                        rng.range_f64(8.0, 28.0),
                    )
                })
                .collect();
            let dir = v(
                rng.next_f64_signed(),
                rng.next_f64_signed(),
                rng.next_f64_signed(),
            )
            .normalize();
            let mut p = Vec3::ZERO;
            for _ in 0..8 {
                let travel = field
                    .iter()
                    .filter_map(|s| sphere_exit_distance(p, dir, *s))
                    .fold(0.0f64, f64::max);
                if travel <= 0.0 {
                    break;
                }
                p += dir * (travel + 1e-9);
            }
            for s in &field {
                assert!(
                    p.distance(s.center) >= s.radius,
                    "still inside {s:?} at {p:?}"
                );
            }
        }
    }

    #[test]
    fn overlap_helpers_agree_with_a_zero_motion_sweep() {
        let mut rng = Rng::new(0x5EED_0C01);
        let box_ = Aabb::new(v(3.0, -2.0, 1.0), v(8.0, 4.0, 6.0));
        for _ in 0..2000 {
            let p = v(
                rng.range_f64(-20.0, 20.0),
                rng.range_f64(-20.0, 20.0),
                rng.range_f64(-20.0, 20.0),
            );
            let r = rng.range_f64(0.0, 6.0);
            let s = Sphere::new(p, r);

            let other = Sphere::new(v(1.0, 1.0, 1.0), 5.0);
            assert_eq!(
                sphere_overlaps_sphere(s, other),
                swept_sphere_sphere(p, Vec3::ZERO, r, other) == Some(0.0)
            );
            assert_eq!(
                sphere_overlaps_aabb(s, box_),
                swept_sphere_aabb(p, Vec3::ZERO, r, box_) == Some(0.0)
            );
        }
    }

    #[test]
    fn repeated_evaluation_is_bit_identical() {
        let origin = v(0.1, 0.2, 0.3);
        let motion = v(7.7, -13.3, 29.9);
        let target = Sphere::new(v(1.1, -2.2, 18.3), 6.0);
        let run = || swept_sphere_sphere(origin, motion, 0.5, target).unwrap();
        assert_eq!(run().to_bits(), run().to_bits());

        let box_ = Aabb::new(v(0.0, 0.0, 20.0), HULL_HALF);
        let run_box = || swept_sphere_aabb(origin, motion, 3.3, box_).unwrap();
        assert_eq!(run_box().to_bits(), run_box().to_bits());
    }

    // ---------------------------------------------------------------------
    // Broadphase.
    // ---------------------------------------------------------------------

    #[test]
    fn earliest_hit_wins_when_a_segment_crosses_two_overlapping_asteroids() {
        // Two rocks 15 apart with radius 10 each: they interpenetrate, and the
        // bullet's path goes through both.
        let near = Sphere::new(v(0.0, 0.0, 30.0), 10.0);
        let far = Sphere::new(v(0.0, 0.0, 45.0), 10.0);
        assert!(
            sphere_overlaps_sphere(near, far),
            "the test only means something if they overlap"
        );

        let origin = Vec3::ZERO;
        let motion = v(0.0, 0.0, 60.0);

        // Near rock first in the slice.
        let (i, t) = sweep_first_hit(origin, motion, 0.0, &[near, far]).expect("hit");
        assert_eq!(i, 0);
        assert!((t - 20.0 / 60.0).abs() < 1e-12);

        // Far rock first in the slice: the answer must not change.
        let (i, t2) = sweep_first_hit(origin, motion, 0.0, &[far, near]).expect("hit");
        assert_eq!(i, 1);
        assert_eq!(t.to_bits(), t2.to_bits());

        // Both individual sweeps agree with the pick.
        assert_eq!(swept_sphere_sphere(origin, motion, 0.0, near), Some(t));
        let t_far = swept_sphere_sphere(origin, motion, 0.0, far).unwrap();
        assert!(t < t_far);
    }

    #[test]
    fn broadphase_ties_resolve_to_the_lowest_index() {
        // Two identical rocks stacked exactly on top of each other.
        let rock = Sphere::new(v(0.0, 0.0, 30.0), 10.0);
        let (i, _) =
            sweep_first_hit(Vec3::ZERO, v(0.0, 0.0, 60.0), 0.0, &[rock, rock, rock]).expect("hit");
        assert_eq!(i, 0);
    }

    #[test]
    fn broadphase_handles_empty_and_all_miss() {
        assert_eq!(
            sweep_first_hit(Vec3::ZERO, v(0.0, 0.0, 39.0), 0.5, &[]),
            None
        );
        let away = [
            Sphere::new(v(500.0, 0.0, 0.0), 6.0),
            Sphere::new(v(0.0, 0.0, -500.0), 6.0),
        ];
        assert_eq!(
            sweep_first_hit(Vec3::ZERO, v(0.0, 0.0, 39.0), 0.5, &away),
            None
        );
    }

    #[test]
    fn broadphase_matches_a_linear_scan_over_a_full_asteroid_field() {
        // 60 rocks, the field size `createAsteroidField` defaults to, and a
        // bullet crossing it at 780 u/s. The culled scan must return exactly
        // what an unculled one would.
        let mut rng = Rng::new(0xA57E_01D5);
        let mut field = Vec::new();
        for _ in 0..60 {
            field.push(Sphere::new(
                v(
                    rng.range_f64(-400.0, 400.0),
                    rng.range_f64(-160.0, 160.0),
                    rng.range_f64(-400.0, 400.0),
                ),
                rng.range_f64(5.0, 30.0) * 0.95,
            ));
        }

        let step = BULLET_SPEED * FRAME_DT;
        let mut checked_a_hit = false;
        for _ in 0..500 {
            let origin = v(
                rng.range_f64(-400.0, 400.0),
                rng.range_f64(-160.0, 160.0),
                rng.range_f64(-400.0, 400.0),
            );
            let dir = v(
                rng.next_f64_signed(),
                rng.next_f64_signed(),
                rng.next_f64_signed(),
            )
            .normalize();
            let motion = dir * step;

            let mut expect: Option<(usize, f64)> = None;
            for (i, s) in field.iter().enumerate() {
                if let Some(t) = swept_sphere_sphere(origin, motion, BULLET_RADIUS, *s) {
                    match expect {
                        Some((_, bt)) if t >= bt => {}
                        _ => expect = Some((i, t)),
                    }
                }
            }
            let got = sweep_first_hit(origin, motion, BULLET_RADIUS, &field);
            assert_eq!(expect, got);
            checked_a_hit |= got.is_some();
        }
        assert!(checked_a_hit, "the field was never actually hit");
    }

    // ---------------------------------------------------------------------
    // Sphere vs. box.
    // ---------------------------------------------------------------------

    #[test]
    fn ship_sweeps_into_a_hull_face() {
        // Ship radius 3.3 (`main.js`: 2.2 * SHIP_SCALE) approaching the +x face
        // of a hull from 100 units out.
        let box_ = Aabb::new(Vec3::ZERO, HULL_HALF);
        let radius = 3.3;
        let origin = v(100.0, 0.0, 0.0);
        let motion = v(-100.0, 0.0, 0.0);
        let t = swept_sphere_aabb(origin, motion, radius, box_).expect("hit");
        // Contact when the center reaches x = 45 + 3.3.
        let expected = (100.0 - 48.3) / 100.0;
        assert!((t - expected).abs() < 1e-12, "t = {t}");
        let contact = origin + motion * t;
        assert!((contact.x - 48.3).abs() < 1e-9);
    }

    #[test]
    fn a_sphere_starting_inside_the_box_reports_impact_immediately() {
        let box_ = Aabb::new(Vec3::ZERO, HULL_HALF);
        assert_eq!(
            swept_sphere_aabb(v(1.0, 2.0, 3.0), v(50.0, 0.0, 0.0), 3.3, box_),
            Some(0.0)
        );
        // Just outside the surface but within the radius: also immediate.
        assert_eq!(
            swept_sphere_aabb(v(47.0, 0.0, 0.0), v(0.0, 0.0, 1.0), 3.3, box_),
            Some(0.0)
        );
    }

    #[test]
    fn a_sphere_that_only_clips_the_expanded_box_corner_does_not_hit() {
        // Unit box, unit sphere. The expanded box is [-2, 2]^3, but the real
        // swept volume has a quarter-cylinder of radius 1 around the vertical
        // edge at (1, 1), not a square corner. The segment below runs along
        // x + y = 3.6 in the z = 0 plane: it passes through the expanded box's
        // corner region while staying 1.13 units from the edge, so it must
        // miss.
        let box_ = Aabb::new(Vec3::ZERO, Vec3::ONE);
        let origin = v(3.6, 0.0, 0.0);
        let motion = v(-3.6, 3.6, 0.0);

        // The cheap conservative test really would claim a hit here.
        assert!(
            ray_aabb_entry(origin, motion, box_.expanded(1.0)).is_some(),
            "test is only meaningful if the expanded box is entered"
        );
        // Closest approach to the rounded edge, for the record: 1.6/sqrt(2).
        assert!((1.6 / 2.0f64.sqrt() - 1.1313708498984762).abs() < 1e-12);

        assert_eq!(swept_sphere_aabb(origin, motion, 1.0, box_), None);
    }

    #[test]
    fn a_sphere_that_grazes_a_box_edge_hits_the_rounded_edge() {
        // Same geometry, aimed 1.0 unit from the edge at (1, 1, z): tangent to
        // the rounded edge, so it is a hit — at the tangency, not at the
        // expanded box's corner.
        let box_ = Aabb::new(Vec3::ZERO, Vec3::ONE);
        let origin = v(4.0, 2.0, 0.0);
        let motion = v(-4.0, 0.0, 0.0);
        let t = swept_sphere_aabb(origin, motion, 1.0, box_).expect("hit");
        assert!((t - 0.75).abs() < 1e-12, "t = {t}");
        let contact = origin + motion * t;
        assert!(contact.abs_diff_eq(v(1.0, 2.0, 0.0), 1e-12));
        // The contact point is exactly one radius from the edge.
        assert!((contact.distance(v(1.0, 1.0, 0.0)) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_sphere_sweeping_onto_a_box_corner_hits_the_rounded_corner() {
        // Straight down the (1,1,1) diagonal at the corner.
        let box_ = Aabb::new(Vec3::ZERO, Vec3::ONE);
        let d = 1.0 / 3.0f64.sqrt();
        let start = 10.0;
        let origin = v(start * d, start * d, start * d);
        let motion = -origin;
        let t = swept_sphere_aabb(origin, motion, 1.0, box_).expect("hit");
        let contact = origin + motion * t;
        // Contact when the center is exactly one radius from the corner.
        assert!(
            (contact.distance(Vec3::ONE) - 1.0).abs() < 1e-9,
            "distance {}",
            contact.distance(Vec3::ONE)
        );
    }

    #[test]
    fn box_sweep_ignores_a_box_that_is_out_of_reach() {
        let box_ = Aabb::new(Vec3::ZERO, HULL_HALF);
        // Travelling parallel to the box, well clear of it.
        assert_eq!(
            swept_sphere_aabb(v(0.0, 100.0, -500.0), v(0.0, 0.0, 1000.0), 3.3, box_),
            None
        );
        // Heading for it but running out of step.
        assert_eq!(
            swept_sphere_aabb(v(200.0, 0.0, 0.0), v(-10.0, 0.0, 0.0), 3.3, box_),
            None
        );
    }

    #[test]
    fn overlaps_aabb_rejects_spawn_positions_the_js_would_accept() {
        // `clipsAvoidance` in asteroids.js compares per-axis distances, which
        // carves out a box, not a rounded box. The corner-adjacent position
        // below is 20 units clear of the hull in reality.
        let box_ = Aabb::new(Vec3::ZERO, HULL_HALF);
        let corner_adjacent = v(45.0 + 12.0, 18.0 + 12.0, 35.0 + 12.0);
        assert!(!sphere_overlaps_aabb(
            Sphere::new(corner_adjacent, 8.0),
            box_
        ));
        // Straight out from a face at the same clearance, it does overlap.
        assert!(sphere_overlaps_aabb(
            Sphere::new(v(45.0 + 6.0, 0.0, 0.0), 8.0),
            box_
        ));
        // Fully inside counts.
        assert!(sphere_overlaps_aabb(Sphere::new(Vec3::ZERO, 1.0), box_));
    }

    #[test]
    fn aabb_helpers_are_consistent() {
        let box_ = Aabb::new(v(1.0, 2.0, 3.0), v(4.0, 5.0, 6.0));
        assert_eq!(box_.min(), v(-3.0, -3.0, -3.0));
        assert_eq!(box_.max(), v(5.0, 7.0, 9.0));
        assert_eq!(Aabb::from_min_max(box_.min(), box_.max()), box_);
        assert!(box_.contains_point(box_.center));
        assert!(box_.contains_point(box_.max()));
        assert!(!box_.contains_point(box_.max() + v(1e-9, 0.0, 0.0)));
        assert_eq!(box_.closest_point(v(100.0, 0.0, 0.0)), v(5.0, 0.0, 0.0));
        assert_eq!(box_.closest_point(box_.center), box_.center);
        assert_eq!(box_.expanded(1.0).half_extents, v(5.0, 6.0, 7.0));
    }

    // ---------------------------------------------------------------------
    // Cross-checks against brute force.
    // ---------------------------------------------------------------------

    /// First sampled `t` at which the moving sphere overlaps `target`.
    fn sampled_first_hit(
        origin: Vec3,
        motion: Vec3,
        radius: f64,
        target: Sphere,
        samples: u32,
    ) -> Option<f64> {
        for i in 0..=samples {
            let t = f64::from(i) / f64::from(samples);
            if sphere_overlaps_sphere(Sphere::new(origin + motion * t, radius), target) {
                return Some(t);
            }
        }
        None
    }

    /// A displacement that lands near `target` from `origin` more often than a
    /// blind random vector would, so that the cross-checks below spend their
    /// samples on the interesting cases (near-misses and grazes) instead of on
    /// empty space.
    fn aimed_motion(rng: &mut Rng, origin: Vec3, target: Sphere, spread: f64) -> Vec3 {
        let jitter = v(
            rng.next_f64_signed(),
            rng.next_f64_signed(),
            rng.next_f64_signed(),
        ) * spread;
        (target.center - origin + jitter) * rng.range_f64(0.2, 1.8)
    }

    #[test]
    fn swept_sphere_sphere_matches_dense_sampling() {
        let mut rng = Rng::new(0xBEEF_1234);
        let samples = 4096;
        let mut hits = 0;
        for _ in 0..3000 {
            let origin = v(
                rng.range_f64(-40.0, 40.0),
                rng.range_f64(-40.0, 40.0),
                rng.range_f64(-40.0, 40.0),
            );
            let radius = rng.range_f64(0.0, 7.0);
            let target = Sphere::new(
                v(
                    rng.range_f64(-40.0, 40.0),
                    rng.range_f64(-40.0, 40.0),
                    rng.range_f64(-40.0, 40.0),
                ),
                rng.range_f64(1.0, 12.0),
            );
            let motion = aimed_motion(&mut rng, origin, target, radius + target.radius);

            let got = swept_sphere_sphere(origin, motion, radius, target);
            let sampled = sampled_first_hit(origin, motion, radius, target, samples);

            match (got, sampled) {
                (Some(t), _) => {
                    hits += 1;
                    assert!((0.0..=1.0).contains(&t), "t out of range: {t}");
                    // Soundness: the bodies really are touching at `t`.
                    let gap =
                        (origin + motion * t).distance(target.center) - (radius + target.radius);
                    assert!(gap <= 1e-9, "reported a hit with a gap of {gap}");
                    // Completeness: never later than a sampled overlap.
                    if let Some(ts) = sampled {
                        assert!(t <= ts + 1e-12, "t {t} later than sampled {ts}");
                    }
                }
                (None, Some(ts)) => {
                    panic!("missed a contact that sampling found at t = {ts}");
                }
                (None, None) => {}
            }
        }
        assert!(hits > 500, "only {hits} hits — the test data is too sparse");
    }

    /// First sampled `t` at which the moving sphere overlaps `target`.
    fn sampled_first_box_hit(
        origin: Vec3,
        motion: Vec3,
        radius: f64,
        target: Aabb,
        samples: u32,
    ) -> Option<f64> {
        for i in 0..=samples {
            let t = f64::from(i) / f64::from(samples);
            if sphere_overlaps_aabb(Sphere::new(origin + motion * t, radius), target) {
                return Some(t);
            }
        }
        None
    }

    #[test]
    fn swept_sphere_aabb_matches_dense_sampling() {
        let mut rng = Rng::new(0xC0DE_4321);
        let samples = 4096;
        let mut hits = 0;
        let mut corner_cases = 0;
        for _ in 0..3000 {
            let origin = v(
                rng.range_f64(-30.0, 30.0),
                rng.range_f64(-30.0, 30.0),
                rng.range_f64(-30.0, 30.0),
            );
            let radius = rng.range_f64(0.0, 6.0);
            let target = Aabb::new(
                v(
                    rng.range_f64(-15.0, 15.0),
                    rng.range_f64(-15.0, 15.0),
                    rng.range_f64(-15.0, 15.0),
                ),
                v(
                    rng.range_f64(1.0, 14.0),
                    rng.range_f64(1.0, 14.0),
                    rng.range_f64(1.0, 14.0),
                ),
            );
            // Aimed at the box, with enough jitter to graze it and to miss:
            // the corner and edge paths only get exercised by sweeps that
            // arrive off-centre.
            let motion = aimed_motion(
                &mut rng,
                origin,
                Sphere::new(target.center, 0.0),
                target.half_extents.length() + radius,
            );

            let got = swept_sphere_aabb(origin, motion, radius, target);
            let sampled = sampled_first_box_hit(origin, motion, radius, target, samples);

            match (got, sampled) {
                (Some(t), _) => {
                    hits += 1;
                    assert!((0.0..=1.0).contains(&t), "t out of range: {t}");
                    // Soundness: the sphere really is touching the box at `t`.
                    let p = origin + motion * t;
                    let gap = p.distance(target.closest_point(p)) - radius;
                    assert!(gap <= 1e-9, "reported a hit with a gap of {gap}");
                    if let Some(ts) = sampled {
                        assert!(t <= ts + 1e-12, "t {t} later than sampled {ts}");
                    } else {
                        // A contact too brief for the sampler to see: a graze.
                        corner_cases += 1;
                    }
                }
                (None, Some(ts)) => {
                    panic!("missed a contact that sampling found at t = {ts}");
                }
                (None, None) => {}
            }
        }
        assert!(hits > 500, "only {hits} hits — the test data is too sparse");
        // Not an assertion about correctness, just a note that grazes are rare.
        assert!(corner_cases < hits);
    }

    #[test]
    fn moving_vs_moving_matches_the_frame_it_describes() {
        // Cross-check the relative-space solve against sampling both bodies
        // along their own paths.
        let mut rng = Rng::new(0x0FED_CBA9);
        let samples = 4096;
        let mut hits = 0;
        for _ in 0..2000 {
            let origin = v(
                rng.range_f64(-40.0, 40.0),
                rng.range_f64(-40.0, 40.0),
                rng.range_f64(-40.0, 40.0),
            );
            let radius = rng.range_f64(0.5, 7.0);
            let target = Sphere::new(
                v(
                    rng.range_f64(-40.0, 40.0),
                    rng.range_f64(-40.0, 40.0),
                    rng.range_f64(-40.0, 40.0),
                ),
                rng.range_f64(1.0, 12.0),
            );
            let target_motion = v(
                rng.range_f64(-45.0, 45.0),
                rng.range_f64(-45.0, 45.0),
                rng.range_f64(-45.0, 45.0),
            );
            // Aim the *relative* motion, then add the target's own drift back
            // in, so both bodies genuinely move and interceptions still happen.
            let motion =
                target_motion + aimed_motion(&mut rng, origin, target, radius + target.radius);

            let got = swept_sphere_vs_moving_sphere(origin, motion, radius, target, target_motion);

            let mut sampled = None;
            for i in 0..=samples {
                let t = f64::from(i) / f64::from(samples);
                let a = Sphere::new(origin + motion * t, radius);
                let b = Sphere::new(target.center + target_motion * t, target.radius);
                if sphere_overlaps_sphere(a, b) {
                    sampled = Some(t);
                    break;
                }
            }

            match (got, sampled) {
                (Some(t), _) => {
                    hits += 1;
                    let a = origin + motion * t;
                    let b = target.center + target_motion * t;
                    let gap = a.distance(b) - (radius + target.radius);
                    assert!(gap <= 1e-9, "reported a hit with a gap of {gap}");
                    if let Some(ts) = sampled {
                        assert!(t <= ts + 1e-12, "t {t} later than sampled {ts}");
                    }
                }
                (None, Some(ts)) => panic!("missed a contact sampling found at t = {ts}"),
                (None, None) => {}
            }
        }
        assert!(hits > 500, "only {hits} hits — the test data is too sparse");
    }

    // ---------------------------------------------------------------------
    // Solver internals.
    // ---------------------------------------------------------------------

    #[test]
    fn quadratic_interval_returns_sorted_roots() {
        // t^2 - 4t + 3 = 0 has roots 1 and 3, written with the halved linear
        // term the helper expects.
        let (lo, hi) = quadratic_interval(1.0, -2.0, 3.0).unwrap();
        assert!((lo - 1.0).abs() < 1e-12);
        assert!((hi - 3.0).abs() < 1e-12);
        // Double root.
        assert_eq!(quadratic_interval(1.0, -1.0, 1.0), Some((1.0, 1.0)));
        // No real root.
        assert_eq!(quadratic_interval(1.0, 0.0, 1.0), None);
        // The degenerate double-root-at-zero branch.
        assert_eq!(quadratic_interval(2.0, 0.0, 0.0), Some((0.0, 0.0)));
        // Roots straddling zero: c < 0 means the origin is inside.
        let (lo, hi) = quadratic_interval(1.0, 0.0, -4.0).unwrap();
        assert!((lo + 2.0).abs() < 1e-12);
        assert!((hi - 2.0).abs() < 1e-12);
    }

    #[test]
    fn ray_aabb_entry_handles_axis_aligned_and_stationary_axes() {
        let box_ = Aabb::new(Vec3::ZERO, Vec3::ONE);
        // Straight in along -x.
        let t = ray_aabb_entry(v(4.0, 0.0, 0.0), v(-8.0, 0.0, 0.0), box_).unwrap();
        assert!((t - 3.0 / 8.0).abs() < 1e-12);
        // Starting inside.
        assert_eq!(
            ray_aabb_entry(Vec3::ZERO, v(1.0, 1.0, 1.0), box_),
            Some(0.0)
        );
        // Stationary on an axis that is outside the slab: never enters.
        assert_eq!(
            ray_aabb_entry(v(4.0, 9.0, 0.0), v(-8.0, 0.0, 0.0), box_),
            None
        );
        // Falls short.
        assert_eq!(
            ray_aabb_entry(v(4.0, 0.0, 0.0), v(-1.0, 0.0, 0.0), box_),
            None
        );
    }
}
