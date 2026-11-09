//! Vector, quaternion, and deterministic transcendental math.
//!
//! [`Vec3`] is the replacement for `THREE.Vector3`, which the JS game uses for
//! essentially every quantity that has a direction: positions, velocities,
//! muzzle offsets, surface normals, collision pushes, camera rigs. Ports of
//! `main.js`, `bullets.js`, `missiles.js`, and `bot.js` all bottom out here.
//!
//! [`Quat`] is the replacement for `THREE.Quaternion`, and the rotation algebra
//! beside it ([`quat_mul`], [`quat_rotate`], [`quat_from_axis_angle`],
//! [`quat_normalize`], [`forward`]/[`up`]/[`right`]) is the *only* copy in the
//! crate. It used to be three: `ship.rs`, `bot.rs`, and `missiles.rs` each grew
//! their own while this module was read-only to them, which is exactly the
//! duplication [`crate::world::World`]'s docs warned about.
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
//! Every operation on [`Vec3`] and [`Quat`] is built from `+`, `-`, `*`, `/`,
//! and `sqrt`, all of which IEEE-754 requires to be correctly rounded and
//! therefore bit-identical on every conforming platform.
//!
//! Transcendental functions are *not* correctly rounded by any standard —
//! glibc, musl, Apple's libm and the WASM toolchain differ in the last bits — so
//! none of them are taken from `std` on a simulation path. [`det`] holds
//! hand-rolled replacements built from the exact operations above, and
//! [`quat_from_axis_angle`] is written on top of them. See [`det`] for the
//! provenance of each one.

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

// ---------------------------------------------------------------------------
// Quaternions
// ---------------------------------------------------------------------------

/// A unit quaternion, in the same `(x, y, z, w)` order the JS puts on the wire.
///
/// This type used to live in [`crate::world`], whose docs said the rotation
/// algebra "belongs beside [`Vec3`] in `math`". It does now, and so does the
/// type: the free functions below are the whole of it, and there is exactly one
/// implementation of each. `world` re-exports the type, so `world::Quat` is
/// still a valid path.
///
/// `#[repr(C)]` so it matches the `[f64; 4]` the protocol carries, making the
/// conversions free.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct Quat {
    /// `x` component of the vector part.
    pub x: f64,
    /// `y` component of the vector part.
    pub y: f64,
    /// `z` component of the vector part.
    pub z: f64,
    /// Scalar part.
    pub w: f64,
}

impl Quat {
    /// No rotation. This is what `THREE.Quaternion` starts as, and what every
    /// team-0 spawn uses (`server/index.js:480`).
    pub const IDENTITY: Quat = Quat::new(0.0, 0.0, 0.0, 1.0);

    /// A 180° rotation about `+y`: the team-1 spawn orientation, facing `-z`.
    /// `server/index.js:481` (`[0, 1, 0, 0]`).
    pub const FLIP_Y: Quat = Quat::new(0.0, 1.0, 0.0, 0.0);

    /// Builds a quaternion from components. Not normalized — callers are
    /// responsible for handing in a unit quaternion.
    #[must_use]
    pub const fn new(x: f64, y: f64, z: f64, w: f64) -> Self {
        Quat { x, y, z, w }
    }

    /// Converts from the `[x, y, z, w]` array form used on the wire.
    #[must_use]
    pub const fn from_array(a: [f64; 4]) -> Self {
        Quat::new(a[0], a[1], a[2], a[3])
    }

    /// Converts to the `[x, y, z, w]` array form used on the wire.
    #[must_use]
    pub const fn to_array(self) -> [f64; 4] {
        [self.x, self.y, self.z, self.w]
    }

    /// True if every component is finite. Worth asserting at boundaries: a
    /// `NaN` orientation poisons every derived direction within a tick.
    #[must_use]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite() && self.w.is_finite()
    }
}

impl Default for Quat {
    fn default() -> Self {
        Quat::IDENTITY
    }
}

/// Composes two rotations: the rotation `b` applied *in the frame of* `a`.
///
/// Matches `THREE.Quaternion.multiplyQuaternions(a, b)` term for term, which is
/// what `ship.quaternion.multiply(q)` performs at `main.js:1252`. The operation
/// order is load-bearing — floating-point addition is not associative, so a
/// tidier-looking rearrangement is a different number.
#[inline]
#[must_use]
pub fn quat_mul(a: Quat, b: Quat) -> Quat {
    Quat::new(
        a.x * b.w + a.w * b.x + a.y * b.z - a.z * b.y,
        a.y * b.w + a.w * b.y + a.z * b.x - a.x * b.z,
        a.z * b.w + a.w * b.z + a.x * b.y - a.y * b.x,
        a.w * b.w - a.x * b.x - a.y * b.y - a.z * b.z,
    )
}

