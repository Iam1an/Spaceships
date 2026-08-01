//! Homing missiles and flare countermeasures.
//!
//! Port of `public/src/missiles.js`, minus everything that was a mesh. The JS
//! file is 484 lines of which roughly 300 build cones, cylinders, additive
//! sprites and explosion layers; what is left — and what lives here — is five
//! behaviours:
//!
//! 1. **Lock-on** ([`acquire_lock`]). Which enemy the `E` key hands the missile.
//! 2. **Homing** ([`update`]). A rate-limited turn toward the target.
//! 3. **Obstacle avoidance** ([`update`], via the private avoidance kernel).
//!    Missiles steer around asteroids and the moon.
//! 4. **Flare seduction** ([`deploy_flares`], [`update`]). `Q` releases
//!    [`crate::rules::WeaponRules::flare_count`] decoys; a missile within
//!    [`crate::rules::WeaponRules::flare_seduction_dist`] of someone else's
//!    flare abandons its target for it.
//! 5. **Detonation** ([`update`]). Impact, proximity, obstacle and lifetime
//!    detonations, resolved as a swept segment rather than a point sample.
//!
//! # Two fixes carried in from the analysis, both verified against the source
//!
//! **`missiles.js` ignores the boss's hit radius.** `bullets.js` reads the
//! target record's `hitRadius` override (`const reach = (r.hitRadius !==
//! undefined ? r.hitRadius : SHIP_HIT_RADIUS) + RADIUS`), which the campaign
//! sets to 28 when it inserts the boss hitboxes (`main.js:2945`,
//! `hitRadius: 28`). `missiles.js` never looks at the field: its ship test is
//! `dx*dx + dy*dy + dz*dz < HIT_RADIUS * HIT_RADIUS` against its own private
//! `HIT_RADIUS = 6.0`. So the 50-damage weapon has to pass within 6 units of a
//! hitbox *point* while the 10-damage weapon gets 28, and in practice missiles
//! sail through the capital ship. Here every hit test goes through
//! [`Ship::hit_radius`], which returns [`Ship::hit_radius_override`] when the
//! campaign has set one — the unified value from
//! [`crate::rules::WeaponRules::boss_hitbox_radius`]. One radius, one function,
//! no weapon-specific answer.
//!
//! **`missiles.js` adds no radius for the missile body — confirmed.** The
//! `TODO(verify)` on [`crate::rules::WeaponRules::missile_radius`] is correct:
//! the JS compares squared distance against `HIT_RADIUS` alone, where
//! `bullets.js` adds its `RADIUS = 0.5` to every reach. A missile is a 3.5-unit
//! body with a 0.28 radius plus a 1.8-unit nose cone (`BODY_LEN`, `BODY_RAD`,
//! `NOSE_LEN`), so 0.0 is an oversight rather than a decision — but it *is* the
//! shipped behaviour, so it is preserved. The value is threaded through every
//! test in this module as `rules.weapons.missile_radius`, so raising it is a
//! one-line rules change and nothing here needs to be touched.
//!
//! # Swept detonation
//!
//! The JS moves the missile and then asks whether the new *point* is inside
//! something. A missile is not a bullet, so this is not the outright disaster
//! it is in `bullets.js`: at 160 u/s a missile covers 2.7 units per 60 Hz tick
//! and 8 units at the [`crate::rules::MAX_FRAME_DT`] cap, against target
//! spheres 12–13.5 units across, so a *head-on* contact is never skipped
//! outright. What the point test does drop is every **grazing** contact — a
//! step whose two endpoints both sit outside the sphere while the segment
//! between them clips it. Near the edge of a 6-unit hitbox that is most of the
//! contacts, which is what makes missiles feel like they pass through people.
//!
//! Every test here instead sweeps the segment the missile actually travelled
//! ([`crate::collision`]) and takes the **earliest** contact across all
//! candidates, so a missile that crosses a flare and a ship in one step
//! detonates on whichever it reached first rather than on whichever the loop
//! happened to visit first.
//!
//! # Determinism
//!
//! Two transcendental functions are unavoidable in the ported math, and both
//! are hazards under the crate's one rule (see [`crate`]): `sin`/`cos`/`powf`
//! and friends are not bit-identical across platforms or libm versions, and the
//! same simulation has to run on a server and in a browser's WASM.
//!
//! Neither is taken from `std`. Both are re-implemented here from `+ - * /`,
//! `sqrt` and bit manipulation only, which IEEE-754 requires to be correctly
//! rounded and therefore identical everywhere:
//!
//! - [`acos_deterministic`] — the homing steer needs the angle between the
//!   current and desired directions (`missiles.js`: `Math.acos`).
//! - [`pow_deterministic`] — the flare velocity decay is
//!   `vel *= FLARE_DRAG ^ dt` (`missiles.js`: `Math.pow(0.22, dt)`).
//!
//! The third would have been `Math.sin`/`Math.cos` in the flare burst's
//! spherical direction sampling; that one is avoided outright by sampling the
//! sphere with a rejection method that needs only `sqrt`. See
//! [`random_unit_vector`].
//!
//! **These two functions do not belong in this module.** They are general math
//! and every other ported module will want them (`ship.rs` alone needs
//! `0.001^(dt*k/6)`, `drift_drag^dt` and `1 - e^(-rate*dt)`). They live here
//! only because `math.rs` is not this module's to edit.
//!
//! # What stayed in JS
//!
//! Trail particles, explosion layers, nozzle glow pulses, flare flicker, and
//! every `Math.random()` that only decides a sprite's size or lifetime. Those
//! are render state: they never feed back into a position, so they are not
//! simulation and are not reproduced. The simulation reports
//! [`SimEvent::Explosion`] and [`SimEvent::FlareBurst`] and lets the renderer
//! decide what a detonation looks like.

use crate::collision::{swept_sphere_aabb, swept_sphere_sphere, Aabb, Sphere};
use crate::math::Vec3;
use crate::rng::Rng;
use crate::world::{
    EntityId, ExplosionKind, Flare, Missile, MissileTarget, Quat, Ship, ShipKind, SimEvent, Team,
    World,
};

// ---------------------------------------------------------------------------
// Constants the JS has and `rules` does not
// ---------------------------------------------------------------------------

/// Least distance to a target at which a missile still steers toward it.
///
/// `missiles.js:335` (`if (d > 0.5)`). Inside half a unit the direction to the
/// target is numerically meaningless, so the missile holds its heading for the
/// one tick before it detonates. Not in [`crate::rules`].
const MIN_TRACK_DISTANCE: f64 = 0.5;

/// Angular error below which a missile does not steer at all.
///
/// `missiles.js:348` (`if (angleDiff > 0.001)`). Also the guard that keeps the
/// `turn_rate * dt / angle` factor from dividing by zero. Not in
/// [`crate::rules`].
const MIN_TURN_ANGLE: f64 = 0.001;

/// Below this distance from the missile's heading to an obstacle centre, the
/// avoidance kernel cannot form a push direction and falls back to a fixed
/// sideways nudge. `missiles.js:97` (`if (dist < 1e-3)`). Not in
/// [`crate::rules`].
const AVOID_DEGENERATE_DIST: f64 = 1e-3;

/// Rough world-space size reported for a missile detonation, for the renderer's
/// particle preset. The JS explosion's outermost layer grows to 16
/// (`missiles.js:204`). Cosmetic; nothing in the simulation reads it.
const EXPLOSION_SCALE: f64 = 16.0;

// ---------------------------------------------------------------------------
// Detonation shapes supplied by the caller
// ---------------------------------------------------------------------------

/// An extra solid a missile detonates against, supplied per tick by the caller.
///
/// The campaign's capital ship is the reason this exists: its hull is a shape
/// that `campaign.rs` owns and this module must not know about. Detonation is
/// therefore resolved generically — hand in whatever spheres and boxes the
/// current mode has, and the resulting [`DetonationCause::Volume`] names the
/// index you handed in so the owner can decide what a hit on it means.
///
/// Nothing here damages a volume. The missile detonates, the caller is told,
/// and the caller applies whatever that costs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Volume {
    /// A solid sphere.
    Sphere(Sphere),
    /// A solid axis-aligned box.
    Aabb(Aabb),
}

impl Volume {
    /// Earliest fraction of the step at which a sphere of `radius` moving from
    /// `origin` by `motion` touches this volume.
    fn sweep(self, origin: Vec3, motion: Vec3, radius: f64) -> Option<f64> {
        match self {
            Volume::Sphere(s) => swept_sphere_sphere(origin, motion, radius, s),
            Volume::Aabb(b) => swept_sphere_aabb(origin, motion, radius, b),
        }
    }
}

// ---------------------------------------------------------------------------
// Detonation reports
// ---------------------------------------------------------------------------

/// Why a missile stopped existing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetonationCause {
    /// [`crate::rules::WeaponRules::missile_life`] ran out. `missiles.js:313`.
    Expired,
    /// It reached a ship. Damage has already been applied to that ship's hit
    /// points by [`update`]; the fields describe what was applied so a caller
    /// can score it, or report it to a server, without recomputing anything.
    Ship {
        /// Who was hit.
        id: EntityId,
        /// Damage applied, [`crate::rules::WeaponRules::missile_damage`].
        damage: i32,
        /// Whether that damage took the ship to zero.
        killed: bool,
    },
    /// It reached a flare, which was consumed. `missiles.js:381`.
    Flare {
        /// [`Flare::key`] of the decoy that ate it.
        key: u64,
    },
    /// It reached an asteroid. No damage: `missiles.js` detonates on rocks
    /// without ever calling an asteroid-damage path, unlike `bullets.js`.
    Asteroid {
        /// [`crate::world::Asteroid::id`].
        id: u32,
    },
    /// It reached an indestructible sphere — the moon.
    Obstacle {
        /// Index into [`World::obstacles`].
        index: usize,
    },
    /// It reached one of the caller-supplied [`Volume`]s.
    Volume {
        /// Index into the slice passed to [`update`].
        index: usize,
    },
}

/// One missile detonation, reported to the caller.
///
/// Emitted for every missile that leaves [`World::missiles`], including the
/// ones that simply timed out, so a caller can drive audio, particles, the kill
/// feed, and the network `hit` message from one list.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detonation {
    /// [`Missile::key`] of the missile that detonated.
    pub missile: u64,
    /// Who fired it.
    pub owner: EntityId,
    /// The owner's team at launch.
    pub owner_team: Option<Team>,
    /// Where it went off. For a contact detonation this is the point on the
    /// swept segment where the surfaces first touched, not the end-of-tick
    /// position the JS reports.
    pub pos: Vec3,
    /// What it went off on.
    pub cause: DetonationCause,
}

// ---------------------------------------------------------------------------
// Deterministic replacements for the two transcendentals the port needs
// ---------------------------------------------------------------------------

