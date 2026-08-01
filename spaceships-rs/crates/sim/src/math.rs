//! Vector math for the simulation.
//!
//! [`Vec3`] is the replacement for `THREE.Vector3`, which the JS game uses for
//! essentially every quantity that has a direction: positions, velocities,
//! muzzle offsets, surface normals, collision pushes, camera rigs. Ports of
//! `main.js`, `bullets.js`, `missiles.js`, and `bot.js` all bottom out here.
//!
//! # Why `f64`
//!
//! The wire format carries JSON numbers, which are IEEE-754 doubles, and the JS
//! simulation runs entirely in doubles. Using `f64` here means a value can make
//! the round trip browser -> server -> browser without a precision change, so a
//! Rust server and the existing JS client cannot drift apart from rounding
//! alone.
//!
//! # Determinism
//!
//! Every operation in this module is built from `+`, `-`, `*`, `/`, and `sqrt`,
//! all of which IEEE-754 requires to be correctly rounded and therefore
//! bit-identical on every conforming platform. No transcendental functions are
//! used, deliberately: `sin`/`cos`/`powf` are *not* guaranteed identical across
//! platforms or libm versions, and would break server/WASM agreement.

use std::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

/// A 3-component vector of `f64`, in the game's right-handed `[x, y, z]` space
/// (`+y` up, ships fly along their local `+z`).
///
/// `#[repr(C)]` so the layout matches the `[f64; 3]` the protocol puts on the
/// wire, making the [`Vec3::to_array`] / [`Vec3::from_array`] conversions free.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct Vec3 {
    /// Right.
    pub x: f64,
    /// Up.
    pub y: f64,
    /// Forward.
    pub z: f64,
}

impl Vec3 {
    /// The zero vector.
    pub const ZERO: Vec3 = Vec3::new(0.0, 0.0, 0.0);
    /// `(1, 1, 1)`.
    pub const ONE: Vec3 = Vec3::new(1.0, 1.0, 1.0);
    /// Unit vector along `+x` (right).
    pub const X: Vec3 = Vec3::new(1.0, 0.0, 0.0);
    /// Unit vector along `+y` (up).
    pub const Y: Vec3 = Vec3::new(0.0, 1.0, 0.0);
    /// Unit vector along `+z` (forward — the direction ships fire).
    pub const Z: Vec3 = Vec3::new(0.0, 0.0, 1.0);