/// A rotation of `angle` radians about a unit `axis`.
/// `THREE.Quaternion.setFromAxisAngle`.
///
/// **Transcendental, but deterministically so.** The sine and cosine come from
/// [`det::sin`] and [`det::cos`], never from libm: an orientation is simulation
/// state, and a last-bit disagreement between a server and a WASM client
/// compounds over a match into a desync. `bot.rs` already built its
/// `setFromAxisAngle` this way; `ship.rs` did not, and adopting the
/// deterministic pair is the one behavioural change the hoist makes. The
/// operation order — `axis * sin(angle/2)` for the vector part, `cos(angle/2)`
/// for the scalar — is unchanged.
#[inline]
#[must_use]
pub fn quat_from_axis_angle(axis: Vec3, angle: f64) -> Quat {
    let half = angle * 0.5;
    let s = det::sin(half);
    Quat::new(axis.x * s, axis.y * s, axis.z * s, det::cos(half))
}

/// Renormalizes a quaternion, returning the identity for a degenerate one.
///
/// `THREE.Quaternion.normalize`, which the flight model calls every frame
/// (`main.js:1255`) so integration drift cannot denormalize the pose. A zero or
/// non-finite quaternion comes back as [`Quat::IDENTITY`] rather than `NaN`,
/// matching `THREE`'s own zero-length guard.
#[inline]
#[must_use]
pub fn quat_normalize(q: Quat) -> Quat {
    let len_sq = q.x * q.x + q.y * q.y + q.z * q.z + q.w * q.w;
    if len_sq == 0.0 || !len_sq.is_finite() {
        return Quat::IDENTITY;
    }
    let inv = 1.0 / len_sq.sqrt();
    Quat::new(q.x * inv, q.y * inv, q.z * inv, q.w * inv)
}

/// Rotates `v` by `q`. `THREE.Vector3.applyQuaternion`, reproduced in the same
/// operation order.
#[inline]
#[must_use]
pub fn quat_rotate(q: Quat, v: Vec3) -> Vec3 {
    // t = 2 * cross(q.xyz, v)
    let tx = 2.0 * (q.y * v.z - q.z * v.y);
    let ty = 2.0 * (q.z * v.x - q.x * v.z);
    let tz = 2.0 * (q.x * v.y - q.y * v.x);
    // v + q.w * t + cross(q.xyz, t)
    Vec3::new(
        v.x + q.w * tx + q.y * tz - q.z * ty,
        v.y + q.w * ty + q.z * tx - q.x * tz,
        v.z + q.w * tz + q.x * ty - q.y * tx,
    )
}

/// The ship's nose direction: local `+z` in world space.
///
/// The JS builds this fresh as `new THREE.Vector3(0, 0, 1).applyQuaternion(q)`
/// at half a dozen sites (`main.js:1280`, `:1288`, `:1425`, ...).
#[inline]
#[must_use]
pub fn forward(q: Quat) -> Vec3 {
    quat_rotate(q, Vec3::Z)
}

/// The ship's up direction: local `+y` in world space.
#[inline]
#[must_use]
pub fn up(q: Quat) -> Vec3 {
    quat_rotate(q, Vec3::Y)
}

/// The ship's right direction: local `+x` in world space.
#[inline]
#[must_use]
pub fn right(q: Quat) -> Vec3 {
    quat_rotate(q, Vec3::X)
}

// ---------------------------------------------------------------------------
// Deterministic transcendentals
// ---------------------------------------------------------------------------