/// Arccosine, in radians, built only from IEEE-exact operations.
///
/// # Why this is not `f64::acos`
///
/// The homing steer needs the angle between the missile's heading and its
/// desired heading (`missiles.js:347`, `Math.acos`). `f64::acos` dispatches to
/// the platform's libm, and libm implementations of the inverse trigonometric
/// functions are not required to be correctly rounded — they differ in the last
/// bits between glibc, musl, Apple's libm and the WASM toolchain's. A last-bit
/// difference in the angle changes the steering factor
/// `turn_rate * dt / angle`, which changes the heading, which compounds over an
/// eight-second flight. Server and client would disagree about where the
/// missile went.
///
/// This implementation uses only `+ - * /`, `sqrt` and a bit mask, all of which
/// IEEE-754 requires to be correctly rounded, so it produces identical bits on
/// x86-64, aarch64 and wasm32.
///
/// The rational approximation is the one from Sun's fdlibm `__ieee754_acos`
/// (Copyright (C) 1993 by Sun Microsystems, Inc.; permission to use, copy,
/// modify and distribute is freely granted provided the notice is preserved),
/// which is the ancestor of the routine in most libms. Accuracy is under an
/// ulp; the tests check it against `f64::acos` over the whole domain.
///
/// Returns `0.0` at `x >= 1`, `pi` at `x <= -1`, and `NaN` for a `NaN` input.
/// Out-of-range inputs clamp rather than being rejected, because a `dot` of two
/// unit vectors can land a hair outside `[-1, 1]` through rounding.
#[must_use]
pub fn acos_deterministic(x: f64) -> f64 {
    // Coefficients of the fdlibm rational R(z) = P(z) / Q(z), which
    // approximates (asin(s) - s) / s^3. Written as the shortest decimal that
    // round-trips to the same double as fdlibm's own literal.
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
    const PIO2_HI: f64 = core::f64::consts::FRAC_PI_2;
    /// The part of `pi / 2` that does not fit in [`PIO2_HI`].
    const PIO2_LO: f64 = 6.123_233_995_736_766e-17;
    /// `2^-57`: below this, `acos(x)` is `pi / 2` to the last bit.
    const TINY: f64 = 6.938_893_903_907_228e-18;
    /// `pi`. fdlibm spells this `3.14159265358979311600e+00`, which is the
    /// same double.
    const PI: f64 = core::f64::consts::PI;

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
    // The top half of `s`, exactly. `df * df` is then exact, which is what lets
    // the correction `c` recover the bits `sqrt` had to drop.
    let df = f64::from_bits(s.to_bits() & 0xffff_ffff_0000_0000);
    let c = (z - df * df) / (s + df);
    let r = rational(z);
    let w = r * s + c;
    2.0 * (df + w)
}

/// `base.powf(exp)` for a strictly positive, finite `base`, built only from
/// IEEE-exact operations.
///
/// # Why this is not `f64::powf`
///
/// `missiles.js:467` decays a flare's velocity with `Math.pow(0.22, dt)`, the
/// standard framerate-independent drag idiom, which appears again all over
/// `main.js` (`0.001^(dt * k / 6)`, `drift_drag^dt`). `powf` is a composition of
/// `log` and `exp` in the platform's libm and is *not* bit-identical across
/// platforms — and a flare's position is not cosmetic, it decides whether a
/// missile is seduced. Two machines that disagree about where a flare drifted
/// disagree about whether a missile turned.
///
/// Computed as `exp2(exp * log2(base))`, with both halves built from `+ - * /`
/// and bit manipulation. Accuracy is a few ulps, far tighter than anything the
/// simulation can observe; what matters is that they are the *same* few ulps
/// everywhere.
///
/// Returns `NaN` for a non-positive or non-finite `base`, or a non-finite
/// `exp`.
#[must_use]
pub fn pow_deterministic(base: f64, exp: f64) -> f64 {
    // `base <= 0.0` is false for a NaN base; `is_finite` is what catches it.
    if base <= 0.0 || !base.is_finite() || !exp.is_finite() {
        return f64::NAN;
    }
    if exp == 0.0 || base == 1.0 {
        return 1.0;
    }
    exp2_deterministic(exp * log2_deterministic(base))
}

/// Base-2 logarithm of a strictly positive, finite `x`, from exact operations.
///
/// Splits `x` into `m * 2^k` with `m` in `[1/sqrt(2), sqrt(2))` — exact, it is
/// a bit operation — then evaluates `ln(m)` as the odd series `2 * atanh(s)`
/// with `s = (m - 1) / (m + 1)`, so `|s| <= 0.1716`. Eleven terms put the
/// remainder below `1e-17` relative, which is under the last bit.
fn log2_deterministic(x: f64) -> f64 {
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
                                    + z * (1.0 / 17.0 + z * (1.0 / 19.0 + z * (1.0 / 21.0))))))))));
    let ln_m = 2.0 * s * poly;
    f64::from(k) + ln_m * LOG2_E
}