    /// Builds a vector from components.
    #[inline]
    #[must_use]
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Vec3 { x, y, z }
    }

    /// Builds a vector with all three components set to `v`.
    #[inline]
    #[must_use]
    pub const fn splat(v: f64) -> Self {
        Vec3::new(v, v, v)
    }

    /// Converts from the `[x, y, z]` array form used on the wire.
    #[inline]
    #[must_use]
    pub const fn from_array(a: [f64; 3]) -> Self {
        Vec3::new(a[0], a[1], a[2])
    }

    /// Converts to the `[x, y, z]` array form used on the wire.
    #[inline]
    #[must_use]
    pub const fn to_array(self) -> [f64; 3] {
        [self.x, self.y, self.z]
    }

    /// Dot product.
    #[inline]
    #[must_use]
    pub fn dot(self, rhs: Vec3) -> f64 {
        self.x * rhs.x + self.y * rhs.y + self.z * rhs.z
    }

    /// Cross product, right-handed: `X.cross(Y) == Z`.
    #[inline]
    #[must_use]
    pub fn cross(self, rhs: Vec3) -> Vec3 {
        Vec3::new(
            self.y * rhs.z - self.z * rhs.y,
            self.z * rhs.x - self.x * rhs.z,
            self.x * rhs.y - self.y * rhs.x,
        )
    }

    /// Squared magnitude. Prefer this over [`Vec3::length`] for comparisons: it
    /// skips the `sqrt` and stays exact.
    #[inline]
    #[must_use]
    pub fn length_squared(self) -> f64 {
        self.dot(self)
    }

    /// Magnitude.
    #[inline]
    #[must_use]
    pub fn length(self) -> f64 {
        self.length_squared().sqrt()
    }

    /// Squared distance to `rhs`. Prefer this over [`Vec3::distance`] for
    /// radius checks (`d2 < r * r`), which is what the collision code wants.
    #[inline]
    #[must_use]
    pub fn distance_squared(self, rhs: Vec3) -> f64 {
        (self - rhs).length_squared()
    }

    /// Distance to `rhs`.
    #[inline]
    #[must_use]
    pub fn distance(self, rhs: Vec3) -> f64 {
        (self - rhs).length()
    }

    /// Returns a unit vector in the same direction.
    ///
    /// A zero-length vector is returned unchanged (as zero) rather than
    /// producing `NaN`. This matches `THREE.Vector3.normalize`, which divides by
    /// `length() || 1`, so ported JS keeps its exact behaviour. Use
    /// [`Vec3::try_normalize`] when a degenerate direction is a bug you want to
    /// see.
    #[inline]
    #[must_use]
    pub fn normalize(self) -> Vec3 {
        let len = self.length();
        if len == 0.0 {
            self
        } else {
            self / len
        }
    }

    /// Returns a unit vector in the same direction, or `None` if the vector has
    /// zero (or non-finite) length.
    #[inline]
    #[must_use]
    pub fn try_normalize(self) -> Option<Vec3> {
        let len = self.length();
        if len > 0.0 && len.is_finite() {
            Some(self / len)
        } else {
            None
        }
    }

    /// True if this vector is already unit length, within `tol`.
    #[inline]
    #[must_use]
    pub fn is_normalized(self, tol: f64) -> bool {
        (self.length_squared() - 1.0).abs() <= tol
    }

    /// `self` scaled by `s`. Spelled out because "scale" reads better than `*`
    /// at call sites ported from `multiplyScalar`.
    #[inline]
    #[must_use]
    pub fn scale(self, s: f64) -> Vec3 {
        self * s
    }

    /// `self + rhs * s`, the `addScaledVector` that shows up all over the JS
    /// (muzzle offsets, projectile advance, collision pushes).
    #[inline]
    #[must_use]
    pub fn add_scaled(self, rhs: Vec3, s: f64) -> Vec3 {
        Vec3::new(self.x + rhs.x * s, self.y + rhs.y * s, self.z + rhs.z * s)
    }

    /// Linear interpolation; `t == 0` yields `self`, `t == 1` yields `rhs`.
    #[inline]
    #[must_use]
    pub fn lerp(self, rhs: Vec3, t: f64) -> Vec3 {
        self + (rhs - self) * t
    }

    /// Component-wise minimum.
    #[inline]
    #[must_use]
    pub fn min(self, rhs: Vec3) -> Vec3 {
        Vec3::new(self.x.min(rhs.x), self.y.min(rhs.y), self.z.min(rhs.z))
    }

    /// Component-wise maximum.
    #[inline]
    #[must_use]
    pub fn max(self, rhs: Vec3) -> Vec3 {
        Vec3::new(self.x.max(rhs.x), self.y.max(rhs.y), self.z.max(rhs.z))
    }

    /// Component-wise clamp between `lo` and `hi`.
    #[inline]
    #[must_use]
    pub fn clamp(self, lo: Vec3, hi: Vec3) -> Vec3 {
        self.max(lo).min(hi)
    }

    /// Returns `self` shortened to at most `max_len`, leaving shorter vectors
    /// untouched. Used for speed caps.
    #[inline]
    #[must_use]
    pub fn clamp_length(self, max_len: f64) -> Vec3 {
        let len_sq = self.length_squared();
        if len_sq > max_len * max_len && len_sq > 0.0 {
            self * (max_len / len_sq.sqrt())
        } else {
            self
        }
    }

    /// Projection of `self` onto `onto`. Returns [`Vec3::ZERO`] if `onto` is
    /// degenerate.
    #[inline]
    #[must_use]
    pub fn project_onto(self, onto: Vec3) -> Vec3 {
        let len_sq = onto.length_squared();
        if len_sq == 0.0 {
            Vec3::ZERO
        } else {
            onto * (self.dot(onto) / len_sq)
        }
    }

    /// Reflects `self` about the plane with unit normal `normal`.
    #[inline]
    #[must_use]
    pub fn reflect(self, normal: Vec3) -> Vec3 {
        self - normal * (2.0 * self.dot(normal))
    }

    /// True if every component is finite (no `NaN`, no infinity). Worth
    /// asserting at simulation boundaries: one `NaN` position propagates through
    /// the whole world within a tick.
    #[inline]
    #[must_use]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }

    /// True if every component of `self` is within `tol` of `rhs`. For tests and
    /// assertions — never branch simulation logic on this.
    #[inline]
    #[must_use]
    pub fn abs_diff_eq(self, rhs: Vec3, tol: f64) -> bool {
        (self.x - rhs.x).abs() <= tol
            && (self.y - rhs.y).abs() <= tol
            && (self.z - rhs.z).abs() <= tol
    }
}

impl From<[f64; 3]> for Vec3 {
    #[inline]
    fn from(a: [f64; 3]) -> Self {
        Vec3::from_array(a)
    }
}