/// Transcendental functions that are bit-identical on every platform.
///
/// `f64::sin`, `f64::cos`, `f64::exp`, `f64::acos` and `f64::powf` dispatch to
/// the platform's libm, and libm is only required to be *accurate*, never
/// *correctly rounded*. glibc, musl, Apple's libm and the WASM toolchain
/// disagree in the last bits, and this crate has to produce the same result on
/// a server and in a browser (see the crate docs). Every function here is built
/// from `+ - * /`, `sqrt` and bit manipulation, all of which IEEE-754 requires
/// to be correctly rounded, so the result depends on nothing but the input
/// bits.
///
/// # Provenance
///
/// These were written independently by three wave-1 agents and are collected
/// here unchanged, so no existing result moves:
///
/// | Function | Came from | Method |
/// |---|---|---|
/// | [`det::sin`], [`det::cos`] | `bot.rs` (`dmath`) | Taylor series on the folded quarter-period |
/// | [`det::exp_neg`] | `bot.rs` (`dmath`) | halve, Taylor series, square back up |
/// | [`det::acos`] | `missiles.rs` (`acos_deterministic`) | Sun fdlibm's `__ieee754_acos` rational approximation |
/// | [`det::pow`] | `missiles.rs` (`pow_deterministic`) | `exp2(exp * log2(base))`, both hand-rolled |
///
/// The series are cut where the next term falls below `1e-18` over the argument
/// range the callers use, so the values agree with libm to within an ulp or
/// two. That closeness is a convenience for anyone A/B-ing against the JS;
/// determinism rests on self-consistency, not on agreeing with libm.
pub mod det {
    use std::f64::consts::{FRAC_PI_2, PI};

    /// Series terms for `sin` on `[0, π/2]`.
    const SIN_TERMS: u32 = 10;
    /// Series terms for `cos` on `[0, π/2]`.
    const COS_TERMS: u32 = 11;
    /// Series terms for `exp(-x)` on `[0, 0.5]`.
    const EXP_TERMS: u32 = 17;

    /// Above this, `e^-x` underflows to zero anyway and the halving loop would
    /// spin for no reason.
    const EXP_MAX: f64 = 745.0;

    /// `2π`. Exact: doubling only touches the exponent.
    const TWO_PI: f64 = 2.0 * PI;

    /// `sin(x)`, for any finite `x`.
    ///
    /// Arguments already inside `[-π, π]` — which is every caller in this crate,
    /// because [`super::quat_from_axis_angle`] halves its angle — are used
    /// verbatim, so this is *bit-identical* to the `[0, π]`-only version
    /// `bot.rs` shipped. Larger arguments are reduced modulo `2π` first, which
    /// costs accuracy the further out they go; nothing in the simulation goes
    /// there.
    #[must_use]
    pub fn sin(x: f64) -> f64 {
        let r = reduce(x);
        if r < 0.0 {
            -sin_nonneg(-r)
        } else {
            sin_nonneg(r)
        }
    }

    /// `cos(x)`, for any finite `x`. See [`sin`] for the reduction.
    #[must_use]
    pub fn cos(x: f64) -> f64 {
        cos_nonneg(reduce(x).abs())
    }

    /// `e^-x` for `x >= 0` — the `1 - e^(-rate * dt)` smoothing that every
    /// `THREE.MathUtils.damp` call in the JS is built from.
    ///
    /// A non-positive or `NaN` argument returns `1.0`: that yields a lerp factor
    /// of zero, which holds a value still rather than poisoning it.
    #[must_use]
    pub fn exp_neg(x: f64) -> f64 {
        if x.is_nan() || x <= 0.0 {
            return 1.0;
        }
        if x >= EXP_MAX {
            return 0.0;
        }
        // Halve until the argument is small enough for a short series, then
        // square back up. Halving and squaring only touch the exponent, so they
        // add no reproducibility risk.
        let mut y = x;
        let mut halvings = 0u32;
        while y > 0.5 {
            y *= 0.5;
            halvings += 1;
        }
        let mut r = exp_neg_small(y);
        for _ in 0..halvings {
            r *= r;
        }
        r
    }

    /// Reduces `x` into `[-π, π]`. The identity on `[-π, π]`, which is what
    /// keeps [`sin`] and [`cos`] bit-compatible with the version they replace.
    #[inline]
    fn reduce(x: f64) -> f64 {
        if !x.is_finite() {
            return f64::NAN;
        }
        if x.abs() <= PI {
            return x;
        }
        x - TWO_PI * (x / TWO_PI).round()
    }

    /// `sin(x)` for `x` in `[0, π]` (and a shade beyond, from reduction slop).
    fn sin_nonneg(x: f64) -> f64 {
        // sin(π - x) == sin(x), which folds the range into [0, π/2] where the
        // series is shortest.
        let x = if x > FRAC_PI_2 { PI - x } else { x };
        sin_quarter(x)
    }

    /// `cos(x)` for `x` in `[0, π]` (and a shade beyond).
    fn cos_nonneg(x: f64) -> f64 {
        // cos(π - x) == -cos(x).
        if x > FRAC_PI_2 {
            -cos_quarter(PI - x)
        } else {
            cos_quarter(x)
        }
    }