/// `2^y`, from exact operations.
///
/// Splits `y` into a nearest integer `n` and a remainder with magnitude at most
/// `0.5`, evaluates `exp(remainder * ln 2)` by its Taylor series — the argument
/// is at most `0.347`, where sixteen terms are already below the last bit — and
/// scales by `2^n`, which is exact.
fn exp2_deterministic(y: f64) -> f64 {
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

// ---------------------------------------------------------------------------
// Quaternion helper
// ---------------------------------------------------------------------------

/// `v` rotated by the unit quaternion `q`.
///
/// [`Quat`] is deliberately a data type with no operations, and `math.rs` — the
/// module its docs point at — is not this module's to edit, so the one rotation
/// a missile needs lives here. This is `THREE.Vector3.applyQuaternion` term for
/// term, so a ported call site produces the same bits the JS did.
fn rotate(q: Quat, v: Vec3) -> Vec3 {
    let tx = 2.0 * (q.y * v.z - q.z * v.y);
    let ty = 2.0 * (q.z * v.x - q.x * v.z);
    let tz = 2.0 * (q.x * v.y - q.y * v.x);
    Vec3::new(
        v.x + q.w * tx + q.y * tz - q.z * ty,
        v.y + q.w * ty + q.z * tx - q.x * tz,
        v.z + q.w * tz + q.x * ty - q.y * tx,
    )
}

/// The direction a ship's nose points: its local `+z` in world space.
#[must_use]
pub fn forward(quat: Quat) -> Vec3 {
    rotate(quat, Vec3::Z)
}

// ---------------------------------------------------------------------------
// Lock-on
// ---------------------------------------------------------------------------

/// Whether `target` can be locked by a missile at all.
///
/// The JS rule is `if (!r.alive || !r.hasTarget) continue` (`main.js:1394`).
/// `hasTarget` means "a network `state` message has arrived for this ship"
/// (`main.js:849`), which is why solo bots have it forced on at spawn
/// (`main.js:2477`) and multiplayer bots too (`main.js:2967`).
///
/// **One deliberate deviation.** The campaign sets `hasTarget` on exactly one
/// of the boss's twenty hitboxes (`main.js:2738`, `r.hasTarget = (i === 0)`),
/// because the flag also gates the HUD's target marker and twenty markers on
/// one capital ship would be unreadable. That makes a *rendering* flag decide
/// which parts of the boss can be locked, and hitbox 0 sits at
/// `(-85, 0, -150)` from the hull origin — so the only lockable point on the
/// capital ship is one corner of it. Here the flag is honoured for
/// [`ShipKind::Remote`] only, where it means what it says; bots, the local
/// ship, and boss hitboxes are lockable whenever they are alive. Together with
/// the hit-radius fix in [`update`], that is what makes missiles work against
/// the boss at all.
fn is_lockable(target: &Ship) -> bool {
    if !target.alive {
        return false;
    }
    match target.kind {
        ShipKind::Remote => target.interp.has_target,
        ShipKind::Local | ShipKind::Bot | ShipKind::BossHitbox => true,
    }
}

/// True if nothing solid stands between `from` and `to`.
///
/// The JS occlusion test (`main.js:1399`–`:1419`) casts a ray at every asteroid
/// and every obstacle and rejects the candidate if a hit lands nearer than the
/// target. Reproduced with a zero-radius sweep of the exact segment, which is
/// the same test written as a fraction of the segment instead of a distance
/// along a unit ray.
///
/// Note what does *not* occlude: [`World::boxes`]. The JS list holds asteroids
/// and obstacles only, so a lock can be taken straight through a mothership or
/// an airfield. Preserved.
#[must_use]
pub fn has_line_of_sight(world: &World, from: Vec3, to: Vec3) -> bool {
    let motion = to - from;
    for a in &world.asteroids {
        if swept_sphere_sphere(from, motion, 0.0, Sphere::new(a.pos, a.radius)).is_some() {
            return false;
        }
    }
    for o in &world.obstacles {
        if swept_sphere_sphere(from, motion, 0.0, Sphere::new(o.pos, o.radius)).is_some() {
            return false;
        }
    }
    true
}

/// The ship a missile fired by `shooter` right now would lock onto.
///
/// Port of the `KeyE` acquisition block at `main.js:1629`–`:1422`: **the
/// nearest living enemy with a clear line of sight**, and nothing else.
///
/// It is worth being explicit about what the JS rule does *not* contain,
/// because it is easy to assume otherwise:
///
/// - **No cone.** Facing is not consulted at all. A missile locks a target
///   directly behind the shooter as readily as one dead ahead, and then spends
///   the first second of its flight turning around.
/// - **No maximum range.** The nearest enemy on the map is a valid lock even at
///   two kilometres, where the missile's eight-second life
///   ([`crate::rules::WeaponRules::missile_life`]) at 160 u/s gives it 1280
///   units of travel and it expires short of the target.
///
/// The only gate is line of sight, so the rule reads as "the nearest thing you
/// could actually see", not "the thing you are aiming at". Both gaps are
/// balance decisions rather than porting ones, so both are preserved; a cone
/// and a range belong in [`crate::rules`] if they are ever wanted.
///
/// Ties go to the earlier ship in [`World::ships`], matching the JS's
/// `if (d >= closestDist) continue` over a `Map` in insertion order.
#[must_use]
pub fn acquire_lock(world: &World, shooter: EntityId) -> Option<EntityId> {
    let shooter_ship = world.ship(shooter)?;
    let origin = shooter_ship.pos;
    let shooter_team = shooter_ship.team;

    let mut best: Option<(f64, EntityId)> = None;
    for candidate in &world.ships {
        if candidate.id == shooter || !is_lockable(candidate) {
            continue;
        }
        // Friendly fire is rejected only when both sides have a team, matching
        // `server/index.js:941` and [`World::can_damage`].
        if let (Some(a), Some(b)) = (shooter_team, candidate.team) {
            if a == b {
                continue;
            }
        }
        let d = origin.distance(candidate.pos);
        if let Some((best_d, _)) = best {
            if d >= best_d {
                continue;
            }
        }
        if !has_line_of_sight(world, origin, candidate.pos) {
            continue;
        }
        best = Some((d, candidate.id));
    }
    best.map(|(_, id)| id)
}

// ---------------------------------------------------------------------------
// Firing
// ---------------------------------------------------------------------------

/// Launches one missile from `shooter` at `target`, spending a round.
///
/// Returns the new missile's [`Missile::key`], or `None` if the shooter does
/// not exist, is dead, or is out of missiles — in which case nothing is spent.
///
/// `target` is taken rather than acquired so that a bot, which picks its own
/// victim through its own range and cone rules (`bot.js`), goes through the
/// same launcher as the player. Pass `None` for a dumb-fire missile: it flies
/// straight until it expires or hits something, which is also what a missile
/// does after its target dies (`missiles.js:331`).
///
/// Geometry from `main.js:1426`: the missile appears
/// [`crate::rules::WeaponRules::missile_spawn_offset`] units ahead of the ship
/// along its nose, pointing the same way. It inherits no velocity —
/// `missiles.js:299` sets `vel` to `dir * MISSILE_SPEED` and nothing else — so
/// firing while boosting does not make a faster missile.
pub fn fire(world: &mut World, shooter: EntityId, target: Option<EntityId>) -> Option<u64> {
    let spawn_offset = world.rules.weapons.missile_spawn_offset;
    let life = world.rules.weapons.missile_life;

    let ship = world.ship(shooter)?;
    if !ship.alive || ship.missiles_left == 0 {
        return None;
    }
    let dir = forward(ship.quat).normalize();
    let pos = ship.pos.add_scaled(dir, spawn_offset);
    let owner_team = ship.team;

    let key = world.take_projectile_key();
    world.missiles.push(Missile {
        key,
        pos,
        dir,
        target: target.map(MissileTarget::Ship),
        life,
        age: 0.0,
        owner: shooter,
        owner_team,
    });
    if let Some(ship) = world.ship_mut(shooter) {
        ship.missiles_left -= 1;
    }
    Some(key)
}

/// Acquires a lock and fires: the player's `E` key, in one call.
///
/// Returns `None` — spending nothing — when there is no valid target, which is
/// what `main.js:1423` does: the whole launch block sits inside
/// `if (closestRecord !== null)`, so a missile with nothing to shoot at is
/// never launched and the round stays on the rail.
pub fn fire_locked(world: &mut World, shooter: EntityId) -> Option<u64> {
    let target = acquire_lock(world, shooter)?;
    fire(world, shooter, Some(target))
}

/// Whether any missile in flight is currently homing on `id`.
///
/// `missiles.js:282` (`isTargetingLocal`), which drives the cockpit's lock
/// warning ([`crate::world::HudState::missile_lock_warning`]). A missile that
/// has been seduced by a flare stops counting, which is the feedback that tells
/// a pilot the countermeasure worked.
#[must_use]
pub fn is_targeting(world: &World, id: EntityId) -> bool {
    world
        .missiles
        .iter()
        .any(|m| m.target == Some(MissileTarget::Ship(id)))
}

// ---------------------------------------------------------------------------
// Flares
// ---------------------------------------------------------------------------

/// A uniformly-distributed unit vector, drawn without a transcendental.
///
/// `missiles.js:239` samples the sphere the textbook way — a uniform azimuth
/// and a uniform `cos(phi)`, assembled with `Math.cos(theta)` and
/// `Math.sin(theta)`. Those two calls are exactly the platform-dependent
/// rounding the crate rules forbid, and a flare's position is *not* cosmetic:
/// it decides whether a missile within
/// [`crate::rules::WeaponRules::flare_seduction_dist`] diverts.
///
/// This uses Marsaglia's 1972 method instead: draw a point in the unit disc by
/// rejection, then map it to the sphere with one `sqrt`. The distribution is
/// exactly the same uniform one — not an approximation of the JS, a different
/// exact sampler for the same distribution — and it needs nothing but multiply,
/// subtract and `sqrt`.
///
/// The rejection loop makes the number of draws consumed data-dependent (about
/// 1.27 pairs per vector on average). That is still fully deterministic: the
/// same seed rejects the same candidates in the same order on every machine. It
/// does mean [`crate::world::RNG_STREAM_EFFECTS`] must not be shared with
/// anything that needs a fixed draw count at a fixed point in the stream.
#[must_use]
pub fn random_unit_vector(rng: &mut Rng) -> Vec3 {
    loop {
        let u = rng.next_f64_signed();
        let v = rng.next_f64_signed();
        let s = u * u + v * v;
        if s < 1.0 {
            let f = 2.0 * (1.0 - s).sqrt();
            return Vec3::new(u * f, v * f, 1.0 - 2.0 * s);
        }
    }
}

/// Releases one flare charge from `owner`: a burst of
/// [`crate::rules::WeaponRules::flare_count`] decoys.
///
/// Returns the number of flares released, or `0` if the ship does not exist, is
/// dead, or has no charges left — in which case nothing is spent.
///
/// Each decoy gets a uniformly random direction and a speed of
/// `flare_speed * (jitter_min + U * jitter_range)`, i.e. 65 %–135 % of
/// [`crate::rules::WeaponRules::flare_speed`] (`missiles.js:247`). All of it is
/// drawn from [`crate::world::WorldRng::effects`], the stream whose docs
/// already name flare directions as one of its consumers.
///
/// **The burn time is not random.** Every flare gets exactly
/// [`crate::rules::WeaponRules::flare_life`] (1.8 s); the only `Math.random()`
/// touching a lifetime in `missiles.js` is on the *trail sprites* a flare emits
/// (`missiles.js:193`), which are render state and are not simulated. So the
/// question "does this flare outlive the missile chasing it" has one answer on
/// every machine.
///
/// The JS also rotates each sampled direction by the ship's orientation
/// (`applyQuaternion(shipQuaternion)`). Rotating a uniformly-distributed
/// direction does not change its distribution, so that step is not reproduced;
/// the burst is isotropic either way.
pub fn deploy_flares(world: &mut World, owner: EntityId, events: &mut Vec<SimEvent>) -> u32 {
    let count = world.rules.weapons.flare_count;
    let base_speed = world.rules.weapons.flare_speed;
    let jitter_min = world.rules.weapons.flare_speed_jitter_min;
    let jitter_range = world.rules.weapons.flare_speed_jitter_range;
    let life = world.rules.weapons.flare_life;

    let Some(ship) = world.ship(owner) else {
        return 0;
    };
    if !ship.alive || ship.flares_left == 0 {
        return 0;
    }
    let origin = ship.pos;
    if let Some(ship) = world.ship_mut(owner) {
        ship.flares_left -= 1;
    }

    events.push(SimEvent::FlareBurst { owner, origin });
    for _ in 0..count {
        let vel = {
            let rng = &mut world.rng.effects;
            let dir = random_unit_vector(rng);
            let speed = base_speed * (jitter_min + rng.next_f64() * jitter_range);
            dir * speed
        };
        let key = world.take_projectile_key();
        world.flares.push(Flare {
            key,
            pos: origin,
            vel,
            life,
            age: 0.0,
            owner,
        });
    }
    count
}

// ---------------------------------------------------------------------------
// Obstacle avoidance
// ---------------------------------------------------------------------------

/// The avoidance push for one missile, or `None` if nothing is in the way.
///
/// Port of `computeAvoidance` (`missiles.js:80`), kept term for term because
/// the trajectory shape is the point: for every asteroid and obstacle, project
/// its centre onto the missile's heading, and if that closest-approach point
/// lands inside `radius + missile_avoid_self_radius + missile_avoid_margin`,
/// add a unit push away from the centre weighted by how soon the missile gets
/// there. The accumulated pushes are normalized once at the end.
///
/// Three details are load-bearing and are reproduced exactly:
///
/// - The lookahead is `max(missile_avoid_base_lookahead, radius * scale)`, so a
///   50-unit asteroid is noticed at 160 units while a 5-unit one is noticed at
///   130. Anything whose projection is beyond the lookahead, or more than its
///   own radius *behind* the missile, is ignored.
/// - `urgency` falls linearly from 1 at the missile's nose to 0 at the
///   lookahead, so a rock that is already close outweighs a bigger one further
///   out.
/// - Summation order is [`World::asteroids`] first, then [`World::obstacles`],
///   each in list order. Floating-point addition is not associative, so this is
///   part of the answer, not a detail of the loop.
///
/// The pushes are computed against the *heading*, not the velocity, and they do
/// not grow as the surface gets closer beyond the urgency ramp — which is why a
/// missile threading a dense field weaves rather than braking. That is the JS
/// behaviour.
fn compute_avoidance(world: &World, origin: Vec3, dir: Vec3) -> Option<Vec3> {
    let rules = &world.rules.weapons;
    let mut out = Vec3::ZERO;
    let mut any = false;

    let mut consider = |centre: Vec3, radius: f64| {
        let to_centre = centre - origin;
        let t = to_centre.dot(dir);
        let look_ahead = rules
            .missile_avoid_base_lookahead
            .max(radius * rules.missile_avoid_radius_scale);
        if t < -radius || t > look_ahead {
            return;
        }
        let closest = origin.add_scaled(dir, t);
        let away = closest - centre;
        let dist_sq = away.length_squared();
        let threshold = radius + rules.missile_avoid_self_radius + rules.missile_avoid_margin;
        if dist_sq > threshold * threshold {
            return;
        }
        let urgency = 1.0 - t.max(0.0) / look_ahead;
        let dist = dist_sq.sqrt();
        if dist < AVOID_DEGENERATE_DIST {
            // Dead centre: there is no "away", so shove sideways in the
            // horizontal plane. `missiles.js:98` leaves `y` alone, which is why
            // a missile aimed exactly at a rock's centre always breaks sideways
            // and never over the top.
            out = Vec3::new(out.x + (-dir.z) * urgency, out.y, out.z + dir.x * urgency);
        } else {
            out = out.add_scaled(away / dist, urgency);
        }
        any = true;
    };

    for a in &world.asteroids {
        consider(a.pos, a.radius);
    }
    for o in &world.obstacles {
        consider(o.pos, o.radius);
    }

    if any {
        // `THREE.Vector3.normalize` divides by `length() || 1`, so a set of
        // pushes that cancels exactly comes back as zero rather than NaN and
        // the caller's weighted add then leaves the desired direction alone.
        // `Vec3::normalize` has the same contract.
        Some(out.normalize())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// The tick
// ---------------------------------------------------------------------------

/// What a missile ran into, as indices into the lists that were searched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HitKind {
    Asteroid(usize),
    Obstacle(usize),
    Volume(usize),
    Flare(usize),
    Ship(usize),
}

/// Keeps whichever candidate contact happens first; an exact tie goes to the
/// candidate offered first.
fn keep_earliest(best: &mut Option<(f64, HitKind)>, t: Option<f64>, kind: HitKind) {
    let Some(t) = t else { return };
    match best {
        Some((best_t, _)) if *best_t <= t => {}
        _ => *best = Some((t, kind)),
    }
}

/// Whether a missile fired by `owner`/`owner_team` may detonate on `target`.
///
/// The same three clauses as [`World::can_damage`] — no self-damage, no
/// friendly fire, nothing dead or inside its spawn window — with one
/// difference: the team is the one the missile *carried off the rail*
/// ([`Missile::owner_team`]), not the one its owner has now. An eight-second
/// missile outlives a team change, and the JS already stores the launch team
/// for exactly this reason (`missiles.js:305`).
///
/// The invulnerability clause is new to bots: `missiles.js:396` checks only
/// `r.alive`, so a solo bot could be killed on its spawn anchor while the
/// player could not. See [`crate::rules::CombatRules::spawn_invuln`].
fn can_detonate_on(missile: &Missile, target: &Ship) -> bool {
    if target.id == missile.owner {
        return false;
    }
    if !target.is_damageable() {
        return false;
    }
    match (missile.owner_team, target.team) {
        (Some(a), Some(b)) => a != b,
        _ => true,
    }
}

/// Where a missile's current target is, or `None` if the target is gone.
///
/// A dead ship and a burnt-out flare both come back as `None`, which is the
/// `if (!tgt.alive) m.target = null` branch at `missiles.js:330`: the missile
/// loses its lock and flies straight for the rest of its life.
fn target_pos(world: &World, target: MissileTarget) -> Option<Vec3> {
    match target {
        MissileTarget::Ship(id) => world.ship(id).filter(|s| s.alive).map(|s| s.pos),
        MissileTarget::Flare(key) => world
            .flares
            .iter()
            .find(|f| f.key == key && f.life > 0.0)
            .map(|f| f.pos),
    }
}

/// Retargets a missile onto the nearest enemy flare, if one is close enough.
///
/// `missiles.js:316`. The selection rule in full: among flares that are still
/// burning and were **not** released by the missile's own owner, take the one
/// nearest the missile, provided it is within
/// [`crate::rules::WeaponRules::flare_seduction_dist`].
///
/// # Determinism
///
/// There is no randomness in this decision at all — not in the JS either. The
/// only `Math.random()` anywhere near flares picks burst directions and speeds
/// (routed through [`crate::world::WorldRng::effects`] by [`deploy_flares`])
/// and trail-sprite sizes (render state, not simulated). The divert itself is a
/// nearest-neighbour scan over [`World::flares`] in order, with a strict `<`
/// comparison so an exact distance tie keeps the earlier flare. Same world,
/// same answer, every machine.
///
/// Two JS behaviours preserved:
///
/// - Only a missile whose target is a **ship** can be seduced. Once it is
///   chasing a flare it will not hop to a nearer one, and once its target is
///   `None` — because the ship died, or the flare burnt out — it is immune to
///   flares for the rest of its life and flies straight.
/// - Own-owner flares are skipped by id comparison. The JS spells this
///   `if (m.ownerId && f.ownerId && m.ownerId === f.ownerId)`, whose truthiness
///   guard would let entity id `0` be seduced by its own flares. Id `0` does not
///   currently belong to a ship, but the guard is a latent bug, so the
///   comparison here is the plain one that was meant.
fn seduce(world: &World, missile: &mut Missile) {
    if !matches!(missile.target, Some(MissileTarget::Ship(_))) {
        return;
    }
    let mut nearest_dist = world.rules.weapons.flare_seduction_dist;
    let mut nearest: Option<u64> = None;
    for f in &world.flares {
        if f.life <= 0.0 || f.owner == missile.owner {
            continue;
        }
        let d = missile.pos.distance(f.pos);
        if d < nearest_dist {
            nearest_dist = d;
            nearest = Some(f.key);
        }
    }
    if let Some(key) = nearest {
        missile.target = Some(MissileTarget::Flare(key));
    }
}

/// The heading a missile takes this step.
///
/// The steering law from `missiles.js:341`–`:353`, which is *not* a slerp and
/// should not be tidied into one:
///
/// ```text
/// desired = target ? unit(target - pos) : heading
/// if avoidance: desired = unit(desired + avoid * AVOID_WEIGHT)
/// angle   = acos(clamp(heading . desired, -1, 1))
/// if angle > 0.001:
///     heading = unit(lerp(heading, desired, min(1, TURN_RATE * dt / angle)))
/// ```
///
/// The interpolation is linear-then-renormalized, so the realised turn is
/// slightly *less* than `turn_rate * dt`, and dramatically less for large
/// angles — a missile with a 180° error turns at well under half its rated rate
/// through the start of the reversal. That shortfall is the trajectory's
/// signature: it is why a missile fired at a target behind the shooter swings
/// wide instead of pivoting on the spot. Replacing the nlerp with a proper
/// rotation would make missiles measurably better at reversing, which is a
/// balance change, not a port.
///
/// Below `0.001` radians the missile does not steer at all and keeps its exact
/// previous heading, so a missile flying straight accumulates no drift.
fn steer(heading: Vec3, desired: Vec3, turn_rate: f64, dt: f64) -> Vec3 {
    let dot = heading.dot(desired).clamp(-1.0, 1.0);
    let angle = acos_deterministic(dot);
    if angle > MIN_TURN_ANGLE {
        let factor = (turn_rate * dt / angle).min(1.0);
        heading.lerp(desired, factor).normalize()
    } else {
        heading
    }
}

/// Advances every missile and flare in `world` by `dt`.
///
/// Call this once per tick, after ships have moved. It owns [`World::missiles`]
/// and [`World::flares`] entirely: it moves them, resolves what they hit,
/// applies missile damage to [`World::ships`], and removes whatever stopped
/// existing.
///
/// - `volumes` are extra solids to detonate against — see [`Volume`]. Pass
///   `&[]` when there are none.
/// - `events` collects [`SimEvent::Explosion`] for the renderer and
///   [`SimEvent::Damaged`] / [`SimEvent::ShipDestroyed`] for the HUD and score.
/// - `detonations` collects one [`Detonation`] per missile that left the world,
///   which is what a caller turns into a network `hit` message.
///
/// # Order of operations
///
/// Missiles are stepped before flares, in reverse index order, both matching
/// `missiles.js:309` and `:454`. Reverse order is not cosmetic: when two
/// missiles could eat the same flare, the later one gets it. Stepping missiles
/// first means a missile sees flare positions from the *end of the previous
/// tick*, which is the frame of reference the seduction distance was tuned in.
///
/// Within one missile's step: age, seduce, resolve target, avoid, steer, move,
/// detonate.
///
/// # Detonation
///
/// The missile's step is a segment, and every candidate — asteroids, the moon,
/// caller volumes, flares, ships — is swept against that whole segment. The
/// earliest contact wins, so the outcome does not depend on which list was
/// searched first. Exact ties fall back to the JS's order (obstacle, then
/// flare, then ship), which only matters for contrived geometry.
///
/// Radii, each read from exactly one place:
///
/// | Target | Reach |
/// |---|---|
/// | Ship | [`Ship::hit_radius`] + [`crate::rules::WeaponRules::missile_radius`] |
/// | Flare | [`crate::rules::ShipRules::hit_radius`] + `missile_radius` |
/// | Asteroid, obstacle | its radius + [`crate::rules::WeaponRules::missile_detonate_margin`] |
///
/// The ship row is the boss fix: [`Ship::hit_radius`] returns the campaign's
/// 28-unit override where `missiles.js` used its own 6.0. The flare row uses
/// the unified ship radius because `rules` has no flare-specific one and
/// `missiles.js` used a single `HIT_RADIUS` for both — numerically identical,
/// but it is a stand-in and is called out in the port notes.
///
/// # What a detonation does not do
///
/// It does not damage asteroids. `missiles.js:355` detonates on a rock and
/// stops there; only `bullets.js` reports an asteroid hit. So a 50-damage
/// missile chips nothing off a 5 HP rock while a 10-damage bullet takes a point
/// off it. That reads like an oversight next to
/// [`crate::rules::CombatRules::asteroid_damage_per_hit`], whose docs assume
/// every weapon chips a rock, but it is the shipped behaviour and changing it
/// is a balance call. The [`DetonationCause::Asteroid`] report carries the rock
/// id, so a caller that wants the other behaviour has what it needs.
pub fn update(
    world: &mut World,
    dt: f64,
    volumes: &[Volume],
    events: &mut Vec<SimEvent>,
    detonations: &mut Vec<Detonation>,
) {
    // Missiles are lifted out so that resolving a hit can take `&mut World` to
    // apply damage. Nothing fires a missile from inside this loop, so the list
    // cannot grow while it is out.
    let mut missiles = core::mem::take(&mut world.missiles);
    let mut i = missiles.len();
    while i > 0 {
        i -= 1;
        if step_missile(world, &mut missiles[i], dt, volumes, events, detonations) {
            missiles.remove(i);
        }
    }
    world.missiles = missiles;

    update_flares(world, dt);
}

/// Steps one missile. Returns `true` if it detonated and should be removed.
fn step_missile(
    world: &mut World,
    missile: &mut Missile,
    dt: f64,
    volumes: &[Volume],
    events: &mut Vec<SimEvent>,
    detonations: &mut Vec<Detonation>,
) -> bool {
    missile.life -= dt;
    missile.age += dt;
    if missile.life <= 0.0 {
        let pos = missile.pos;
        report(missile, pos, DetonationCause::Expired, events, detonations);
        return true;
    }

    let heading = missile.dir;

    seduce(world, missile);

    // Resolve the target into a direction, dropping a lock that no longer
    // points at anything.
    let mut desired = heading;
    if let Some(target) = missile.target {
        match target_pos(world, target) {
            None => missile.target = None,
            Some(p) => {
                let to_target = p - missile.pos;
                let d = to_target.length();
                if d > MIN_TRACK_DISTANCE {
                    desired = to_target / d;
                }
            }
        }
    }

    if let Some(avoid) = compute_avoidance(world, missile.pos, heading) {
        desired = desired
            .add_scaled(avoid, world.rules.weapons.missile_avoid_weight)
            .normalize();
    }

    missile.dir = steer(heading, desired, world.rules.weapons.missile_turn_rate, dt);

    // `missiles.js:352`/`:354`: the velocity is rebuilt from the heading every
    // step, then the position advances by `vel * dt`. Spelled the same way so
    // the rounding matches.
    let vel = missile.dir * world.rules.weapons.missile_speed;
    let start = missile.pos;
    let motion = vel * dt;
    missile.pos = start + motion;

    let Some((t, kind)) = first_contact(world, missile, start, motion, volumes) else {
        return false;
    };
    let contact = start + motion * t;
    let cause = resolve_contact(world, missile, kind, contact, events);
    report(missile, contact, cause, events, detonations);
    true
}

/// The earliest thing this missile's step ran into, if anything.
fn first_contact(
    world: &World,
    missile: &Missile,
    start: Vec3,
    motion: Vec3,
    volumes: &[Volume],
) -> Option<(f64, HitKind)> {
    let weapons = &world.rules.weapons;
    let body = weapons.missile_radius;
    let margin = weapons.missile_detonate_margin;
    let mut best: Option<(f64, HitKind)> = None;

    // Solid geometry first, mirroring `insideObstacle` running before the flare
    // and ship tests in the JS.
    for (idx, a) in world.asteroids.iter().enumerate() {
        let sphere = Sphere::new(a.pos, a.radius + margin);
        keep_earliest(
            &mut best,
            swept_sphere_sphere(start, motion, body, sphere),
            HitKind::Asteroid(idx),
        );
    }
    for (idx, o) in world.obstacles.iter().enumerate() {
        let sphere = Sphere::new(o.pos, o.radius + margin);
        keep_earliest(
            &mut best,
            swept_sphere_sphere(start, motion, body, sphere),
            HitKind::Obstacle(idx),
        );
    }
    for (idx, v) in volumes.iter().enumerate() {
        keep_earliest(
            &mut best,
            v.sweep(start, motion, body),
            HitKind::Volume(idx),
        );
    }

    // Flares. Note the JS does *not* skip the missile owner's own flares here,
    // only in the seduction scan — so your own countermeasures detonate your own
    // missiles if they drift into one. Preserved.
    let flare_reach = world.rules.ship.hit_radius;
    for (idx, f) in world.flares.iter().enumerate() {
        if f.life <= 0.0 {
            continue;
        }
        let sphere = Sphere::new(f.pos, flare_reach);
        keep_earliest(
            &mut best,
            swept_sphere_sphere(start, motion, body, sphere),
            HitKind::Flare(idx),
        );
    }

    for (idx, s) in world.ships.iter().enumerate() {
        if !can_detonate_on(missile, s) {
            continue;
        }
        // `false`: the coarse-aim bonus is a property of a shooter's *gun*
        // (`main.js:406` passes it into `createBullets` and nowhere else).
        // `missiles.js` never saw it, so a missile gets the plain radius — or
        // the campaign's override, which is the fix this port exists for.
        let sphere = Sphere::new(s.pos, s.hit_radius(&world.rules, false));
        keep_earliest(
            &mut best,
            swept_sphere_sphere(start, motion, body, sphere),
            HitKind::Ship(idx),
        );
    }

    best
}

/// Applies whatever the contact costs, and names it.
fn resolve_contact(
    world: &mut World,
    missile: &Missile,
    kind: HitKind,
    contact: Vec3,
    events: &mut Vec<SimEvent>,
) -> DetonationCause {
    match kind {
        HitKind::Asteroid(idx) => DetonationCause::Asteroid {
            id: world.asteroids[idx].id,
        },
        HitKind::Obstacle(index) => DetonationCause::Obstacle { index },
        HitKind::Volume(index) => DetonationCause::Volume { index },
        HitKind::Flare(idx) => {
            let key = world.flares[idx].key;
            // A consumed flare is a flare with no burn time left; the flare
            // pass sweeps it out of the list in the same tick.
            world.flares[idx].life = 0.0;
            DetonationCause::Flare { key }
        }
        HitKind::Ship(idx) => {
            let damage = world.rules.weapons.missile_damage;
            let respawn_delay = world.rules.combat.respawn_delay;
            let ship = &mut world.ships[idx];
            let id = ship.id;
            ship.hp = (ship.hp - damage).max(0);
            ship.hit_flash = 1.0;
            ship.health_idle_damage = 0.0;
            let new_hp = ship.hp;
            let killed = new_hp <= 0;
            if killed {
                ship.alive = false;
                ship.respawn_timer = respawn_delay;
            }
            events.push(SimEvent::Damaged {
                id,
                amount: damage,
                new_hp,
                source: Some(missile.owner),
            });
            if killed {
                events.push(SimEvent::ShipDestroyed {
                    id,
                    killer: Some(missile.owner),
                    pos: contact,
                });
            }
            DetonationCause::Ship { id, damage, killed }
        }
    }
}

/// Emits the explosion and the detonation record for a missile that is leaving.
fn report(
    missile: &Missile,
    pos: Vec3,
    cause: DetonationCause,
    events: &mut Vec<SimEvent>,
    detonations: &mut Vec<Detonation>,
) {
    events.push(SimEvent::Explosion {
        pos,
        scale: EXPLOSION_SCALE,
        kind: ExplosionKind::MissileHit,
    });
    detonations.push(Detonation {
        missile: missile.key,
        owner: missile.owner,
        owner_team: missile.owner_team,
        pos,
        cause,
    });
}

/// Ages, moves and drags every flare, dropping the ones that burnt out.
///
/// `missiles.js:454`–`:466`. Two details that look like nothing and are not:
///
/// - The position advances on the velocity the flare had at the *start* of the
///   step; the drag is applied afterwards. Swapping the two shortens every
///   burst.
/// - Burn-out is checked before the move, so a flare that expires this tick
///   does not get a final step. Its last simulated position is where a missile
///   saw it.
fn update_flares(world: &mut World, dt: f64) {
    let drag = world.rules.weapons.flare_drag;
    // One decay factor for the whole tick: every flare has the same `dt`, so
    // computing it once is not merely cheaper, it guarantees two flares cannot
    // drift apart by a rounding step. `flare_drag` is not covered by
    // `Rules::validate`, hence the guard.
    let decay = if drag > 0.0 && drag.is_finite() {
        pow_deterministic(drag, dt)
    } else {
        1.0
    };

    let mut i = world.flares.len();
    while i > 0 {
        i -= 1;
        let f = &mut world.flares[i];
        f.age += dt;
        f.life -= dt;
        if f.life <= 0.0 {
            world.flares.remove(i);
            continue;
        }
        f.pos = f.pos.add_scaled(f.vel, dt);
        f.vel *= decay;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::Rules;
    use crate::world::{Asteroid, AsteroidTier, MapKind, Mode, Obstacle};

    fn v(x: f64, y: f64, z: f64) -> Vec3 {
        Vec3::new(x, y, z)
    }

    /// A world with no moon and no rocks, so a test only sees what it adds.
    fn world() -> World {
        World::new(
            0x5EED_0451,
            Rules::DEFAULT,
            Mode::Skirmish,
            MapKind::Terrain,
        )
    }

    fn add_ship(world: &mut World, id: EntityId, kind: ShipKind, pos: Vec3, team: Team) -> usize {
        let mut ship = Ship::spawn(id, kind, pos, Quat::IDENTITY, &world.rules);
        ship.team = Some(team);
        // `Ship::spawn` opens a spawn-protection window; every test that fires
        // at a ship wants it already expired.
        ship.invuln_timer = 0.0;
        ship.interp.has_target = true;
        world.ships.push(ship);
        world.ships.len() - 1
    }

    fn add_rock(world: &mut World, id: u32, pos: Vec3, radius: f64) {
        world.asteroids.push(Asteroid {
            id,
            pos,
            size: radius,
            radius,
            hp: 5,
            tier: AsteroidTier::Small,
            variant: 0,
            rot: Vec3::ZERO,
            spin: Vec3::ZERO,
            hit_flash: 0.0,
        });
    }

    fn add_flare(world: &mut World, pos: Vec3, owner: EntityId) -> u64 {
        let key = world.take_projectile_key();
        let life = world.rules.weapons.flare_life;
        world.flares.push(Flare {
            key,
            pos,
            vel: Vec3::ZERO,
            life,
            age: 0.0,
            owner,
        });
        key
    }

    fn tick(world: &mut World, dt: f64) -> (Vec<SimEvent>, Vec<Detonation>) {
        let mut events = Vec::new();
        let mut detonations = Vec::new();
        update(world, dt, &[], &mut events, &mut detonations);
        (events, detonations)
    }

    /// Ticks until a missile detonates, or gives up.
    fn tick_until_detonation(world: &mut World, dt: f64, steps: usize) -> Option<Detonation> {
        for _ in 0..steps {
            let (_, dets) = tick(world, dt);
            if let Some(d) = dets.first() {
                return Some(*d);
            }
        }
        None
    }

    // -----------------------------------------------------------------
    // The deterministic transcendentals.
    // -----------------------------------------------------------------

    #[test]
    fn acos_matches_the_platform_libm_across_the_domain() {
        // Accuracy is checked against `f64::acos` (allowed in a test, banned in
        // the simulation). What the simulation needs is that this function is
        // *the same everywhere*; what this test adds is that it is also right.
        let mut worst = 0.0f64;
        for i in 0..=20_000 {
            let x = -1.0 + f64::from(i) * 1e-4;
            let got = acos_deterministic(x);
            let want = x.acos();
            worst = worst.max((got - want).abs());
        }
        assert!(worst < 1e-15, "worst absolute error {worst}");

        assert_eq!(acos_deterministic(1.0), 0.0);
        assert!((acos_deterministic(-1.0) - std::f64::consts::PI).abs() < 1e-15);
        assert!((acos_deterministic(0.0) - std::f64::consts::FRAC_PI_2).abs() < 1e-16);
        // Out of range clamps rather than producing NaN: a dot product of two
        // unit vectors can land a hair outside [-1, 1].
        assert_eq!(acos_deterministic(1.0 + 1e-16), 0.0);
        assert!(acos_deterministic(-1.5) >= std::f64::consts::PI);
        assert!(acos_deterministic(f64::NAN).is_nan());
    }

    #[test]
    fn acos_near_one_stays_accurate_where_the_steering_factor_lives() {
        // The homing factor is `turn_rate * dt / angle`, so small angles matter
        // most: they produce the largest factors.
        for i in 1..2000 {
            let x = 1.0 - f64::from(i) * 1e-9;
            let got = acos_deterministic(x);
            let want = x.acos();
            assert!(
                (got - want).abs() <= want.abs() * 1e-12 + 1e-18,
                "x = {x}: {got} vs {want}"
            );
        }
    }

    #[test]
    fn acos_is_bit_stable() {
        let run = || acos_deterministic(0.37).to_bits();
        assert_eq!(run(), run());
    }

    #[test]
    fn pow_matches_powf_where_the_simulation_uses_it() {
        // The flare drag, `0.22 ^ dt`, for every plausible step.
        for i in 1..=5000 {
            let dt = f64::from(i) * 1e-5;
            let got = pow_deterministic(0.22, dt);
            let want = 0.22f64.powf(dt);
            assert!(
                (got - want).abs() <= want.abs() * 1e-14,
                "dt = {dt}: {got} vs {want}"
            );
        }
        // And over a wider grid, since the same helper will serve `ship.rs`.
        for base in [1e-8, 0.001, 0.1, 0.22, 0.5, 0.9, 1.5, 7.0, 1e6] {
            for exp in [-30.0, -3.5, -0.5, 0.0, 1.0 / 60.0, 1.0, 3.0, 12.0] {
                let got = pow_deterministic(base, exp);
                let want = base.powf(exp);
                assert!(
                    (got - want).abs() <= want.abs() * 1e-13,
                    "{base}^{exp}: {got} vs {want}"
                );
            }
        }
        assert_eq!(pow_deterministic(0.22, 0.0), 1.0);
        assert_eq!(pow_deterministic(1.0, 12.5), 1.0);
        assert!(pow_deterministic(-1.0, 0.5).is_nan());
        assert!(pow_deterministic(0.0, 0.5).is_nan());
    }

    #[test]
    fn pow_is_bit_stable() {
        let run = || pow_deterministic(0.22, 1.0 / 60.0).to_bits();
        assert_eq!(run(), run());
    }

    // -----------------------------------------------------------------
    // Lock-on.
    // -----------------------------------------------------------------

    #[test]
    fn lock_picks_the_nearest_enemy_and_ignores_friends_and_the_dead() {
        let mut w = world();
        add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
        add_ship(&mut w, 2, ShipKind::Remote, v(0.0, 0.0, 300.0), Team::One);
        add_ship(&mut w, 3, ShipKind::Remote, v(0.0, 0.0, 100.0), Team::One);
        // A team-mate at 10 units must not be a candidate.
        add_ship(&mut w, 4, ShipKind::Remote, v(0.0, 0.0, 10.0), Team::Zero);
        assert_eq!(acquire_lock(&w, 1), Some(3));

        // Kill the nearest enemy and the lock falls back to the far one.
        w.ship_mut(3).unwrap().alive = false;
        assert_eq!(acquire_lock(&w, 1), Some(2));

        // A remote with no received pose is not lockable.
        w.ship_mut(2).unwrap().interp.has_target = false;
        assert_eq!(acquire_lock(&w, 1), None);
    }

    #[test]
    fn lock_has_no_cone_and_no_range_limit() {
        // Both are absent from `main.js`'s acquisition block, and their absence
        // is load-bearing: it is what lets a missile be fired backwards.
        let mut w = world();
        add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
        // Directly behind the shooter, which faces +z.
        add_ship(&mut w, 2, ShipKind::Remote, v(0.0, 0.0, -50.0), Team::One);
        assert_eq!(acquire_lock(&w, 1), Some(2));

        // And two kilometres away, well past what an 8 s, 160 u/s missile can
        // reach.
        w.ship_mut(2).unwrap().pos = v(0.0, 0.0, 2000.0);
        assert_eq!(acquire_lock(&w, 1), Some(2));
    }

    #[test]
    fn lock_skips_a_target_hidden_behind_a_rock() {
        let mut w = world();
        add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
        add_ship(&mut w, 2, ShipKind::Remote, v(0.0, 0.0, 100.0), Team::One);
        // The far one is off the near one's bearing, so one rock cannot hide
        // both.
        add_ship(&mut w, 3, ShipKind::Remote, v(300.0, 0.0, 0.0), Team::One);
        assert_eq!(acquire_lock(&w, 1), Some(2));

        // Bury the near one behind a rock; the far one is still in the clear.
        add_rock(&mut w, 1, v(0.0, 0.0, 50.0), 20.0);
        assert_eq!(acquire_lock(&w, 1), Some(3));

        // The moon blocks too.
        w.obstacles.push(Obstacle {
            pos: v(150.0, 0.0, 0.0),
            radius: 80.0,
        });
        assert_eq!(acquire_lock(&w, 1), None);
    }

    #[test]
    fn firing_without_a_lock_spends_nothing() {
        let mut w = world();
        add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
        let start = w.ship(1).unwrap().missiles_left;
        assert_eq!(fire_locked(&mut w, 1), None);
        assert_eq!(w.ship(1).unwrap().missiles_left, start);
        assert!(w.missiles.is_empty());
    }

    #[test]
    fn firing_spawns_ahead_of_the_nose_and_spends_a_round() {
        let mut w = world();
        add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
        add_ship(&mut w, 2, ShipKind::Remote, v(0.0, 0.0, 400.0), Team::One);
        let loadout = w.ship(1).unwrap().missiles_left;

        let key = fire_locked(&mut w, 1).expect("a lock exists");
        assert_eq!(w.ship(1).unwrap().missiles_left, loadout - 1);
        assert_eq!(w.missiles.len(), 1);
        let m = w.missiles[0];
        assert_eq!(m.key, key);
        assert_eq!(m.pos, v(0.0, 0.0, w.rules.weapons.missile_spawn_offset));
        assert_eq!(m.dir, Vec3::Z);
        assert_eq!(m.target, Some(MissileTarget::Ship(2)));
        assert_eq!(m.life, w.rules.weapons.missile_life);
        assert!(is_targeting(&w, 2));
        assert!(!is_targeting(&w, 1));

        // Empty rails fire nothing.
        w.ship_mut(1).unwrap().missiles_left = 0;
        assert_eq!(fire_locked(&mut w, 1), None);
    }

    #[test]
    fn forward_is_the_ships_nose() {
        assert_eq!(forward(Quat::IDENTITY), Vec3::Z);
        // A 180-degree turn about y is the team-1 spawn orientation.
        let back = forward(Quat::FLIP_Y);
        assert!(back.abs_diff_eq(-Vec3::Z, 1e-15), "{back:?}");
    }

    // -----------------------------------------------------------------
    // Homing.
    // -----------------------------------------------------------------

    /// The JS steering law, transcribed straight from `missiles.js:346`–`:352`
    /// and using the platform `acos` on purpose. The port has to reproduce this
    /// trajectory, not merely arrive at the same place.
    fn js_steer(heading: Vec3, desired: Vec3, turn_rate: f64, dt: f64) -> Vec3 {
        let dot = heading.dot(desired).clamp(-1.0, 1.0);
        let angle = dot.acos();
        if angle > 0.001 {
            let factor = (turn_rate * dt / angle).min(1.0);
            heading.lerp(desired, factor).normalize()
        } else {
            heading
        }
    }

    #[test]
    fn the_homing_trajectory_matches_the_js_step_for_step() {
        // A missile launched across a stationary target: the classic pursuit
        // curve, which is where a wrong turn law shows up as a different shape
        // rather than a different endpoint.
        let turn_rate = Rules::DEFAULT.weapons.missile_turn_rate;
        let speed = Rules::DEFAULT.weapons.missile_speed;
        let dt = 1.0 / 60.0;
        let target = v(300.0, 40.0, 0.0);

        let mut pos = Vec3::ZERO;
        let mut dir = Vec3::Z;
        let mut js_pos = Vec3::ZERO;
        let mut js_dir = Vec3::Z;
        let mut turned = false;

        for step in 0..400 {
            let to = target - pos;
            let d = to.length();
            let desired = if d > MIN_TRACK_DISTANCE { to / d } else { dir };
            dir = steer(dir, desired, turn_rate, dt);
            pos += dir * speed * dt;

            let js_to = target - js_pos;
            let js_d = js_to.length();
            let js_desired = if js_d > 0.5 { js_to / js_d } else { js_dir };
            js_dir = js_steer(js_dir, js_desired, turn_rate, dt);
            js_pos += js_dir * speed * dt;

            assert!(
                pos.abs_diff_eq(js_pos, 1e-9),
                "diverged at step {step}: {pos:?} vs {js_pos:?}"
            );
            turned |= dir.dot(Vec3::Z) < 0.9;
        }
        assert!(turned, "the missile never actually manoeuvred");
    }

    #[test]
    fn the_turn_rate_is_capped_and_the_nlerp_undershoots_at_large_angles() {
        let dt = 1.0 / 60.0;
        let rate = 1.4;

        // A modest error: the realised turn is very close to the rated rate.
        let heading = Vec3::Z;
        let desired = v(0.3, 0.0, 1.0).normalize();
        let next = steer(heading, desired, rate, dt);
        let turned = acos_deterministic(heading.dot(next).clamp(-1.0, 1.0));
        assert!(turned <= rate * dt + 1e-12, "over-rotated: {turned}");
        assert!(turned > rate * dt * 0.98, "under-rotated: {turned}");

        // A full reversal: the linear interpolation cuts the chord, so the
        // realised turn falls well short of the rated rate. That shortfall is
        // the JS trajectory's signature and must not be "fixed".
        let reversed = steer(Vec3::Z, -Vec3::Z, rate, dt);
        let turned = acos_deterministic(Vec3::Z.dot(reversed).clamp(-1.0, 1.0));
        assert!(turned < rate * dt * 0.75, "reversal turned {turned}");
        assert!(turned >= 0.0);

        // Below the dead angle nothing moves at all, bit for bit.
        assert_eq!(steer(Vec3::Z, Vec3::Z, rate, dt), Vec3::Z);
    }

    #[test]
    fn a_missile_flies_straight_after_losing_its_lock() {
        let mut w = world();
        add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
        add_ship(
            &mut w,
            2,
            ShipKind::Remote,
            v(200.0, 200.0, 200.0),
            Team::One,
        );
        fire_locked(&mut w, 1).unwrap();

        // The target dies before the missile arrives.
        w.ship_mut(2).unwrap().alive = false;
        tick(&mut w, 1.0 / 60.0);
        assert_eq!(w.missiles[0].target, None);
        let heading = w.missiles[0].dir;
        for _ in 0..30 {
            tick(&mut w, 1.0 / 60.0);
        }
        assert_eq!(w.missiles[0].dir, heading, "an unlocked missile must coast");
    }

    // -----------------------------------------------------------------
    // Avoidance.
    // -----------------------------------------------------------------

    #[test]
    fn avoidance_pushes_away_from_a_rock_ahead_and_ignores_one_behind() {
        let mut w = world();
        // Slightly off-centre so there is a well-defined "away".
        add_rock(&mut w, 1, v(4.0, 0.0, 60.0), 10.0);

        let push = compute_avoidance(&w, Vec3::ZERO, Vec3::Z).expect("rock ahead");
        assert!(push.x < -0.9, "should shove away from +x: {push:?}");
        assert!(push.is_normalized(1e-12));

        // Facing away: nothing to avoid.
        assert!(compute_avoidance(&w, Vec3::ZERO, -Vec3::Z).is_none());

        // Beyond the lookahead: nothing to avoid.
        assert!(compute_avoidance(&w, v(0.0, 0.0, -200.0), Vec3::Z).is_none());

        // Off to one side by more than radius + self + margin: nothing.
        assert!(compute_avoidance(&w, v(-60.0, 0.0, 0.0), Vec3::Z).is_none());
    }

    #[test]
    fn avoidance_breaks_sideways_when_aimed_at_a_centre_exactly() {
        let mut w = world();
        add_rock(&mut w, 1, v(0.0, 0.0, 60.0), 10.0);
        let push = compute_avoidance(&w, Vec3::ZERO, Vec3::Z).expect("rock ahead");
        // `missiles.js:98` only touches x and z, so the escape is horizontal.
        assert_eq!(push.y, 0.0);
        assert!(push.x.abs() > 0.99);
    }

    #[test]
    fn avoidance_bends_a_missile_around_the_moon_instead_of_into_it() {
        let mut w = world();
        w.obstacles.push(Obstacle {
            pos: v(0.0, 0.0, 300.0),
            radius: 80.0,
        });
        add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
        // The target sits directly behind the moon, so a missile that does not
        // avoid flies straight into it. The lock is handed in rather than
        // acquired, because `acquire_lock` would (correctly) refuse a target it
        // cannot see.
        add_ship(&mut w, 2, ShipKind::Remote, v(0.0, 0.0, 600.0), Team::One);
        fire(&mut w, 1, Some(2)).unwrap();

        let mut detonated_on_moon = false;
        let mut closest = f64::INFINITY;
        for _ in 0..240 {
            let (_, dets) = tick(&mut w, 1.0 / 60.0);
            for d in dets {
                if matches!(d.cause, DetonationCause::Obstacle { .. }) {
                    detonated_on_moon = true;
                }
            }
            if let Some(m) = w.missiles.first() {
                closest = closest.min(m.pos.distance(v(0.0, 0.0, 300.0)));
            }
        }
        assert!(!detonated_on_moon, "avoidance failed to clear the moon");
        assert!(closest > 80.0, "clipped the surface at {closest}");
    }

    // -----------------------------------------------------------------
    // Flares.
    // -----------------------------------------------------------------

    #[test]
    fn a_burst_releases_the_whole_charge_isotropically() {
        let mut w = world();
        add_ship(&mut w, 1, ShipKind::Local, v(10.0, 0.0, 0.0), Team::Zero);
        let charges = w.ship(1).unwrap().flares_left;
        let mut events = Vec::new();

        let n = deploy_flares(&mut w, 1, &mut events);
        assert_eq!(n, w.rules.weapons.flare_count);
        assert_eq!(w.flares.len(), n as usize);
        assert_eq!(w.ship(1).unwrap().flares_left, charges - 1);
        assert!(matches!(events[0], SimEvent::FlareBurst { owner: 1, .. }));

        let mut sum = Vec3::ZERO;
        let lo = w.rules.weapons.flare_speed * w.rules.weapons.flare_speed_jitter_min;
        let hi = lo + w.rules.weapons.flare_speed * w.rules.weapons.flare_speed_jitter_range;
        for f in &w.flares {
            assert_eq!(f.pos, v(10.0, 0.0, 0.0));
            assert_eq!(f.life, w.rules.weapons.flare_life);
            assert_eq!(f.owner, 1);
            let speed = f.vel.length();
            assert!((lo..=hi).contains(&speed), "speed {speed}");
            sum += f.vel.normalize();
        }
        // Twenty uniform directions should not all point one way.
        assert!(sum.length() < 12.0, "burst is not isotropic: {sum:?}");

        // No charges left, no burst.
        w.ship_mut(1).unwrap().flares_left = 0;
        assert_eq!(deploy_flares(&mut w, 1, &mut events), 0);
    }

    #[test]
    fn flare_bursts_replay_identically_from_the_same_seed() {
        let build = || {
            let mut w = World::new(99, Rules::DEFAULT, Mode::Skirmish, MapKind::Terrain);
            add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
            let mut events = Vec::new();
            deploy_flares(&mut w, 1, &mut events);
            w.flares.clone()
        };
        let a = build();
        let b = build();
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(
                x.vel.to_array().map(f64::to_bits),
                y.vel.to_array().map(f64::to_bits)
            );
        }
    }

    #[test]
    fn random_unit_vectors_are_unit_length_and_reproducible() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        for _ in 0..2000 {
            let x = random_unit_vector(&mut a);
            assert!((x.length() - 1.0).abs() < 1e-15, "not unit: {x:?}");
            assert_eq!(x, random_unit_vector(&mut b));
        }
    }

    #[test]
    fn flares_burn_for_their_rated_life_and_decay_their_velocity() {
        let dt = 1.0 / 60.0;
        let mut w = world();
        add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
        let mut events = Vec::new();
        deploy_flares(&mut w, 1, &mut events);

        let mut elapsed = 0.0;
        while !w.flares.is_empty() {
            tick(&mut w, dt);
            elapsed += dt;
            assert!(elapsed < 3.0, "flares outlived their burn time");
        }
        assert!(
            (elapsed - w.rules.weapons.flare_life).abs() < dt * 1.5,
            "burnt for {elapsed}"
        );

        // And the drag really bites: after a second the speed is 0.22 of the
        // launch speed.
        let mut w = world();
        add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
        deploy_flares(&mut w, 1, &mut events);
        let launch_speed = w.flares[0].vel.length();
        for _ in 0..60 {
            tick(&mut w, dt);
        }
        let speed = w.flares[0].vel.length();
        assert!(
            (speed - launch_speed * 0.22).abs() < launch_speed * 0.01,
            "after 1 s: {speed} from {launch_speed}"
        );
        assert!(speed > 0.0);
    }

    #[test]
    fn a_missile_diverts_to_the_nearest_enemy_flare_and_then_sticks_to_it() {
        let mut w = world();
        add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
        add_ship(&mut w, 2, ShipKind::Remote, v(0.0, 0.0, 900.0), Team::One);
        fire_locked(&mut w, 1).unwrap();

        // Two decoys from the victim, one nearer than the other, both inside
        // the seduction radius and both far enough off the line not to
        // detonate the missile.
        let near = add_flare(&mut w, v(60.0, 0.0, 100.0), 2);
        add_flare(&mut w, v(-140.0, 0.0, 120.0), 2);

        tick(&mut w, 1.0 / 60.0);
        assert_eq!(w.missiles[0].target, Some(MissileTarget::Flare(near)));
        assert!(!is_targeting(&w, 2), "the lock warning must clear");

        // Once seduced it does not hop, even if a nearer decoy appears.
        add_flare(&mut w, v(20.0, 0.0, 60.0), 2);
        tick(&mut w, 1.0 / 60.0);
        assert_eq!(w.missiles[0].target, Some(MissileTarget::Flare(near)));
    }

    #[test]
    fn a_missile_ignores_flares_from_its_own_owner() {
        let mut w = world();
        add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
        add_ship(&mut w, 2, ShipKind::Remote, v(0.0, 0.0, 900.0), Team::One);
        fire_locked(&mut w, 1).unwrap();
        add_flare(&mut w, v(60.0, 0.0, 100.0), 1);
        tick(&mut w, 1.0 / 60.0);
        assert_eq!(w.missiles[0].target, Some(MissileTarget::Ship(2)));
    }

    #[test]
    fn flares_outside_the_seduction_radius_do_nothing() {
        let mut w = world();
        add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
        add_ship(&mut w, 2, ShipKind::Remote, v(0.0, 0.0, 900.0), Team::One);
        fire_locked(&mut w, 1).unwrap();
        let beyond = w.rules.weapons.flare_seduction_dist + 10.0;
        add_flare(&mut w, v(beyond, 0.0, 0.0), 2);
        tick(&mut w, 1.0 / 60.0);
        assert_eq!(w.missiles[0].target, Some(MissileTarget::Ship(2)));
    }

    #[test]
    fn a_seduced_missile_detonates_on_the_decoy_and_consumes_it() {
        let mut w = world();
        add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
        add_ship(&mut w, 2, ShipKind::Remote, v(0.0, 0.0, 900.0), Team::One);
        fire_locked(&mut w, 1).unwrap();
        let key = add_flare(&mut w, v(0.0, 0.0, 120.0), 2);

        let hit = tick_until_detonation(&mut w, 1.0 / 60.0, 120)
            .expect("the missile should have reached the decoy");
        assert_eq!(hit.cause, DetonationCause::Flare { key });
        assert!(w.missiles.is_empty());
        assert!(
            w.flares.iter().all(|f| f.key != key),
            "the decoy should have been consumed"
        );
        // And the victim is untouched.
        assert_eq!(w.ship(2).unwrap().hp, w.rules.ship.max_hp);
    }

    // -----------------------------------------------------------------
    // Detonation.
    // -----------------------------------------------------------------

    #[test]
    fn a_hit_applies_missile_damage_once() {
        let mut w = world();
        add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
        add_ship(&mut w, 2, ShipKind::Remote, v(0.0, 0.0, 200.0), Team::One);
        fire_locked(&mut w, 1).unwrap();

        let hit = tick_until_detonation(&mut w, 1.0 / 60.0, 120).expect("the missile must land");
        let damage = w.rules.weapons.missile_damage;
        assert_eq!(
            hit.cause,
            DetonationCause::Ship {
                id: 2,
                damage,
                killed: false
            }
        );
        assert_eq!(w.ship(2).unwrap().hp, w.rules.ship.max_hp - damage);
        assert_eq!(w.ship(2).unwrap().hit_flash, 1.0);
        assert!(w.missiles.is_empty());
    }

    #[test]
    fn a_missile_never_hits_its_owner_a_team_mate_or_a_ship_under_spawn_protection() {
        let mut w = world();
        add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
        add_ship(&mut w, 2, ShipKind::Remote, v(0.0, 0.0, 60.0), Team::Zero);
        let hostile = add_ship(&mut w, 3, ShipKind::Remote, v(0.0, 0.0, 61.0), Team::One);
        w.ships[hostile].invuln_timer = 5.0;

        fire(&mut w, 1, None).unwrap();
        for _ in 0..30 {
            let (_, dets) = tick(&mut w, 1.0 / 60.0);
            assert!(dets.is_empty(), "a missile hit something it must not");
        }
        assert_eq!(w.ship(2).unwrap().hp, w.rules.ship.max_hp);
        assert_eq!(w.ship(3).unwrap().hp, w.rules.ship.max_hp);
    }

    #[test]
    fn a_killing_hit_reports_the_kill_and_starts_the_respawn_clock() {
        let mut w = world();
        add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
        add_ship(&mut w, 2, ShipKind::Remote, v(0.0, 0.0, 200.0), Team::One);
        w.ship_mut(2).unwrap().hp = 10;
        fire_locked(&mut w, 1).unwrap();

        let mut events = Vec::new();
        let mut dets = Vec::new();
        for _ in 0..120 {
            update(&mut w, 1.0 / 60.0, &[], &mut events, &mut dets);
            if !dets.is_empty() {
                break;
            }
        }
        assert!(matches!(
            dets[0].cause,
            DetonationCause::Ship { killed: true, .. }
        ));
        assert!(!w.ship(2).unwrap().alive);
        assert_eq!(w.ship(2).unwrap().hp, 0);
        assert_eq!(
            w.ship(2).unwrap().respawn_timer,
            w.rules.combat.respawn_delay
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, SimEvent::ShipDestroyed { id: 2, .. })));
    }

    #[test]
    fn the_boss_hitbox_override_is_honoured_where_the_js_used_its_own_radius() {
        // Bug #1. A missile passing 20 units from a boss hitbox: inside the
        // unified 28-unit boss radius, well outside `missiles.js`'s private
        // HIT_RADIUS of 6.0. No lock, so homing cannot rescue the shot.
        let mut w = world();
        add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
        let idx = add_ship(
            &mut w,
            9000,
            ShipKind::BossHitbox,
            v(20.0, 0.0, 200.0),
            Team::One,
        );
        w.ships[idx].hit_radius_override = Some(w.rules.weapons.boss_hitbox_radius);

        fire(&mut w, 1, None).unwrap();
        let hit = tick_until_detonation(&mut w, 1.0 / 60.0, 120)
            .expect("a 28-unit hitbox must catch a pass at 20 units");
        assert!(matches!(hit.cause, DetonationCause::Ship { id: 9000, .. }));

        // With the JS's radius it would have sailed past: same geometry, no
        // override.
        let mut w = world();
        add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
        add_ship(
            &mut w,
            9000,
            ShipKind::BossHitbox,
            v(20.0, 0.0, 200.0),
            Team::One,
        );
        fire(&mut w, 1, None).unwrap();
        assert!(
            tick_until_detonation(&mut w, 1.0 / 60.0, 120).is_none(),
            "a 6-unit radius cannot reach 20 units"
        );
    }

    #[test]
    fn the_missile_body_contributes_no_radius_of_its_own() {
        // Bug #2, preserved deliberately. The reach is exactly the target's
        // radius: a missile grazing at 6.0 + epsilon misses, even though the
        // body is 3.5 units long.
        assert_eq!(Rules::DEFAULT.weapons.missile_radius, 0.0);
        let reach = Rules::DEFAULT.ship.hit_radius;

        for (offset, expect_hit) in [(reach - 0.01, true), (reach + 0.01, false)] {
            let mut w = world();
            add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
            add_ship(
                &mut w,
                2,
                ShipKind::Remote,
                v(offset, 0.0, 200.0),
                Team::One,
            );
            fire(&mut w, 1, None).unwrap();
            let hit = tick_until_detonation(&mut w, 1.0 / 60.0, 120)
                .is_some_and(|d| matches!(d.cause, DetonationCause::Ship { .. }));
            assert_eq!(hit, expect_hit, "offset {offset}");
        }
    }

    #[test]
    fn the_swept_test_catches_a_grazing_hit_the_js_point_test_drops() {
        // The contact the per-frame point test loses. At the frame cap a
        // missile steps 8 units, so a target placed midway between two sampled
        // positions and 5 units off the flight line is 6.4 units from both
        // samples — outside the 6-unit hitbox at every sample — while the
        // segment between them passes 5 units from the centre, which is a hit.
        let dt = crate::rules::MAX_FRAME_DT;
        let mut w = world();
        let step = w.rules.weapons.missile_speed * dt;
        let reach = w.rules.ship.hit_radius;
        let offset = 5.0;

        // Straddle a sample pair: the missile spawns at the offset and steps by
        // `step`, so put the target half a step past one of those samples.
        let spawn_z = w.rules.weapons.missile_spawn_offset;
        let target_z = spawn_z + step * 11.0 + step * 0.5;
        let target = v(offset, 0.0, target_z);

        // Confirm the JS's per-frame point test really would miss.
        let mut nearest_sample = f64::INFINITY;
        for k in 0..40 {
            let sample = v(0.0, 0.0, spawn_z + step * f64::from(k));
            nearest_sample = nearest_sample.min(sample.distance(target));
        }
        assert!(
            nearest_sample > reach,
            "the point test must miss for this to mean anything: {nearest_sample}"
        );

        add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
        add_ship(&mut w, 2, ShipKind::Remote, target, Team::One);
        fire(&mut w, 1, None).unwrap();
        let hit =
            tick_until_detonation(&mut w, dt, 40).expect("the swept test must catch the crossing");
        assert!(matches!(hit.cause, DetonationCause::Ship { id: 2, .. }));
        // The reported position is on the target's surface, not at the end of
        // the step.
        let d = hit.pos.distance(target);
        assert!((d - reach).abs() < 1e-9, "contact at {d}");
    }

    #[test]
    fn a_missile_detonates_on_a_rock_without_damaging_it() {
        let mut w = world();
        add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
        add_rock(&mut w, 42, v(0.0, 0.0, 60.0), 10.0);
        let hp = w.asteroid(42).unwrap().hp;
        fire(&mut w, 1, None).unwrap();

        let hit = tick_until_detonation(&mut w, 1.0 / 60.0, 60)
            .expect("a missile flown into a rock detonates");
        assert_eq!(hit.cause, DetonationCause::Asteroid { id: 42 });
        // `missiles.js` never reports an asteroid hit. Preserved, and called
        // out in the module docs.
        assert_eq!(w.asteroid(42).unwrap().hp, hp);

        // It went off on the detonation shell — the rock plus the margin — not
        // on the surface.
        let d = hit.pos.distance(v(0.0, 0.0, 60.0));
        let shell = 10.0 + w.rules.weapons.missile_detonate_margin;
        assert!(
            (d - shell).abs() < 1e-9,
            "detonated at {d}, expected {shell}"
        );
    }

    #[test]
    fn caller_supplied_volumes_detonate_missiles_and_name_themselves() {
        let mut w = world();
        add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
        fire(&mut w, 1, None).unwrap();

        // The kind of hull the campaign might hand in: a box across the flight
        // path, plus a sphere nowhere near it.
        let hull = [
            Volume::Sphere(Sphere::new(v(500.0, 0.0, 0.0), 10.0)),
            Volume::Aabb(Aabb::new(v(0.0, 0.0, 120.0), v(60.0, 20.0, 30.0))),
        ];
        let mut events = Vec::new();
        let mut dets = Vec::new();
        for _ in 0..60 {
            update(&mut w, 1.0 / 60.0, &hull, &mut events, &mut dets);
            if !dets.is_empty() {
                break;
            }
        }
        assert_eq!(dets.len(), 1);
        assert_eq!(dets[0].cause, DetonationCause::Volume { index: 1 });
        // Contact is on the near face of the box.
        assert!((dets[0].pos.z - 90.0).abs() < 1e-9, "{:?}", dets[0].pos);
    }

    #[test]
    fn a_missile_expires_after_its_rated_life() {
        let mut w = world();
        add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
        fire(&mut w, 1, None).unwrap();

        let dt = 1.0 / 60.0;
        let mut elapsed = 0.0;
        let mut cause = None;
        while cause.is_none() {
            let (_, dets) = tick(&mut w, dt);
            elapsed += dt;
            cause = dets.first().map(|d| d.cause);
            assert!(elapsed < 20.0, "the missile never expired");
        }
        assert_eq!(cause, Some(DetonationCause::Expired));
        assert!(
            (elapsed - w.rules.weapons.missile_life).abs() < dt * 1.5,
            "expired at {elapsed}"
        );
        assert!(w.missiles.is_empty());
    }

    #[test]
    fn the_earliest_contact_wins_when_two_targets_share_a_step() {
        // A flare and a ship on the same line. The JS checks flares first and
        // would agree in one of these cases; the point is that the answer comes
        // from the geometry, so it also holds when the ship is nearer.
        for flare_first in [true, false] {
            let mut w = world();
            add_ship(&mut w, 1, ShipKind::Local, Vec3::ZERO, Team::Zero);
            let (flare_z, ship_z) = if flare_first {
                (100.0, 130.0)
            } else {
                (130.0, 100.0)
            };
            add_ship(&mut w, 2, ShipKind::Remote, v(0.0, 0.0, ship_z), Team::One);
            let key = add_flare(&mut w, v(0.0, 0.0, flare_z), 2);
            fire(&mut w, 1, None).unwrap();

            let hit = tick_until_detonation(&mut w, crate::rules::MAX_FRAME_DT, 60)
                .expect("something must be hit");
            if flare_first {
                assert_eq!(hit.cause, DetonationCause::Flare { key });
            } else {
                assert!(matches!(hit.cause, DetonationCause::Ship { id: 2, .. }));
            }
        }
    }

    // -----------------------------------------------------------------
    // Determinism.
    // -----------------------------------------------------------------

    #[test]
    fn a_whole_engagement_replays_bit_for_bit() {
        let run = || {
            let mut w = World::new(0xBEEF, Rules::DEFAULT, Mode::Skirmish, MapKind::Space);
            add_ship(&mut w, 1, ShipKind::Local, v(0.0, 0.0, -400.0), Team::Zero);
            add_ship(
                &mut w,
                2,
                ShipKind::Remote,
                v(120.0, 30.0, -150.0),
                Team::One,
            );
            add_rock(&mut w, 1, v(20.0, 0.0, -300.0), 14.0);
            add_rock(&mut w, 2, v(-40.0, 10.0, -250.0), 22.0);
            fire_locked(&mut w, 1).unwrap();
            let mut events = Vec::new();
            deploy_flares(&mut w, 2, &mut events);
            let mut dets = Vec::new();
            let mut trace = Vec::new();
            for _ in 0..300 {
                update(&mut w, 1.0 / 60.0, &[], &mut events, &mut dets);
                for m in &w.missiles {
                    trace.push(m.pos.to_array());
                }
                for f in &w.flares {
                    trace.push(f.pos.to_array());
                }
            }
            (trace, dets, w.ship(2).unwrap().hp)
        };
        let (a_trace, a_dets, a_hp) = run();
        let (b_trace, b_dets, b_hp) = run();
        assert_eq!(a_trace.len(), b_trace.len());
        for (x, y) in a_trace.iter().zip(&b_trace) {
            assert_eq!(x.map(f64::to_bits), y.map(f64::to_bits));
        }
        assert_eq!(a_dets, b_dets);
        assert_eq!(a_hp, b_hp);
        assert!(!a_trace.is_empty());
    }
}