impl From<Vec3> for [f64; 3] {
    #[inline]
    fn from(v: Vec3) -> Self {
        v.to_array()
    }
}

impl Add for Vec3 {
    type Output = Vec3;
    #[inline]
    fn add(self, rhs: Vec3) -> Vec3 {
        Vec3::new(self.x + rhs.x, self.y + rhs.y, self.z + rhs.z)
    }
}

impl Sub for Vec3 {
    type Output = Vec3;
    #[inline]
    fn sub(self, rhs: Vec3) -> Vec3 {
        Vec3::new(self.x - rhs.x, self.y - rhs.y, self.z - rhs.z)
    }
}

impl Neg for Vec3 {
    type Output = Vec3;
    #[inline]
    fn neg(self) -> Vec3 {
        Vec3::new(-self.x, -self.y, -self.z)
    }
}

impl Mul<f64> for Vec3 {
    type Output = Vec3;
    #[inline]
    fn mul(self, s: f64) -> Vec3 {
        Vec3::new(self.x * s, self.y * s, self.z * s)
    }
}

impl Mul<Vec3> for f64 {
    type Output = Vec3;
    #[inline]
    fn mul(self, v: Vec3) -> Vec3 {
        v * self
    }
}

impl Div<f64> for Vec3 {
    type Output = Vec3;
    #[inline]
    fn div(self, s: f64) -> Vec3 {
        Vec3::new(self.x / s, self.y / s, self.z / s)
    }
}

impl AddAssign for Vec3 {
    #[inline]
    fn add_assign(&mut self, rhs: Vec3) {
        *self = *self + rhs;
    }
}

impl SubAssign for Vec3 {
    #[inline]
    fn sub_assign(&mut self, rhs: Vec3) {
        *self = *self - rhs;
    }
}

impl MulAssign<f64> for Vec3 {
    #[inline]
    fn mul_assign(&mut self, s: f64) {
        *self = *self * s;
    }
}

impl DivAssign<f64> for Vec3 {
    #[inline]
    fn div_assign(&mut self, s: f64) {
        *self = *self / s;
    }
}

#[cfg(test)]
mod tests {
    use super::Vec3;

    const EPS: f64 = 1e-12;

    fn v(x: f64, y: f64, z: f64) -> Vec3 {
        Vec3::new(x, y, z)
    }

    #[test]
    fn constructors_and_constants() {
        assert_eq!(Vec3::ZERO, v(0.0, 0.0, 0.0));
        assert_eq!(Vec3::ONE, v(1.0, 1.0, 1.0));
        assert_eq!(Vec3::X, v(1.0, 0.0, 0.0));
        assert_eq!(Vec3::Y, v(0.0, 1.0, 0.0));
        assert_eq!(Vec3::Z, v(0.0, 0.0, 1.0));
        assert_eq!(Vec3::splat(2.5), v(2.5, 2.5, 2.5));
        assert_eq!(Vec3::default(), Vec3::ZERO);
    }

    #[test]
    fn array_round_trip_matches_wire_order() {
        let a = [1.5, -2.5, 3.25];
        let vec = Vec3::from_array(a);
        assert_eq!(vec.x, 1.5);
        assert_eq!(vec.y, -2.5);
        assert_eq!(vec.z, 3.25);
        assert_eq!(vec.to_array(), a);
        assert_eq!(Vec3::from(a), vec);
        assert_eq!(<[f64; 3]>::from(vec), a);
    }

    #[test]
    fn add_and_sub() {
        assert_eq!(v(1.0, 2.0, 3.0) + v(4.0, -5.0, 6.0), v(5.0, -3.0, 9.0));
        assert_eq!(v(1.0, 2.0, 3.0) - v(4.0, -5.0, 6.0), v(-3.0, 7.0, -3.0));
        assert_eq!(-v(1.0, -2.0, 3.0), v(-1.0, 2.0, -3.0));
    }

    #[test]
    fn add_sub_identity_and_inverse() {
        let a = v(3.0, -7.5, 0.25);
        assert_eq!(a + Vec3::ZERO, a);
        assert_eq!(a - a, Vec3::ZERO);
        assert_eq!(a + -a, Vec3::ZERO);
    }

    #[test]
    fn scale_and_divide() {
        let a = v(1.0, -2.0, 4.0);
        assert_eq!(a.scale(2.5), v(2.5, -5.0, 10.0));
        assert_eq!(a * 2.5, v(2.5, -5.0, 10.0));
        assert_eq!(2.5 * a, v(2.5, -5.0, 10.0));
        assert_eq!(a / 2.0, v(0.5, -1.0, 2.0));
        assert_eq!(a * 0.0, Vec3::ZERO);
        assert_eq!(a * 1.0, a);
    }