    /// `sin(x)` for `x` in `[0, π/2]`.
    fn sin_quarter(x: f64) -> f64 {
        let y = x * x;
        let mut term = x;
        let mut sum = x;
        for k in 1..=SIN_TERMS {
            let n = f64::from(2 * k) * f64::from(2 * k + 1);
            term = -term * y / n;
            sum += term;
        }
        sum
    }

    /// `cos(x)` for `x` in `[0, π/2]`.
    fn cos_quarter(x: f64) -> f64 {
        let y = x * x;
        let mut term = 1.0;
        let mut sum = 1.0;
        for k in 1..=COS_TERMS {
            let n = f64::from(2 * k - 1) * f64::from(2 * k);
            term = -term * y / n;
            sum += term;
        }
        sum
    }

    /// `e^-x` for `x` in `[0, 0.5]`.
    fn exp_neg_small(x: f64) -> f64 {
        let mut term = 1.0;
        let mut sum = 1.0;
        for n in 1..=EXP_TERMS {
            term = term * -x / f64::from(n);
            sum += term;
        }
        sum
    }

    /// Arccosine, in radians.
    ///
    /// The homing steer needs the angle between a missile's heading and its
    /// desired heading (`missiles.js:347`, `Math.acos`). A last-bit difference
    /// in that angle changes the steering factor `turn_rate * dt / angle`,
    /// which changes the heading, which compounds over an eight-second flight.
    ///
    /// The rational approximation is the one from Sun's fdlibm
    /// `__ieee754_acos` (Copyright (C) 1993 by Sun Microsystems, Inc.;
    /// permission to use, copy, modify and distribute is freely granted
    /// provided the notice is preserved), which is the ancestor of the routine
    /// in most libms. Accuracy is under an ulp.
    ///
    /// Returns `0.0` at `x >= 1`, `pi` at `x <= -1`, and `NaN` for a `NaN`
    /// input. Out-of-range inputs clamp rather than being rejected, because a
    /// `dot` of two unit vectors can land a hair outside `[-1, 1]` through
    /// rounding.
    #[must_use]
    pub fn acos(x: f64) -> f64 {
        // Coefficients of the fdlibm rational R(z) = P(z) / Q(z), which
        // approximates (asin(s) - s) / s^3. Written as the shortest decimal
        // that round-trips to the same double as fdlibm's own literal.
        const PS0: f64 = 0.166_666_666_666_666_66;
        const PS1: f64 = -0.325_565_818_622_400_9;
        const PS2: f64 = 0.201_212_532_134_862_93;
        const PS3: f64 = -0.040_055_534_500_679_41;
        const PS4: f64 = 0.000_791_534_994_289_814_5;
        const PS5: f64 = 3.479_331_075_960_212e-5;
        const QS1: f64 = -2.403_394_911_734_414;
        const QS2: f64 = 2.020_945_760_233_505_7;
        const QS3: f64 = -0.688_283_971_605_453_3;
        const QS4: f64 = 0.077_038_150_555_901_94;
        /// `pi / 2`, high half — and the nearest double to `pi / 2`, so the
        /// standard constant is exactly fdlibm's `pio2_hi`.
        const PIO2_HI: f64 = FRAC_PI_2;
        /// The part of `pi / 2` that does not fit in [`PIO2_HI`].
        const PIO2_LO: f64 = 6.123_233_995_736_766e-17;
        /// `2^-57`: below this, `acos(x)` is `pi / 2` to the last bit.
        const TINY: f64 = 6.938_893_903_907_228e-18;

        fn rational(z: f64) -> f64 {
            let p = z * (PS0 + z * (PS1 + z * (PS2 + z * (PS3 + z * (PS4 + z * PS5)))));
            let q = 1.0 + z * (QS1 + z * (QS2 + z * (QS3 + z * QS4)));
            p / q
        }

        if x.is_nan() {
            return f64::NAN;
        }
        if x >= 1.0 {
            return 0.0;
        }
        if x <= -1.0 {
            return PI + 2.0 * PIO2_LO;
        }
        if x.abs() < 0.5 {
            if x.abs() <= TINY {
                return PIO2_HI + PIO2_LO;
            }
            let z = x * x;
            let r = rational(z);
            return PIO2_HI - (x - (PIO2_LO - x * r));
        }
        if x < 0.0 {
            let z = (1.0 + x) * 0.5;
            let s = z.sqrt();
            let r = rational(z);
            let w = r * s - PIO2_LO;
            return PI - 2.0 * (s + w);
        }
        let z = (1.0 - x) * 0.5;
        let s = z.sqrt();
        // The top half of `s`, exactly. `df * df` is then exact, which is what
        // lets the correction `c` recover the bits `sqrt` had to drop.
        let df = f64::from_bits(s.to_bits() & 0xffff_ffff_0000_0000);
        let c = (z - df * df) / (s + df);
        let r = rational(z);
        let w = r * s + c;
        2.0 * (df + w)
    }