    #[test]
    fn assign_ops() {
        let mut a = v(1.0, 2.0, 3.0);
        a += v(1.0, 1.0, 1.0);
        assert_eq!(a, v(2.0, 3.0, 4.0));
        a -= v(0.5, 0.5, 0.5);
        assert_eq!(a, v(1.5, 2.5, 3.5));
        a *= 2.0;
        assert_eq!(a, v(3.0, 5.0, 7.0));
        a /= 2.0;
        assert_eq!(a, v(1.5, 2.5, 3.5));
    }

    #[test]
    fn dot_product() {
        assert_eq!(v(1.0, 2.0, 3.0).dot(v(4.0, -5.0, 6.0)), 4.0 - 10.0 + 18.0);
        // Orthogonal basis vectors have zero dot product.
        assert_eq!(Vec3::X.dot(Vec3::Y), 0.0);
        assert_eq!(Vec3::Y.dot(Vec3::Z), 0.0);
        assert_eq!(Vec3::Z.dot(Vec3::X), 0.0);
        // Unit vectors dot to 1 with themselves.
        assert_eq!(Vec3::X.dot(Vec3::X), 1.0);
        // Commutative.
        let (a, b) = (v(1.5, -0.5, 2.0), v(-3.0, 4.0, 0.25));
        assert_eq!(a.dot(b), b.dot(a));
        // Antiparallel vectors dot negative.
        assert!(Vec3::Z.dot(-Vec3::Z) < 0.0);
    }

    #[test]
    fn cross_product_is_right_handed() {
        assert_eq!(Vec3::X.cross(Vec3::Y), Vec3::Z);
        assert_eq!(Vec3::Y.cross(Vec3::Z), Vec3::X);
        assert_eq!(Vec3::Z.cross(Vec3::X), Vec3::Y);
    }

    #[test]
    fn cross_product_is_anticommutative_and_orthogonal() {
        let a = v(1.0, 2.0, 3.0);
        let b = v(-4.0, 5.0, 0.5);
        assert_eq!(a.cross(b), -b.cross(a));
        // Result is perpendicular to both inputs.
        let c = a.cross(b);
        assert!(c.dot(a).abs() < 1e-9);
        assert!(c.dot(b).abs() < 1e-9);
        // Parallel vectors cross to zero.
        assert_eq!(a.cross(a), Vec3::ZERO);
        assert_eq!(a.cross(a * 3.0), Vec3::ZERO);
    }

    #[test]
    fn length_and_length_squared() {
        let a = v(3.0, 4.0, 0.0);
        assert_eq!(a.length_squared(), 25.0);
        assert_eq!(a.length(), 5.0);
        // Classic 1-2-2 triple: length exactly 3.
        assert_eq!(v(1.0, 2.0, 2.0).length(), 3.0);
        assert_eq!(Vec3::ZERO.length(), 0.0);
        assert_eq!(Vec3::X.length(), 1.0);
        // Scaling scales the length.
        assert!((a.scale(2.0).length() - 10.0).abs() < EPS);
    }

    #[test]
    fn distance_and_distance_squared() {
        let a = v(1.0, 1.0, 1.0);
        let b = v(4.0, 5.0, 1.0);
        assert_eq!(a.distance_squared(b), 25.0);
        assert_eq!(a.distance(b), 5.0);
        // Symmetric, and zero to itself.
        assert_eq!(b.distance(a), 5.0);
        assert_eq!(a.distance(a), 0.0);
        // Consistent with length of the difference.
        assert_eq!(a.distance(b), (a - b).length());
    }

    #[test]
    fn normalize_produces_unit_length() {
        let a = v(3.0, 4.0, 0.0);
        let n = a.normalize();
        assert!(n.abs_diff_eq(v(0.6, 0.8, 0.0), EPS));
        assert!((n.length() - 1.0).abs() < EPS);
        assert!(n.is_normalized(1e-9));
        // Direction is preserved: the normalized vector is parallel to the input.
        assert!(a.cross(n).abs_diff_eq(Vec3::ZERO, 1e-9));
    }

    #[test]
    fn normalize_of_zero_stays_zero_like_three_js() {
        // THREE.Vector3.normalize divides by `length() || 1`, so a zero vector
        // comes back as zero rather than NaN. Ported JS depends on that.
        let n = Vec3::ZERO.normalize();
        assert_eq!(n, Vec3::ZERO);
        assert!(n.is_finite());
    }