    /// `base.powf(exp)` for a strictly positive, finite `base`.
    ///
    /// `missiles.js:467` decays a flare's velocity with `Math.pow(0.22, dt)`,
    /// the standard framerate-independent drag idiom, which appears again all
    /// over `main.js` (`0.001^(dt * k / 6)`, `drift_drag^dt`). `powf` is a
    /// composition of `log` and `exp` in the platform's libm and is *not*
    /// bit-identical across platforms — and a flare's position is not cosmetic,
    /// it decides whether a missile is seduced.
    ///
    /// Computed as `exp2(exp * log2(base))`, with both halves built from
    /// `+ - * /` and bit manipulation. Accuracy is a few ulps, far tighter than
    /// anything the simulation can observe; what matters is that they are the
    /// *same* few ulps everywhere.
    ///
    /// Returns `NaN` for a non-positive or non-finite `base`, or a non-finite
    /// `exp`.
    #[must_use]
    pub fn pow(base: f64, exp: f64) -> f64 {
        // `base <= 0.0` is false for a NaN base; `is_finite` is what catches it.
        if base <= 0.0 || !base.is_finite() || !exp.is_finite() {
            return f64::NAN;
        }
        if exp == 0.0 || base == 1.0 {
            return 1.0;
        }
        exp2(exp * log2(base))
    }

    /// Base-2 logarithm of a strictly positive, finite `x`.
    ///
    /// Splits `x` into `m * 2^k` with `m` in `[1/sqrt(2), sqrt(2))` — exact, it
    /// is a bit operation — then evaluates `ln(m)` as the odd series
    /// `2 * atanh(s)` with `s = (m - 1) / (m + 1)`, so `|s| <= 0.1716`. Eleven
    /// terms put the remainder below `1e-17` relative, which is under the last
    /// bit.
    fn log2(x: f64) -> f64 {
        /// `sqrt(2)`, the split point that keeps `|s|` smallest.
        const SQRT_2: f64 = core::f64::consts::SQRT_2;
        /// `1 / ln(2)`.
        const LOG2_E: f64 = core::f64::consts::LOG2_E;
        /// `2^54`, to lift a subnormal into the normal range.
        const TWO_54: f64 = 18_014_398_509_481_984.0;

        let mut bits = x.to_bits();
        let mut biased_exp = ((bits >> 52) & 0x7ff) as i32;
        if biased_exp == 0 {
            let lifted = x * TWO_54;
            bits = lifted.to_bits();
            biased_exp = ((bits >> 52) & 0x7ff) as i32 - 54;
        }
        // The mantissa with the exponent forced to zero, i.e. `m` in `[1, 2)`.
        let mut m = f64::from_bits((bits & 0x000f_ffff_ffff_ffff) | 0x3ff0_0000_0000_0000);
        let mut k = biased_exp - 1023;
        if m > SQRT_2 {
            m *= 0.5;
            k += 1;
        }

        let s = (m - 1.0) / (m + 1.0);
        let z = s * s;
        // 1 + z/3 + z^2/5 + ... + z^10/21, by Horner.
        let poly = 1.0
            + z * (1.0 / 3.0
                + z * (1.0 / 5.0
                    + z * (1.0 / 7.0
                        + z * (1.0 / 9.0
                            + z * (1.0 / 11.0
                                + z * (1.0 / 13.0
                                    + z * (1.0 / 15.0
                                        + z * (1.0 / 17.0
                                            + z * (1.0 / 19.0 + z * (1.0 / 21.0))))))))));
        let ln_m = 2.0 * s * poly;
        f64::from(k) + ln_m * LOG2_E
    }