    #[test]
    fn try_normalize_rejects_degenerate_input() {
        assert!(Vec3::ZERO.try_normalize().is_none());
        assert!(v(f64::NAN, 0.0, 0.0).try_normalize().is_none());
        assert!(v(f64::INFINITY, 0.0, 0.0).try_normalize().is_none());
        let n = v(0.0, -2.0, 0.0).try_normalize().unwrap();
        assert_eq!(n, v(0.0, -1.0, 0.0));
    }

    #[test]
    fn add_scaled_matches_manual_expansion() {
        let origin = v(1.0, 2.0, 3.0);
        let dir = Vec3::Z;
        // main.js: ship.position.clone().addScaledVector(fwd, 6)
        assert_eq!(origin.add_scaled(dir, 6.0), v(1.0, 2.0, 9.0));
        assert_eq!(origin.add_scaled(dir, 0.0), origin);
        assert_eq!(origin.add_scaled(dir, -3.0), origin - dir * 3.0);
    }

    #[test]
    fn lerp_endpoints_and_midpoint() {
        let a = v(0.0, 0.0, 0.0);
        let b = v(10.0, -20.0, 4.0);
        assert_eq!(a.lerp(b, 0.0), a);
        assert_eq!(a.lerp(b, 1.0), b);
        assert!(a.lerp(b, 0.5).abs_diff_eq(v(5.0, -10.0, 2.0), EPS));
    }

    #[test]
    fn min_max_clamp_are_component_wise() {
        let a = v(1.0, 5.0, -3.0);
        let b = v(4.0, 2.0, 0.0);
        assert_eq!(a.min(b), v(1.0, 2.0, -3.0));
        assert_eq!(a.max(b), v(4.0, 5.0, 0.0));
        assert_eq!(
            v(10.0, -10.0, 0.5).clamp(Vec3::splat(-1.0), Vec3::splat(1.0)),
            v(1.0, -1.0, 0.5)
        );
    }

    #[test]
    fn clamp_length_caps_only_when_over() {
        let fast = Vec3::Z * 100.0;
        assert!(fast.clamp_length(80.0).abs_diff_eq(Vec3::Z * 80.0, 1e-9));
        let slow = Vec3::Z * 10.0;
        assert_eq!(slow.clamp_length(80.0), slow);
        assert_eq!(Vec3::ZERO.clamp_length(80.0), Vec3::ZERO);
    }

    #[test]
    fn project_onto_axis() {
        let a = v(3.0, 4.0, 5.0);
        assert!(a.project_onto(Vec3::Y).abs_diff_eq(v(0.0, 4.0, 0.0), EPS));
        // Projecting onto a longer parallel vector gives the same answer.
        assert!(a
            .project_onto(Vec3::Y * 7.0)
            .abs_diff_eq(v(0.0, 4.0, 0.0), EPS));
        assert_eq!(a.project_onto(Vec3::ZERO), Vec3::ZERO);
    }

    #[test]
    fn reflect_about_plane_normal() {
        // Straight down onto a floor bounces straight up.
        let incoming = v(0.0, -1.0, 0.0);
        assert!(incoming.reflect(Vec3::Y).abs_diff_eq(v(0.0, 1.0, 0.0), EPS));
        // A glancing hit keeps its tangential component.
        let glancing = v(1.0, -1.0, 0.0);
        assert!(glancing.reflect(Vec3::Y).abs_diff_eq(v(1.0, 1.0, 0.0), EPS));
    }

    #[test]
    fn is_finite_catches_nan_and_infinity() {
        assert!(v(1.0, 2.0, 3.0).is_finite());
        assert!(!v(f64::NAN, 0.0, 0.0).is_finite());
        assert!(!v(0.0, f64::INFINITY, 0.0).is_finite());
        assert!(!v(0.0, 0.0, f64::NEG_INFINITY).is_finite());
    }

    #[test]
    fn abs_diff_eq_respects_tolerance() {
        let a = v(1.0, 1.0, 1.0);
        let b = v(1.0 + 1e-9, 1.0, 1.0);
        assert!(a.abs_diff_eq(b, 1e-6));
        assert!(!a.abs_diff_eq(b, 1e-12));
    }

    #[test]
    fn arithmetic_is_bit_deterministic() {
        // The whole crate rests on this: +, -, *, / and sqrt are IEEE-754
        // exact, so repeating a computation reproduces the identical bits.
        let a = v(0.1, 0.2, 0.3);
        let b = v(0.7, -1.3, 2.9);
        let run = || ((a + b) * 3.7 - a.cross(b) / 1.3).normalize().length();
        assert_eq!(run().to_bits(), run().to_bits());
    }
}