    /// `2^y`.
    ///
    /// Splits `y` into a nearest integer `n` and a remainder with magnitude at
    /// most `0.5`, evaluates `exp(remainder * ln 2)` by its Taylor series — the
    /// argument is at most `0.347`, where sixteen terms are already below the
    /// last bit — and scales by `2^n`, which is exact.
    fn exp2(y: f64) -> f64 {
        /// `ln(2)`.
        const LN_2: f64 = core::f64::consts::LN_2;

        if y.is_nan() {
            return f64::NAN;
        }
        if y >= 1024.0 {
            return f64::INFINITY;
        }
        if y <= -1075.0 {
            return 0.0;
        }
        let n = if y >= 0.0 {
            (y + 0.5).floor()
        } else {
            (y - 0.5).ceil()
        };
        let r = (y - n) * LN_2;

        let mut term = 1.0;
        let mut sum = 1.0;
        let mut k = 1.0;
        while k <= 16.0 {
            term = term * r / k;
            sum += term;
            k += 1.0;
        }
        sum * two_pow(n as i32)
    }

    /// `2^n` for an integer `n`, assembled from the exponent field. Exact.
    fn two_pow(n: i32) -> f64 {
        if n > 1023 {
            f64::INFINITY
        } else if n >= -1022 {
            f64::from_bits(((n + 1023) as u64) << 52)
        } else if n >= -1074 {
            // Subnormal: the value is a single mantissa bit.
            f64::from_bits(1u64 << (n + 1074))
        } else {
            0.0
        }
    }
}

// ---------------------------------------------------------------------------
// Ballistic prediction
// ---------------------------------------------------------------------------

/// Time until a projectile of speed `speed`, launched now from `self_pos`,
/// reaches a target at `target_pos` moving at `target_vel`.
///
/// `main.js:633` (`solveIntercept`). Solves `|R + U t| = speed * t` for the
/// smallest positive `t`, where `R` is the offset to the target and `U` the
/// relative velocity, and returns `None` when there is no solution — the target
/// is outrunning the projectile, or the geometry is degenerate.
///
/// Only `+ - * /` and `sqrt`, so it is bit-identical everywhere; see [`det`] for
/// why that matters.
///
/// # On `self_vel`
///
/// Pass [`Vec3::ZERO`] for a gun in this game. A bullet is spawned with
/// `direction * bullet_speed` and inherits nothing from the shooter
/// (`bullets.js:44`), so the shooter's own motion must not enter the relative
/// velocity. `bot.js:172` passes zero and is correct.
///
/// The JS player aim assist does **not**: `main.js:2047` passes `shipVelocity`,
/// which solves for a projectile that carries the ship's momentum. The faster
/// the player flies, the further the assisted reticle leads by an amount the
/// bullet never makes up. Both Rust callers — [`crate::bot`] and
/// [`crate::aim_assist`] — pass zero, and
/// `aim_assist::tests::the_shooters_own_velocity_never_enters_the_intercept_solve`
/// pins that so the JS bug cannot come back.
///
/// This lives here, next to [`Vec3`], because it has two call sites in two
/// modules and a third copy is exactly how the JS ended up with the divergence
/// above.
///
/// ```
/// use spaceships_sim::math::{solve_intercept, Vec3};
///
/// // A target 100 units ahead, sitting still, and a 10 u/s projectile.
/// let t = solve_intercept(Vec3::new(0.0, 0.0, 100.0), Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, 10.0);
/// assert_eq!(t, Some(10.0));
/// ```
#[must_use]
pub fn solve_intercept(
    target_pos: Vec3,
    target_vel: Vec3,
    self_pos: Vec3,
    self_vel: Vec3,
    speed: f64,
) -> Option<f64> {
    let r = target_pos - self_pos;
    let u = target_vel - self_vel;
    let rr = r.length_squared();
    let ru = r.dot(u);
    let uu = u.length_squared();

    let a = uu - speed * speed;
    let b = 2.0 * ru;
    let c = rr;

    if a.abs() < 1e-6 {
        // The target is closing at exactly projectile speed: the quadratic
        // collapses to a linear equation.
        if b.abs() < 1e-6 {
            return None;
        }
        let t = -c / b;
        return if t > 0.0 { Some(t) } else { None };
    }

    let disc = b * b - 4.0 * a * c;
    if disc < 0.0 {
        return None;
    }
    let sd = disc.sqrt();
    let t1 = (-b - sd) / (2.0 * a);
    let t2 = (-b + sd) / (2.0 * a);
    let mut t = f64::INFINITY;
    if t1 > 0.0 {
        t = t.min(t1);
    }
    if t2 > 0.0 {
        t = t.min(t2);
    }
    if t.is_finite() {
        Some(t)
    } else {
        None
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
