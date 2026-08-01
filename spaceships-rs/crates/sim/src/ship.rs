//! Ship kinematics, state transitions, and ship-vs-world collision.
//!
//! This is the flight model: what a throttle, a stick deflection, a boost and a
//! brake do to a [`Ship`]'s position, velocity and orientation, what happens
//! when that ship flies into a rock, and what happens when it runs out of hit
//! points.
//!
//! # What lives here
//!
//! | Function | Replaces |
//! |---|---|
//! | [`integrate`] | the flight block of `update()`, `main.js:1200`–`:1330` |
//! | [`tick_timers`] | the invuln/respawn/regen clocks, `main.js:1532`, `:1736`, `:1962` |
//! | [`apply_damage`] | `applyPlayerDamageLocal` (`main.js:3230`) and `dealSelfDamage` (`main.js:2168`) |
//! | [`kill`] | `killSelf` (`main.js:748`) |
//! | [`respawn`] | `reviveSelf` (`main.js:755`) |
//! | [`resolve_world_collisions`] | `resolveCollisions` (`main.js:2192`) and `resolveMothershipCollisions` (`main.js:2160`) |
//! | [`terrain_height`] | `getTerrainHeight` (`terrain.js:35`) |
//!
//! Weapons, bots, missiles, asteroid generation and campaign scripting are
//! deliberately absent — they are other modules.
//!
//! # Ordering is behaviour
//!
//! [`integrate`] performs the same operations in the same order as the JS. That
//! is not stylistic: `brakeBoostTimer` is decremented *before* it is read to
//! decide the velocity blend, and `brakeCharge` is advanced *after* the position
//! update but *before* the overcharge test. Reordering any of it changes the
//! feel by one frame everywhere, and by a whole boost at the edges.
//!
//! # Determinism, and the transcendental surface
//!
//! The crate docs warn that `sin`, `cos`, `exp` and `powf` are not guaranteed
//! bit-identical across platforms or libm versions. The flight model cannot
//! avoid them — the JS is written in terms of `THREE.MathUtils.damp` (an `exp`),
//! `Math.pow(0.001, ..)` blends, `setFromAxisAngle` (a `sin`/`cos` pair) and a
//! `Math.pow(x, 1.6)` response curve, and the terrain heightfield is seven sine
//! octaves.
//!
//! Every one of those calls is funnelled through the handful of functions in
//! the "transcendental surface" section below, and **nothing else in this module
//! calls a transcendental function directly**. If server-versus-WASM agreement
//! ever needs hand-rolled implementations, those functions plus
//! [`raw_terrain_height`] are the complete list of things to replace. Everything
//! else here is `+ - * /` and `sqrt`, which IEEE-754 requires to be exact.
//!
//! # Sweeping, and where it is and is not applied
//!
//! The JS resolves ship collision with a point-in-sphere test against the ship's
//! end-of-frame position (`main.js:2196`). At the largest frame delta the game
//! allows ([`crate::rules::MAX_FRAME_DT`], 0.05 s) a boosting ship with a
//! brake-release bonus covers about 9.3 units, which is wider than the 8-unit
//! combined reach of the hull and the smallest asteroid tier — so a fast ship
//! can cross a small rock without ever being tested against it.
//!
//! The fix here is deliberately conservative. The static pass runs first and is
//! arithmetically identical to the JS, so the overwhelmingly common case does
//! not change at all. Only when that pass finds *no* contact does
//! [`resolve_world_collisions`] sweep the step's segment (using
//! [`crate::collision`]) looking for a body the ship passed clean through, and
//! resolve against the earliest one. See [`resolve_world_collisions`] for the
//! one case this still does not catch.

use crate::collision::{swept_sphere_aabb, swept_sphere_sphere, Aabb, Sphere};
use crate::math::Vec3;
use crate::rng::Rng;
use crate::rules::Rules;
use crate::world::{Asteroid, BoxVolume, Input, MapKind, Mode, Obstacle, Quat, Ship};

// ---------------------------------------------------------------------------
// Tunables that have no home in `rules`
// ---------------------------------------------------------------------------
//
// Every game *rule* belongs in `crate::rules`. The constants below are not
// rules: three are input-layer thresholds the JS spells as bare literals, one is
// a cosmetic decay rate, and the rest are numerical-method parameters for the
// terrain sweep. They are collected here, named and cited, so that moving any of
// them into `rules` later is a mechanical edit.

/// Gamepad throttle-axis deadzone. `main.js:1211`/`:1213` (`> 0.01`).
///
/// Not [`crate::rules::ShipRules::steer_deadzone`], which is 0.05 and applies to
/// the aiming axes only.
const THROTTLE_AXIS_DEADZONE: f64 = 0.01;

/// Deflection below which a released arrow key stops overriding mouse steering.
/// `main.js:1240` (`Math.abs(arrowKx) > 0.01`).
const ARROW_RELEASE_EPSILON: f64 = 0.01;

/// Speed below which a drifting ship's velocity is left alone rather than being
/// pulled onto the nose. `main.js:1279` (`speed > 0.001`). Guards a division.
const DRIFT_GRIP_MIN_SPEED: f64 = 0.001;

/// Squared separation below which a push-out is skipped as degenerate.
/// `main.js:2201` and `main.js:2146` (`distSq > 0.0001`). Two bodies at exactly
/// the same point have no separating normal.
const CONTACT_MIN_DIST_SQ: f64 = 0.0001;

/// Per-second decay of the cosmetic damage flash. `asteroids.js:101`, which
/// applies the same rate to rocks.
const HIT_FLASH_DECAY_RATE: f64 = 4.0;

/// Horizontal spacing of the terrain kill-plane samples along one step, in world
/// units.
///
/// A numerical-method parameter, not a game rule: it trades work against how
/// narrow a ridge the sweep can catch. Set at roughly the ship's collision
/// radius, which keeps the sample gap below the ship's own size.
const TERRAIN_SWEEP_SPACING: f64 = 4.0;

/// Hard cap on terrain samples per step, so a teleport cannot turn one tick into
/// an unbounded loop over a heightfield that costs eleven `sin`/`cos` calls per
/// sample.
const TERRAIN_SWEEP_MAX_SAMPLES: u32 = 8;

/// Steepness of the airfield flattening ramp. `terrain.js:18`
/// (`smoothstep(Math.min(tx, tz) * 2.5)`).
const AIRFIELD_BLEND_SHARPNESS: f64 = 2.5;

// ---------------------------------------------------------------------------
// The transcendental surface
// ---------------------------------------------------------------------------

/// `THREE.MathUtils.damp(x, y, lambda, dt)`.
///
/// An exponential approach: the value covers `1 - e^(-lambda * dt)` of the
/// remaining gap each step, which makes the result independent of the step size
/// in a way that a plain `lerp(x, y, k * dt)` is not.
///
/// The interpolation is written `(1 - t) * x + t * y`, matching
/// `THREE.MathUtils.lerp`, rather than the algebraically equal
/// `x + (y - x) * t` used by `THREE.Vector3.lerp`. The two differ in the last
/// bit and the JS uses each in a different place; this is the scalar one.
///
/// **Transcendental.** Calls `exp`.
#[inline]
#[must_use]
fn damp(x: f64, y: f64, lambda: f64, dt: f64) -> f64 {
    let t = 1.0 - (-lambda * dt).exp();
    (1.0 - t) * x + t * y
}

/// The `1 - Math.pow(0.001, dt * rate / 6)` blend factor the JS uses for every
/// velocity approach (`main.js:1282`, `:1294`).
///
/// Algebraically this is [`damp`]'s factor with `lambda = rate * ln(1000) / 6`,
/// but it is kept in the JS's own form so the two can be diffed literally.
///
/// **Transcendental.** Calls `powf`.
#[inline]
#[must_use]
fn pow_blend(rate: f64, dt: f64) -> f64 {
    1.0 - 0.001f64.powf(dt * rate / 6.0)
}

/// Per-step retention for a per-second drag factor: `drag^dt`. `main.js:1285`
/// (`shipVelocity.multiplyScalar(Math.pow(drag, dt))`).
///
/// **Transcendental.** Calls `powf`.
#[inline]
#[must_use]
fn drag_factor(drag: f64, dt: f64) -> f64 {
    drag.powf(dt)
}

/// The steering response curve, `sign(x) * |x|^exponent`. `main.js:1227`.
///
/// `sign` follows `Math.sign`, which returns 0 for 0 — Rust's `f64::signum`
/// returns 1 for `+0.0`, which would make a centred stick only accidentally
/// correct.
///
/// **Transcendental.** Calls `powf`.
#[inline]
#[must_use]
fn steer_curve(x: f64, exponent: f64) -> f64 {
    js_sign(x) * x.abs().powf(exponent)
}

/// `(sin(a), cos(a))`. The only place quaternion construction reads either.
///
/// **Transcendental.** Calls `sin` and `cos`.
#[inline]
#[must_use]
fn sin_cos(a: f64) -> (f64, f64) {
    (a.sin(), a.cos())
}

// ---------------------------------------------------------------------------
// Small numeric helpers
// ---------------------------------------------------------------------------

/// `Math.sign`: -1, 0 or +1, with 0 for both zeroes.
#[inline]
#[must_use]
fn js_sign(x: f64) -> f64 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    }
}

/// `Math.sign(x) || 1`: the JS idiom at `main.js:2136`, where a zero offset from
/// a box face still needs a direction to push along.
#[inline]
#[must_use]
fn sign_or_positive(x: f64) -> f64 {
    if x < 0.0 {
        -1.0
    } else {
        1.0
    }
}

/// `t * t * (3 - 2t)` on a clamped `t`. `terrain.js:9`.
#[inline]
#[must_use]
fn smoothstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// ---------------------------------------------------------------------------
// Quaternions
// ---------------------------------------------------------------------------
//
// [`crate::world::Quat`] is storage only, and its docs say rotation algebra
// "belongs beside `Vec3` in `math`". It is not there yet, and `math` is not this
// module's to edit, so the operations the flight model needs live here. They are
// `pub` because missiles, bots and the campaign turrets will all want them;
// moving them to `math` later is a re-export away.

/// Composes two rotations: the rotation `b` applied *in the frame of* `a`.
///
/// Matches `THREE.Quaternion.multiplyQuaternions(a, b)` term for term, which is
/// what `ship.quaternion.multiply(q)` performs at `main.js:1252`.
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
/// **Transcendental** by way of [`sin_cos`].
#[inline]
#[must_use]
pub fn quat_from_axis_angle(axis: Vec3, angle: f64) -> Quat {
    let (s, c) = sin_cos(angle * 0.5);
    Quat::new(axis.x * s, axis.y * s, axis.z * s, c)
}

/// Renormalizes a quaternion, returning the identity for a degenerate one.
///
/// `THREE.Quaternion.normalize`, which the flight model calls every frame
/// (`main.js:1255`) so integration drift cannot denormalize the pose.
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
// The flight model
// ---------------------------------------------------------------------------

/// What one [`integrate`] step did, beyond the state it wrote into the [`Ship`].
///
/// Everything here is either needed by the caller to finish the tick
/// ([`Self::prev_pos`] feeds [`resolve_world_collisions`],
/// [`Self::self_damage`] has to be routed through [`apply_damage`]) or is a
/// derived flag the renderer and HUD would otherwise re-derive and get subtly
/// wrong.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FlightStep {
    /// Where the ship was before the step. The collision sweep needs the
    /// segment, not just its endpoint.
    pub prev_pos: Vec3,
    /// Whether the brake/drift control was held *and* the ship was alive.
    /// `main.js:1200`.
    pub braking: bool,
    /// Whether either kind of boost was active, for the exhaust trail.
    /// `main.js:1264`.
    pub boosting: bool,
    /// Whether the held boost (Shift) was active — the one that drains the meter
    /// and multiplies top speed. `main.js:1262`.
    pub shift_boost: bool,
    /// Whether a brake-release boost was active. `main.js:1263`.
    pub brake_release_boost: bool,
    /// Steering after deadzone, response curve and arrow-key ramp: the values
    /// the JS stores in `camTel.steerX`/`steerY` for the camera lean.
    pub steer: [f64; 2],
    /// Whole points of brake-overcharge self-damage that came due this step.
    ///
    /// Returned rather than applied, because the damage rules (invulnerability,
    /// death, respawn) are [`apply_damage`]'s job and the caller may need to
    /// report the same points to a server. `main.js:1315`–`:1323`.
    pub self_damage: i32,
}

/// Advances one ship's flight model by `dt`.
///
/// Ports the block from `main.js:1200` (`const braking = ...`) to `main.js:1330`
/// (the end of the overcharge test) in the same order, including the parts that
/// deliberately keep running while the ship is dead: the brake-release timer
/// counts down and the boost meter recharges whether or not you are alive.
///
/// `dt` should be a fixed step ([`crate::world::TICK_DT`]). It is never read
/// from a clock; see the crate docs.
///
/// # What this does not do
///
/// - **No aim assist.** `main.js:1256` calls `applyAimAssist` between the
///   rotation update and the boost block; that is a weapons-adjacent module and
///   it composes by premultiplying [`Ship::quat`] after this returns.
/// - **No collision.** Call [`resolve_world_collisions`] with
///   [`FlightStep::prev_pos`] afterwards, which is where the JS does it
///   (`main.js:1689`).
/// - **No damage.** [`FlightStep::self_damage`] is returned, not applied.
///
/// # Ship kinds
///
/// This is the *player* flight model. [`crate::world::ShipKind::Remote`] ships
/// are interpolated from network state and
/// [`crate::world::ShipKind::BossHitbox`] entries are slaved to the capital
/// ship, so neither belongs here. Bots in the JS use a different model entirely
/// (`bot.js`); if the port ever unifies them, a bot only has to synthesize an
/// [`Input`].
pub fn integrate(ship: &mut Ship, input: &Input, rules: &Rules, mode: Mode, dt: f64) -> FlightStep {
    let s = &rules.ship;
    let prev_pos = ship.pos;
    let alive = ship.alive;

    // `main.js:1200`. Braking is gated on being alive, which is why no later
    // `braking` test needs its own liveness check.
    let braking = alive && input.braking;

    let mut steer = [0.0, 0.0];
    if alive {
        // -- Throttle. `main.js:1204`–`:1217`. --------------------------------
        let mut target = ship.target_throttle;
        if let Some(over) = input.throttle_override {
            // The touch HUD commands an absolute throttle, and the accumulated
            // wheel is consumed and discarded (`main.js:1207`).
            target = over * s.max_throttle;
        } else {
            target += input.throttle_notches * s.throttle_step;
            let axis = input.throttle_axis;
            if axis > THROTTLE_AXIS_DEADZONE {
                target += s.key_throttle_rate * dt * axis;
            } else if axis < -THROTTLE_AXIS_DEADZONE {
                target -= s.key_throttle_rate * dt * -axis;
            }
        }
        ship.target_throttle = target.clamp(0.0, s.max_throttle);
        ship.throttle = damp(
            ship.throttle,
            ship.target_throttle,
            s.throttle_damp_rate,
            dt,
        );

        // -- Steering. `main.js:1218`–`:1242`. --------------------------------
        // Free-look suppresses steering entirely. The JS splits this in two —
        // the right mouse button zeroes the mouse axes (`main.js:1218`) while
        // `gp.freeLook` suppresses the gamepad override (`main.js:1220`) — but
        // `Input` presents one merged aiming axis, so there is one flag.
        let (mut sx, mut sy) = if input.free_look {
            (0.0, 0.0)
        } else {
            (input.steer_x, input.steer_y)
        };
        if sx.abs() < s.steer_deadzone {
            sx = 0.0;
        }
        if sy.abs() < s.steer_deadzone {
            sy = 0.0;
        }
        sx = steer_curve(sx, s.steer_curve_exponent);
        sy = steer_curve(sy, s.steer_curve_exponent);

        // Arrow keys ramp toward full deflection instead of snapping, so a
        // keyboard pilot is not strictly worse than a mouse one. Q slows the
        // ramp for fine aim; releasing always ramps back four times faster.
        let up_rate = if input.arrow_fine {
            s.arrow_ramp_up_rate_fine
        } else {
            s.arrow_ramp_up_rate
        };
        let rate_x = if input.arrow_x != 0.0 {
            up_rate
        } else {
            s.arrow_ramp_down_rate
        };
        let rate_y = if input.arrow_y != 0.0 {
            up_rate
        } else {
            s.arrow_ramp_down_rate
        };
        ship.arrow_kx = damp(ship.arrow_kx, input.arrow_x, rate_x, dt);
        ship.arrow_ky = damp(ship.arrow_ky, input.arrow_y, rate_y, dt);
        if input.arrow_x != 0.0 || ship.arrow_kx.abs() > ARROW_RELEASE_EPSILON {
            sx = ship.arrow_kx;
        }
        if input.arrow_y != 0.0 || ship.arrow_ky.abs() > ARROW_RELEASE_EPSILON {
            sy = ship.arrow_ky;
        }
        steer = [sx, sy];

        // -- Rotation. `main.js:1243`–`:1255`. --------------------------------
        // Braking is a handling mode as much as a brake: it buys pitch and yaw
        // authority (and roll, which shares the pitch multiplier — see below).
        let pitch_mult = if braking { s.brake_pitch_mult } else { 1.0 };
        let yaw_mult = if braking { s.brake_yaw_mult } else { 1.0 };
        // Nose-up is the evasive input, so it is the faster one.
        let pitch_rate = if sy < 0.0 {
            s.pitch_rate * s.pitch_up_boost
        } else {
            s.pitch_rate
        } * pitch_mult;
        let pitch = sy * pitch_rate * dt;
        let yaw = -sx * s.yaw_rate * yaw_mult * dt;
        // `main.js:1249`–`:1251` scales roll by `pitchMult`, not by a roll
        // multiplier of its own. Reproduced rather than tidied: there is no
        // BRAKE_ROLL_MULT to reach for, and inventing one would change handling.
        let roll = input.roll * s.roll_rate * pitch_mult * dt;

        // Post-multiplied, so each rotation is about the ship's *own* axes.
        let mut q = ship.quat;
        if pitch != 0.0 {
            q = quat_mul(q, quat_from_axis_angle(Vec3::X, pitch));
        }
        if yaw != 0.0 {
            q = quat_mul(q, quat_from_axis_angle(Vec3::Y, yaw));
        }
        if roll != 0.0 {
            q = quat_mul(q, quat_from_axis_angle(Vec3::Z, roll));
        }
        ship.quat = quat_normalize(q);
    }

    // -- Boost. `main.js:1260`–`:1275`. ---------------------------------------
    // The release-boost timer runs regardless of liveness; the flags that read
    // it are gated instead.
    if ship.brake_boost_timer > 0.0 {
        ship.brake_boost_timer = (ship.brake_boost_timer - dt).max(0.0);
    }
    let want_shift = input.boost;
    // Boosting and braking are mutually exclusive: you cannot drift on the gas.
    let shift_boost = alive && !braking && want_shift && ship.boost_meter > 0.0;
    let brake_release_boost = alive && ship.brake_boost_timer > 0.0;
    let boosting = alive && (shift_boost || brake_release_boost);

    ship.boost_idle += dt;
    if shift_boost {
        ship.boost_meter = (ship.boost_meter - s.boost_drain * dt).max(0.0);
        ship.boost_idle = 0.0;
    } else if want_shift {
        // Holding Shift on an empty meter still pins the idle clock, so the
        // meter does not refill under a held key.
        ship.boost_idle = 0.0;
    }
    if ship.boost_meter < s.max_boost && ship.boost_idle >= s.boost_regen_delay {
        ship.boost_meter = (ship.boost_meter + s.boost_recharge * dt).min(s.max_boost);
    }

    // -- Velocity and position. `main.js:1276`–`:1296`. -----------------------
    if alive {
        if braking {
            // Drifting: the nose and the velocity come apart. Grip pulls the
            // velocity *direction* back onto the nose without changing its
            // magnitude, then drag removes speed. Holding S instead of coasting
            // swaps the gentle drag for the hard stop.
            let speed = ship.vel.length();
            if speed > DRIFT_GRIP_MIN_SPEED && s.drift_grip > 0.0 {
                let desired = forward(ship.quat) * speed;
                ship.vel = ship.vel.lerp(desired, pow_blend(s.drift_grip, dt));
            }
            let drag = if input.hard_brake {
                s.drift_brake
            } else {
                s.drift_drag
            };
            ship.vel *= drag_factor(drag, dt);
        } else {
            // Under thrust the velocity chases `forward * throttle`, so the ship
            // has inertia but always ends up pointing where it is going.
            let speed_mult = if shift_boost { s.boost_factor } else { 1.0 };
            let fwd = forward(ship.quat);
            let mut target = fwd * (ship.throttle * speed_mult);
            if brake_release_boost {
                // A flat bonus along the nose, scaled by the charge the brake
                // was released at.
                target = target.add_scaled(fwd, s.brake_boost_bonus_max * ship.brake_boost_charge);
            }
            // The release boost blends in slowly on purpose: it should read as a
            // slingshot rather than a step change.
            let blend = if brake_release_boost {
                s.velocity_blend_release
            } else {
                s.velocity_blend
            };
            ship.vel = ship.vel.lerp(target, pow_blend(blend, dt));
        }
        ship.pos = ship.pos.add_scaled(ship.vel, dt);
    }

    // -- Brake charge. `main.js:1298`–`:1311`. --------------------------------
    // The position update above already happened: the charge accumulated here
    // arms *next* frame's release.
    if braking {
        ship.brake_charge = (ship.brake_charge + dt / s.brake_full_time).min(1.0);
    } else if ship.prev_braking && alive {
        if ship.brake_charge >= s.brake_boost_min {
            ship.brake_boost_timer = ship.brake_charge * s.brake_boost_duration_max;
            ship.brake_boost_charge = ship.brake_charge;
        }
        ship.brake_charge = 0.0;
    } else if !alive {
        ship.brake_charge = 0.0;
        ship.brake_boost_timer = 0.0;
        ship.brake_boost_charge = 0.0;
    }
    ship.prev_braking = braking;

    // -- Overcharge. `main.js:1312`–`:1330`. ----------------------------------
    // Hold a full brake charge too long and it starts eating the hull. Damage
    // accumulates fractionally and is spent in whole points, so the stream stays
    // integral whatever `dt` is.
    let mut self_damage = 0;
    if braking && ship.brake_charge >= 1.0 && alive {
        ship.brake_overcharge_time += dt;
        if ship.brake_overcharge_time > s.brake_overcharge_damage_delay
            && mode.has_collision_damage()
        {
            ship.self_damage_accum += s.brake_overcharge_dps * dt;
            while ship.self_damage_accum >= 1.0 {
                ship.self_damage_accum -= 1.0;
                self_damage += 1;
            }
        }
    } else {
        ship.brake_overcharge_time = 0.0;
        ship.self_damage_accum = 0.0;
    }

    FlightStep {
        prev_pos,
        braking,
        boosting,
        shift_boost,
        brake_release_boost,
        steer,
        self_damage,
    }
}

// ---------------------------------------------------------------------------
// Clocks
// ---------------------------------------------------------------------------

/// What [`tick_timers`] found when it advanced a ship's clocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TimerStep {
    /// The respawn countdown reached zero on this tick, so the caller should
    /// pick a spawn pose and call [`respawn`].
    ///
    /// Reported rather than acted on because *where* a ship respawns depends on
    /// the mode: a team anchor in skirmish, the campaign checkpoint in the
    /// campaign, the start line in trials, and the server's chosen pose in
    /// multiplayer (`main.js:3306`–`:3341`).
    pub respawn_due: bool,
    /// Hit points restored by regeneration on this tick.
    pub regenerated: i32,
    /// Spawn invulnerability expired on this tick.
    pub invuln_expired: bool,
}

/// Advances a ship's timers: spawn invulnerability, the respawn countdown,
/// health regeneration, and the cosmetic damage flash.
///
/// Ports `main.js:1532`–`:1543` (regen), `:1736`–`:1738` (respawn) and
/// `:1962`–`:1964` (invulnerability). The JS scatters these across the frame and
/// gives only the local player a respawn countdown and an invulnerability timer;
/// here every ship gets both, which is the point of
/// [`crate::rules::CombatRules::spawn_invuln`].
///
/// # A quirk preserved on purpose
///
/// Regeneration restores at most one interval's worth of hit points per call,
/// even when `dt` exceeds
/// [`crate::rules::CombatRules::health_regen_interval`] — `main.js:1537` is an
/// `if`, not a `while`. At the 0.1 s interval and the 0.05 s frame cap that is
/// unreachable in practice, but it is reproduced rather than silently corrected:
/// turning it into a `while` would make regeneration faster at low frame rates,
/// which is a balance change.
pub fn tick_timers(ship: &mut Ship, rules: &Rules, dt: f64) -> TimerStep {
    let mut out = TimerStep::default();

    if ship.invuln_timer > 0.0 {
        ship.invuln_timer = (ship.invuln_timer - dt).max(0.0);
        out.invuln_expired = ship.invuln_timer == 0.0;
    }

    if !ship.alive && ship.respawn_timer > 0.0 {
        ship.respawn_timer -= dt;
        if ship.respawn_timer <= 0.0 {
            ship.respawn_timer = 0.0;
            out.respawn_due = true;
        }
    }

    if ship.alive {
        // Both clocks must clear: taking a hit *or* firing a shot suppresses
        // regeneration, so nobody heals through a firefight.
        ship.health_idle_damage += dt;
        ship.health_idle_shot += dt;
        let c = &rules.combat;
        if ship.health_idle_damage >= c.health_regen_delay
            && ship.health_idle_shot >= c.health_regen_delay
            && ship.hp < rules.ship.max_hp
        {
            ship.health_regen_tick += dt;
            if ship.health_regen_tick >= c.health_regen_interval {
                ship.health_regen_tick -= c.health_regen_interval;
                let before = ship.hp;
                ship.hp = (ship.hp + c.health_regen_amount).min(rules.ship.max_hp);
                out.regenerated = ship.hp - before;
            }
        } else {
            ship.health_regen_tick = 0.0;
        }
    }

    if ship.hit_flash > 0.0 {
        ship.hit_flash = (ship.hit_flash - HIT_FLASH_DECAY_RATE * dt).max(0.0);
    }

    out
}

// ---------------------------------------------------------------------------
// Damage, death, respawn
// ---------------------------------------------------------------------------

/// The result of an attempt to damage a ship.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DamageResult {
    /// Hit points actually removed. Zero when the hit was rejected, and less
    /// than the requested amount when the ship had fewer points left than the
    /// hit was worth.
    pub applied: i32,
    /// Hit points after.
    pub hp: i32,
    /// The ship was alive before and is dead now.
    pub killed: bool,
    /// The hit was refused because the ship was dead or inside its spawn window.
    pub rejected: bool,
}

/// Applies `amount` damage to a ship, respecting the universal spawn-protection
/// gate.
///
/// Ports `applyPlayerDamageLocal` (`main.js:3230`). Every damage path in the
/// game — weapon hits, collision, terrain, brake overcharge, the server's
/// authoritative `hp` message — should arrive here, because this is where
/// [`Ship::is_damageable`] is consulted. The JS has that check in two places and
/// missing from a third: `applyHitToBot` (`main.js:3201`) tests only `alive`, so
/// a freshly respawned solo bot can be killed on its spawn anchor while the
/// player cannot.
///
/// Death itself is [`kill`], which this calls. The caller still owns the
/// consequences that are not the ship's own state — scoreboard, kill feed,
/// campaign lives, team counters — because none of those live in a [`Ship`].
///
/// A non-positive `amount` is a no-op rather than a heal.
pub fn apply_damage(ship: &mut Ship, amount: i32, rules: &Rules, mode: Mode) -> DamageResult {
    if !ship.is_damageable() {
        return DamageResult {
            applied: 0,
            hp: ship.hp,
            killed: false,
            rejected: true,
        };
    }
    if amount <= 0 {
        return DamageResult {
            applied: 0,
            hp: ship.hp,
            killed: false,
            rejected: false,
        };
    }

    // Taking a hit restarts the regeneration clock; firing restarts the other
    // one (`main.js:1526`), which weapons owns.
    ship.health_idle_damage = 0.0;
    ship.health_regen_tick = 0.0;
    ship.hit_flash = 1.0;

    let before = ship.hp;
    ship.hp = (ship.hp - amount).max(0);
    let applied = before - ship.hp;

    let killed = ship.hp <= 0;
    if killed {
        kill(ship, rules, mode);
    }

    DamageResult {
        applied,
        hp: ship.hp,
        killed,
        rejected: false,
    }
}

/// Kills a ship and starts its respawn countdown.
///
/// Ports `killSelf` (`main.js:748`) plus the respawn-delay selection at
/// `main.js:3253`/`:3258`. Idempotent: killing a dead ship does nothing, which
/// is what the JS's `if (!myAlive) return` guard buys.
///
/// The velocity is zeroed so a dead hull does not drift on into the rocks; the
/// `touching_*` state is frozen while dead, because collision does not run on a
/// corpse, and a moving one would re-trigger it all on respawn.
///
/// Campaign deaths use the shorter
/// [`crate::rules::CombatRules::campaign_respawn_delay`] because a warp-in
/// effect covers the gap. Running out of lives is the caller's business: it sets
/// `respawn_timer` back to zero and ends the mission (`main.js:3243`).
pub fn kill(ship: &mut Ship, rules: &Rules, mode: Mode) {
    if !ship.alive {
        return;
    }
    ship.alive = false;
    ship.hp = 0;
    ship.vel = Vec3::ZERO;
    ship.respawn_timer = respawn_delay(rules, mode);
}

/// How long a ship stays dead in this mode.
#[must_use]
pub fn respawn_delay(rules: &Rules, mode: Mode) -> f64 {
    match mode {
        Mode::Campaign(_) => rules.combat.campaign_respawn_delay,
        _ => rules.combat.respawn_delay,
    }
}

/// Returns a ship to life at `pos` facing `quat`.
///
/// Ports `reviveSelf` (`main.js:755`) and the parts of `revivePlayerLocal`
/// (`main.js:3306`) that touch the ship rather than the HUD: full hit points, a
/// full load-out, zeroed velocity and throttle, and a fresh spawn-protection
/// window.
///
/// Choosing `pos` and `quat` is the caller's job — see
/// [`TimerStep::respawn_due`] for why.
///
/// Campaign respawns come back hurt; apply [`campaign_respawn_hp`] afterwards.
/// The JS does exactly that, overwriting `myHp` two lines after `reviveSelf`
/// (`main.js:3343`).
pub fn respawn(ship: &mut Ship, pos: Vec3, quat: Quat, rules: &Rules) {
    ship.alive = true;
    ship.hp = rules.ship.max_hp;
    ship.respawn_timer = 0.0;
    ship.invuln_timer = rules.combat.spawn_invuln;

    ship.pos = pos;
    ship.quat = quat;
    ship.vel = Vec3::ZERO;
    ship.throttle = 0.0;
    ship.target_throttle = 0.0;
    ship.arrow_kx = 0.0;
    ship.arrow_ky = 0.0;

    ship.missiles_left = rules.weapons.missile_max;
    ship.flares_left = rules.weapons.flare_max;

    // The JS leaves these stale across a death, because the code that clears
    // them is gated on being alive; they happen to be recomputed on the first
    // live frame. Clearing them here removes the dependency on that accident.
    ship.touching_asteroids.clear();
    ship.touching_moon = false;
    ship.touching_ground = false;

    ship.brake_charge = 0.0;
    ship.brake_boost_timer = 0.0;
    ship.brake_boost_charge = 0.0;
    ship.brake_overcharge_time = 0.0;
    ship.self_damage_accum = 0.0;
    ship.prev_braking = false;

    ship.health_idle_damage = rules.combat.health_regen_delay;
    ship.health_idle_shot = rules.combat.health_regen_delay;
    ship.health_regen_tick = 0.0;
    ship.hit_flash = 0.0;
}

/// Hit points a campaign respawn comes back with. `main.js:3343`
/// (`Math.floor(SHIP_MAX_HP * 0.55)`).
#[must_use]
pub fn campaign_respawn_hp(rules: &Rules) -> i32 {
    let hp = (f64::from(rules.ship.max_hp) * rules.campaign.respawn_hp_fraction).floor();
    (hp as i32).clamp(1, rules.ship.max_hp)
}

// ---------------------------------------------------------------------------
// Terrain
// ---------------------------------------------------------------------------

/// Terrain surface height at a world `(x, z)`.
///
/// Ports `getTerrainHeight` (`terrain.js:35`), including the flat-topped
/// airfields and the hard zero outside
/// [`crate::rules::WorldRules::terrain_size`].
///
/// # Where the numbers come from
///
/// The seven-octave sum in [`raw_terrain_height`] is `rawHeight`
/// (`terrain.js:22`), transcribed literally. Its frequencies, phases and
/// amplitudes are **not** in [`crate::rules`] — there is no field for them — so
/// they are written there with the JS line beside each. They are world geometry
/// rather than balance (changing one is a different map, not a rebalance), so
/// this is a defensible home, but if a heightfield ever has to be swapped per
/// map they belong in `WorldRules`.
///
/// The airfield rectangles *are* derived from the rules
/// ([`crate::rules::WorldRules::airfield_z`] and `airfield_half`), which the JS
/// duplicates as a third copy at `terrain.js:5`.
///
/// **Transcendental**, heavily: eleven `sin`/`cos` calls. This is the most
/// platform-sensitive function in the module. It only ever feeds a `<`
/// comparison against a ship's altitude, so a last-bit disagreement can change
/// a kill/no-kill outcome only for a ship sitting exactly on the kill plane.
#[must_use]
pub fn terrain_height(x: f64, z: f64, rules: &Rules) -> f64 {
    let half = rules.world.terrain_size * 0.5;
    if x.abs() > half || z.abs() > half {
        return 0.0;
    }
    raw_terrain_height(x, z) * (1.0 - airfield_blend(x, z, rules))
}

/// The un-flattened heightfield. `terrain.js:22` (`rawHeight`).
///
/// **Transcendental.** See [`terrain_height`].
#[must_use]
fn raw_terrain_height(x: f64, z: f64) -> f64 {
    // Two broad, low-frequency ridge systems, each a product of two half-
    // rectified sines so the ranges form blobs rather than a grid.
    let mut h = 0.0;
    h += ((x * 0.0010 + 1.1).sin() * 0.5 + 0.5) * ((z * 0.0012 + 2.3).sin() * 0.5 + 0.5) * 390.0;
    h += ((x * 0.0014 + 3.4).sin() * 0.5 + 0.5) * ((z * 0.0009 + 0.7).sin() * 0.5 + 0.5) * 255.0;
    // Two clipped octaves: `max(0, ..)` cuts the troughs off, which is what puts
    // flat valley floors between the ridges.
    h += ((x * 0.0029 + 0.9).sin() * (z * 0.0026 + 1.6).cos()).max(0.0) * 165.0;
    h += (x * 0.0039 - z * 0.0023 + 2.2).sin().max(0.0) * 105.0;
    // Three unclipped detail octaves.
    h += (x * 0.0072 + 1.7).sin() * (z * 0.0065 + 0.4).cos() * 42.0;
    h += (x * 0.0117 - z * 0.0091 + 3.1).sin() * 24.0;
    h += (x * 0.0208 + z * 0.0182 + 0.8).sin() * 12.0;
    h.max(0.0)
}

/// How flat the terrain is forced to be at `(x, z)`: 0 on open ground, 1 on an
/// airfield apron. `terrain.js:13` (`airfieldBlend`).
#[must_use]
fn airfield_blend(x: f64, z: f64, rules: &Rules) -> f64 {
    let hw = rules.world.airfield_half.x;
    let hd = rules.world.airfield_half.z;
    let mut max_blend = 0.0f64;
    for cz in [-rules.world.airfield_z, rules.world.airfield_z] {
        let tx = (1.0 - (x / hw).abs()).max(0.0);
        let tz = (1.0 - ((z - cz) / hd).abs()).max(0.0);
        max_blend = max_blend.max(smoothstep(tx.min(tz) * AIRFIELD_BLEND_SHARPNESS));
    }
    max_blend
}

/// The altitude below which a ship dies on the terrain map: the surface plus
/// [`crate::rules::WorldRules::terrain_kill_clearance`]. `main.js:2251`.
#[must_use]
pub fn terrain_kill_altitude(x: f64, z: f64, rules: &Rules) -> f64 {
    terrain_height(x, z, rules) + rules.world.terrain_kill_clearance
}

// ---------------------------------------------------------------------------
// Ship vs. world
// ---------------------------------------------------------------------------

/// The static geometry a ship is resolved against.
///
/// Handed in as slices rather than as a `&World` so the caller can hold a
/// `&mut Ship` at the same time: `World`'s ship list and its geometry lists are
/// separate fields, and Rust's disjoint-field borrows make
/// `resolve_world_collisions(&mut w.ships[i], .., WorldGeometry { asteroids: &w.asteroids, .. })`
/// legal.
#[derive(Debug, Clone, Copy)]
pub struct WorldGeometry<'a> {
    /// Destructible rocks. Contact costs 15–29 hit points, once per entry.
    pub asteroids: &'a [Asteroid],
    /// Indestructible spheres — the moon. Contact is fatal.
    pub obstacles: &'a [Obstacle],
    /// Solid boxes — motherships or airfields. Contact only bounces.
    pub boxes: &'a [BoxVolume],
    /// Which map, which decides whether the terrain kill plane exists.
    pub map: MapKind,
}

/// What a ship ran into during one step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CollisionReport {
    /// Total self-damage earned this step, **before** the invulnerability gate.
    ///
    /// Route it through [`apply_damage`], which is where that gate lives. The JS
    /// gates inside `dealSelfDamage` (`main.js:2168`) *after* rolling the
    /// damage, so the RNG stream advances identically whether or not the ship
    /// was protected; that is reproduced here.
    pub self_damage: i32,
    /// Number of asteroids newly entered this step. Collision damage is
    /// edge-triggered: sitting inside a rock costs nothing after the first frame
    /// (`main.js:2214`).
    pub asteroid_impacts: u32,
    /// The moon was newly entered. Worth [`crate::rules::ShipRules::max_hp`],
    /// i.e. instant death from full health.
    pub moon_impact: bool,
    /// The terrain kill plane was newly crossed. Also worth full hit points.
    pub terrain_impact: bool,
    /// A mothership or airfield hull was touched. Costs nothing; reported for
    /// the impact sound.
    pub box_impact: bool,
    /// The step was resolved by the swept pass rather than the static one — the
    /// ship would have tunnelled clean through under the JS's end-of-frame point
    /// test.
    pub tunnelled: bool,
}

/// Pushes a ship out of everything it is touching and charges it for the
/// privilege.
///
/// Ports `resolveCollisions` (`main.js:2192`) and `resolveMothershipCollisions`
/// (`main.js:2160`) in that order, which matters: the hull push-out runs last
/// and therefore wins if a rock is embedded in a mothership.
///
/// `prev_pos` is [`FlightStep::prev_pos`], the position before the step. It is
/// needed for the sweep and for nothing else.
///
/// # Damage
///
/// Nothing here touches hit points. [`CollisionReport::self_damage`] is returned
/// for the caller to feed to [`apply_damage`], because damage means death means
/// respawn timers means scoreboards, none of which belong in a geometry routine.
/// The push-out and the restitution *are* applied unconditionally, including to
/// an invulnerable ship: spawn protection stops damage, not physics.
///
/// # The swept pass, and its one blind spot
///
/// The static pass reproduces the JS exactly. If it finds nothing, the swept
/// pass looks for a body the whole step passed through and resolves against the
/// earliest one.
///
/// What that does not catch: a step that tunnels through body A *and* ends up
/// inside body B. The static pass resolves B, the swept pass never runs, and A
/// is missed. Fixing it properly means iterating to the first contact, advancing
/// to it, and re-sweeping the remainder — a real continuous solver, and a change
/// to where the ship ends up in ordinary contact too. That is worth doing, and
/// it is not worth doing quietly inside a port.
pub fn resolve_world_collisions(
    ship: &mut Ship,
    prev_pos: Vec3,
    geom: WorldGeometry<'_>,
    rules: &Rules,
    mode: Mode,
    rng: &mut Rng,
) -> CollisionReport {
    let mut report = CollisionReport::default();
    if !ship.alive {
        return report;
    }
    let radius = rules.ship.collide_radius;
    let takes_damage = mode.has_collision_damage();
    let swept_end = ship.pos;
    let mut any_static_contact = false;

    // -- Asteroids. `main.js:2193`–`:2221`. -----------------------------------
    // `touching_asteroids` is rebuilt from scratch each step; an id that was in
    // it last step and is not now has been left behind, and re-entering charges
    // again.
    let mut still_touching: Vec<u32> = Vec::new();
    for a in geom.asteroids {
        if resolve_sphere(
            &mut ship.pos,
            &mut ship.vel,
            radius,
            a.pos,
            a.radius,
            rules.combat.collision_restitution,
        ) {
            any_static_contact = true;
            still_touching.push(a.id);
            if !ship.touching_asteroids.contains(&a.id) {
                report.asteroid_impacts += 1;
                let dmg = roll_asteroid_collision_damage(rules, rng);
                if takes_damage {
                    report.self_damage += dmg;
                }
            }
        }
    }
    ship.touching_asteroids = still_touching;

    // -- Obstacles (the moon). `main.js:2222`–`:2248`. ------------------------
    let mut moon_contact = false;
    for o in geom.obstacles {
        if resolve_sphere(
            &mut ship.pos,
            &mut ship.vel,
            radius,
            o.pos,
            o.radius,
            rules.combat.collision_restitution,
        ) {
            any_static_contact = true;
            moon_contact = true;
            if !ship.touching_moon {
                report.moon_impact = true;
                if takes_damage {
                    report.self_damage += rules.ship.max_hp;
                }
            }
        }
    }
    ship.touching_moon = moon_contact;

    // -- Terrain. `main.js:2249`–`:2260`. -------------------------------------
    if geom.map == MapKind::Terrain {
        let contact = resolve_terrain(ship, prev_pos, rules);
        if contact {
            any_static_contact = true;
            if !ship.touching_ground {
                report.terrain_impact = true;
                if takes_damage {
                    report.self_damage += rules.ship.max_hp;
                }
            }
        }
        ship.touching_ground = contact;
    }

    // -- Boxes. `main.js:2160`–`:2164`. ---------------------------------------
    // No damage and no edge trigger: you bounce off a hangar, you do not lose
    // hit points to it.
    for b in geom.boxes {
        if resolve_box(
            &mut ship.pos,
            &mut ship.vel,
            radius,
            *b,
            rules.combat.box_collision_restitution,
        ) {
            any_static_contact = true;
            report.box_impact = true;
        }
    }

    // -- The swept pass. ------------------------------------------------------
    if !any_static_contact {
        if let Some(hit) = earliest_swept_body(prev_pos, swept_end, radius, geom) {
            report.tunnelled = true;
            let contact = prev_pos.lerp(swept_end, hit.t);
            resolve_tunnelled(
                ship,
                contact,
                hit.body,
                geom,
                rules,
                takes_damage,
                rng,
                &mut report,
            );
        }
    }

    report
}

/// Rolls collision damage for one asteroid entry.
///
/// `main.js:2215` writes `15 + Math.floor(Math.random() * 15)`, an inclusive
/// 15..=29. The draw here uses the crate's unbiased bounded generator rather
/// than `floor(f64 * n)`; both are uniform over the same range, and the
/// generator is the one the rest of the simulation uses.
fn roll_asteroid_collision_damage(rules: &Rules, rng: &mut Rng) -> i32 {
    let lo = rules.combat.asteroid_collision_damage_min;
    let hi = rules.combat.asteroid_collision_damage_max;
    if hi <= lo {
        return lo;
    }
    let span = (hi - lo + 1) as u32;
    lo + rng.bounded_u32(span) as i32
}

/// Pushes a sphere out of another sphere and bounces it. Returns whether there
/// was contact.
///
/// The body of `main.js:2196`–`:2213`, repeated verbatim by the JS for the moon
/// at `:2225`–`:2242` with only the damage differing.
///
/// # What the restitution number actually means
///
/// [`crate::rules::CombatRules::collision_restitution`] is 1.3 and its doc
/// comment reads "greater than 1, so a collision adds energy". **That reading of
/// the JS is wrong**, and the value is reproduced here rather than corrected.
///
/// The JS applies `vel -= k * (v·n) * n`, so the outgoing normal speed is
/// `(k - 1)` times the incoming one. `k = 1.0` would kill the normal component
/// dead; `k = 2.0` would be a perfectly elastic bounce. `k = 1.3` gives a
/// coefficient of restitution of **0.3** — a rock absorbs 70 % of the speed you
/// hit it with, and boxes at 1.4 absorb 60 %. Nothing in the game adds energy on
/// contact. The tangential component is untouched either way, so a glancing blow
/// mostly slides.
fn resolve_sphere(
    pos: &mut Vec3,
    vel: &mut Vec3,
    radius: f64,
    center: Vec3,
    target_radius: f64,
    restitution: f64,
) -> bool {
    let d = *pos - center;
    let dist_sq = d.length_squared();
    let min_dist = radius + target_radius;
    if dist_sq >= min_dist * min_dist || dist_sq <= CONTACT_MIN_DIST_SQ {
        return false;
    }
    let dist = dist_sq.sqrt();
    let n = d / dist;
    apply_contact(pos, vel, n, min_dist - dist, restitution);
    true
}

/// Pushes a sphere out of an axis-aligned box and bounces it. Returns whether
/// there was contact. `collideSphereWithBox`, `main.js:2123`.
///
/// The interior case picks the shallowest face and pushes out through it, which
/// is what stops a ship that has somehow ended up inside a mothership from being
/// launched through the far wall.
fn resolve_box(
    pos: &mut Vec3,
    vel: &mut Vec3,
    radius: f64,
    b: BoxVolume,
    restitution: f64,
) -> bool {
    let d = *pos - b.pos;
    let inside = d.x.abs() < b.half.x && d.y.abs() < b.half.y && d.z.abs() < b.half.z;
    let (n, push) = if inside {
        let px = b.half.x - d.x.abs();
        let py = b.half.y - d.y.abs();
        let pz = b.half.z - d.z.abs();
        if px < py && px < pz {
            (Vec3::new(sign_or_positive(d.x), 0.0, 0.0), px + radius)
        } else if py < pz {
            (Vec3::new(0.0, sign_or_positive(d.y), 0.0), py + radius)
        } else {
            (Vec3::new(0.0, 0.0, sign_or_positive(d.z)), pz + radius)
        }
    } else {
        let c = d.clamp(-b.half, b.half);
        let o = d - c;
        let dist_sq = o.length_squared();
        if dist_sq >= radius * radius || dist_sq < CONTACT_MIN_DIST_SQ {
            return false;
        }
        let dist = dist_sq.sqrt();
        (o / dist, radius - dist)
    };
    apply_contact(pos, vel, n, push, restitution);
    true
}

/// Displaces a body along a contact normal and reflects the inbound part of its
/// velocity. Shared tail of [`resolve_sphere`] and [`resolve_box`].
///
/// `n` must be unit length. Velocity is only touched when it is heading *into*
/// the surface, so a body already leaving is not flung.
#[inline]
fn apply_contact(pos: &mut Vec3, vel: &mut Vec3, n: Vec3, push: f64, restitution: f64) {
    if push > 0.0 {
        *pos = pos.add_scaled(n, push);
    }
    let v_dot_n = vel.dot(n);
    if v_dot_n < 0.0 {
        *vel = vel.add_scaled(n, -restitution * v_dot_n);
    }
}

/// Clamps a ship to the terrain kill plane and reports whether it was below it.
///
/// The JS tests the end-of-step position only (`main.js:2250`). This samples the
/// step's horizontal segment at [`TERRAIN_SWEEP_SPACING`] intervals and stops at
/// the first sample below its local kill plane, so a ship crossing a ridge
/// inside one step is caught rather than teleported through it. With one sample
/// — the ordinary case, because a step is a few units long — the arithmetic is
/// identical to the JS.
///
/// The terrain is a heightfield, not a body, so [`crate::collision`] has no test
/// for it and sampling is the tractable form. It is a bound, not a proof: a
/// ridge narrower than the sample spacing can still be missed. The heightfield's
/// steepest gradient is about 2.1, so over the longest possible step
/// ([`crate::rules::MAX_FRAME_DT`] at full boost, ~9.5 units) the ground can
/// rise about 20 units — enough to matter only for a ship already skimming the
/// surface.
fn resolve_terrain(ship: &mut Ship, prev_pos: Vec3, rules: &Rules) -> bool {
    let motion = ship.pos - prev_pos;
    let horizontal = (motion.x * motion.x + motion.z * motion.z).sqrt();
    let samples = terrain_sample_count(horizontal);

    for i in 1..=samples {
        let t = f64::from(i) / f64::from(samples);
        let p = prev_pos.add_scaled(motion, t);
        let kill_y = terrain_kill_altitude(p.x, p.z, rules);
        if p.y < kill_y {
            ship.pos = Vec3::new(p.x, kill_y, p.z);
            if ship.vel.y < 0.0 {
                ship.vel.y *= rules.combat.terrain_bounce;
            }
            return true;
        }
    }
    false
}

/// How many points to sample along a step of the given horizontal length. Always
/// at least one — the endpoint, which is what the JS tests.
#[inline]
fn terrain_sample_count(horizontal: f64) -> u32 {
    // The finiteness test is not redundant: a non-finite step must degenerate to
    // the plain endpoint test rather than to an unbounded loop.
    if !horizontal.is_finite() || horizontal <= TERRAIN_SWEEP_SPACING {
        return 1;
    }
    let n = (horizontal / TERRAIN_SWEEP_SPACING).ceil();
    if n >= f64::from(TERRAIN_SWEEP_MAX_SAMPLES) {
        TERRAIN_SWEEP_MAX_SAMPLES
    } else {
        n as u32
    }
}

/// Which body a swept step ran into, and when.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SweptHit {
    /// Which body.
    body: SweptBody,
    /// Fraction of the step at first contact.
    t: f64,
}

/// The kind and index of a body found by the swept pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SweptBody {
    Asteroid(usize),
    Obstacle(usize),
    Hull(usize),
}

/// Finds the first body the segment `prev -> end` passes through, or `None`.
///
/// Ties go to the earliest `t`, then to asteroids over obstacles over hulls,
/// then to the lowest index — a total order that depends only on the geometry
/// and never on iteration luck.
///
/// This walks [`WorldGeometry`] directly rather than calling
/// [`crate::collision::sweep_first_hit`], which wants a `&[Sphere]`. Building
/// one would mean allocating a temporary every tick to re-describe rocks the
/// world already stores; the loop below is the same test with the same
/// tie-break, and it also has to cover the boxes, which that helper does not.
fn earliest_swept_body(
    prev: Vec3,
    end: Vec3,
    radius: f64,
    geom: WorldGeometry<'_>,
) -> Option<SweptHit> {
    let motion = end - prev;
    if motion == Vec3::ZERO {
        return None;
    }
    let mut best: Option<SweptHit> = None;
    let mut consider = |body: SweptBody, t: Option<f64>| {
        if let Some(t) = t {
            let better = match best {
                Some(b) => t < b.t,
                None => true,
            };
            if better {
                best = Some(SweptHit { body, t });
            }
        }
    };

    for (i, a) in geom.asteroids.iter().enumerate() {
        consider(
            SweptBody::Asteroid(i),
            swept_sphere_sphere(prev, motion, radius, Sphere::new(a.pos, a.radius)),
        );
    }
    for (i, o) in geom.obstacles.iter().enumerate() {
        consider(
            SweptBody::Obstacle(i),
            swept_sphere_sphere(prev, motion, radius, Sphere::new(o.pos, o.radius)),
        );
    }
    for (i, b) in geom.boxes.iter().enumerate() {
        consider(
            SweptBody::Hull(i),
            swept_sphere_aabb(prev, motion, radius, Aabb::new(b.pos, b.half)),
        );
    }
    best
}

/// Places a tunnelling ship at its first contact point and resolves it there.
///
/// At the contact point the surfaces are exactly tangent, so there is no
/// penetration to push out of and only the restitution applies — which is the
/// right answer: the ship stops against the hull it was about to pass through
/// instead of appearing on the far side of it.
#[allow(clippy::too_many_arguments)]
fn resolve_tunnelled(
    ship: &mut Ship,
    contact: Vec3,
    body: SweptBody,
    geom: WorldGeometry<'_>,
    rules: &Rules,
    takes_damage: bool,
    rng: &mut Rng,
    report: &mut CollisionReport,
) {
    ship.pos = contact;
    match body {
        SweptBody::Asteroid(i) => {
            let a = geom.asteroids[i];
            bounce_off_point(ship, a.pos, rules.combat.collision_restitution);
            if !ship.touching_asteroids.contains(&a.id) {
                ship.touching_asteroids.push(a.id);
            }
            report.asteroid_impacts += 1;
            let dmg = roll_asteroid_collision_damage(rules, rng);
            if takes_damage {
                report.self_damage += dmg;
            }
        }
        SweptBody::Obstacle(i) => {
            let o = geom.obstacles[i];
            bounce_off_point(ship, o.pos, rules.combat.collision_restitution);
            ship.touching_moon = true;
            report.moon_impact = true;
            if takes_damage {
                report.self_damage += rules.ship.max_hp;
            }
        }
        SweptBody::Hull(i) => {
            let b = geom.boxes[i];
            let closest = Aabb::new(b.pos, b.half).closest_point(contact);
            bounce_off_point(ship, closest, rules.combat.box_collision_restitution);
            report.box_impact = true;
        }
    }
}

/// Reflects a ship's velocity off a surface whose nearest interior point is
/// `inside`, without displacing it. Used where the ship is already exactly on
/// the surface.
fn bounce_off_point(ship: &mut Ship, inside: Vec3, restitution: f64) {
    let n = (ship.pos - inside).normalize();
    if n == Vec3::ZERO {
        return;
    }
    apply_contact(&mut ship.pos, &mut ship.vel, n, 0.0, restitution);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{AsteroidTier, ShipKind, TICK_DT};

    fn rules() -> Rules {
        Rules::DEFAULT
    }

    fn new_ship() -> Ship {
        let r = rules();
        let mut s = Ship::spawn(1, ShipKind::Local, Vec3::ZERO, Quat::IDENTITY, &r);
        s.invuln_timer = 0.0;
        s
    }

    fn idle_input() -> Input {
        Input {
            id: 1,
            ..Input::default()
        }
    }

    fn run(ship: &mut Ship, input: &Input, steps: u32) {
        let r = rules();
        for _ in 0..steps {
            integrate(ship, input, &r, Mode::Skirmish, TICK_DT);
        }
    }

    // -- Quaternions ------------------------------------------------------

    #[test]
    fn identity_faces_positive_z() {
        assert_eq!(forward(Quat::IDENTITY), Vec3::Z);
        assert_eq!(up(Quat::IDENTITY), Vec3::Y);
        assert_eq!(right(Quat::IDENTITY), Vec3::X);
    }

    #[test]
    fn flip_y_faces_negative_z() {
        // The team-1 spawn orientation (`server/index.js:481`).
        assert!(forward(Quat::FLIP_Y).abs_diff_eq(-Vec3::Z, 1e-12));
    }

    #[test]
    fn axis_angle_and_multiply_compose_like_three_js() {
        // Yaw 90 degrees about +y takes the nose from +z to +x.
        let q = quat_from_axis_angle(Vec3::Y, std::f64::consts::FRAC_PI_2);
        assert!(forward(q).abs_diff_eq(Vec3::X, 1e-12));
        // Two half turns compose to the whole one.
        let h = quat_from_axis_angle(Vec3::Y, std::f64::consts::FRAC_PI_4);
        let composed = quat_normalize(quat_mul(h, h));
        assert!(forward(composed).abs_diff_eq(forward(q), 1e-12));
    }

    #[test]
    fn normalize_rejects_a_degenerate_quaternion() {
        assert_eq!(
            quat_normalize(Quat::new(0.0, 0.0, 0.0, 0.0)),
            Quat::IDENTITY
        );
        assert_eq!(
            quat_normalize(Quat::new(0.0, 0.0, 0.0, 4.0)),
            Quat::IDENTITY
        );
    }

    #[test]
    fn steering_rotates_about_the_ships_own_axes() {
        let mut ship = new_ship();
        let mut input = idle_input();
        input.steer_x = 1.0;
        // `yaw = -sx * YAW_RATE`, so a right stick yaws the nose toward -x.
        run(&mut ship, &input, 30);
        let f = forward(ship.quat);
        assert!(f.x < -0.1, "expected a yaw, got {f:?}");
        assert!(f.y.abs() < 1e-9, "pure yaw must not introduce pitch");
    }

    // -- Throttle ---------------------------------------------------------

    #[test]
    fn throttle_axis_ramps_and_clamps_to_max() {
        let r = rules();
        let mut ship = new_ship();
        let mut input = idle_input();
        input.throttle_axis = 1.0;
        // KEY_THROTTLE_RATE is 30/s, so one second reaches 30 of 80.
        run(&mut ship, &input, 60);
        assert!(ship.target_throttle > 25.0 && ship.target_throttle < 35.0);
        run(&mut ship, &input, 300);
        assert_eq!(ship.target_throttle, r.ship.max_throttle);
        // The smoothed throttle chases it and never overshoots.
        assert!(ship.throttle <= r.ship.max_throttle);
        assert!(ship.throttle > r.ship.max_throttle * 0.99);
    }

    #[test]
    fn throttle_never_goes_negative() {
        let mut ship = new_ship();
        let mut input = idle_input();
        input.throttle_axis = -1.0;
        run(&mut ship, &input, 300);
        assert_eq!(ship.target_throttle, 0.0);
    }

    #[test]
    fn wheel_notches_step_the_throttle() {
        let r = rules();
        let mut ship = new_ship();
        let mut input = idle_input();
        input.throttle_notches = 3.0;
        integrate(&mut ship, &input, &r, Mode::Skirmish, TICK_DT);
        assert_eq!(ship.target_throttle, 3.0 * r.ship.throttle_step);
    }

    #[test]
    fn touch_override_commands_an_absolute_throttle() {
        let r = rules();
        let mut ship = new_ship();
        ship.target_throttle = 80.0;
        let mut input = idle_input();
        input.throttle_override = Some(0.25);
        input.throttle_notches = 5.0; // consumed and discarded
        integrate(&mut ship, &input, &r, Mode::Skirmish, TICK_DT);
        assert_eq!(ship.target_throttle, 0.25 * r.ship.max_throttle);
    }

    #[test]
    fn the_gamepad_throttle_deadzone_is_honoured() {
        let r = rules();
        let mut ship = new_ship();
        let mut input = idle_input();
        input.throttle_axis = 0.005;
        integrate(&mut ship, &input, &r, Mode::Skirmish, TICK_DT);
        assert_eq!(ship.target_throttle, 0.0);
        input.throttle_axis = 0.5;
        integrate(&mut ship, &input, &r, Mode::Skirmish, TICK_DT);
        assert!(ship.target_throttle > 0.0);
    }

    // -- Steering ---------------------------------------------------------

    #[test]
    fn steering_inside_the_deadzone_does_nothing() {
        let r = rules();
        let mut ship = new_ship();
        let mut input = idle_input();
        input.steer_x = 0.04; // below STEER_DEADZONE = 0.05
        let step = integrate(&mut ship, &input, &r, Mode::Skirmish, TICK_DT);
        assert_eq!(step.steer, [0.0, 0.0]);
        assert_eq!(ship.quat, Quat::IDENTITY);
    }

    #[test]
    fn the_response_curve_softens_small_deflections() {
        let r = rules();
        let mut ship = new_ship();
        let mut input = idle_input();
        input.steer_x = 0.5;
        let step = integrate(&mut ship, &input, &r, Mode::Skirmish, TICK_DT);
        // 0.5^1.6 is about 0.33: half a stick is a third of the authority.
        assert!((step.steer[0] - 0.5f64.powf(1.6)).abs() < 1e-12);
        assert!(step.steer[0] < 0.5);
        // Full deflection is untouched, so top-end authority is unchanged.
        input.steer_x = 1.0;
        let step = integrate(&mut ship, &input, &r, Mode::Skirmish, TICK_DT);
        assert!((step.steer[0] - 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_centred_stick_produces_exactly_zero() {
        // Math.sign(0) is 0; f64::signum(+0.0) is 1.0. Getting this wrong is
        // invisible until a sign flip shows up downstream.
        assert_eq!(steer_curve(0.0, 1.6), 0.0);
        assert_eq!(steer_curve(-0.0, 1.6), 0.0);
    }

    #[test]
    fn arrow_keys_ramp_instead_of_snapping() {
        let r = rules();
        let mut ship = new_ship();
        let mut input = idle_input();
        input.arrow_x = 1.0;
        let step = integrate(&mut ship, &input, &r, Mode::Skirmish, TICK_DT);
        // One 60 Hz frame at rate 3 reaches ~5% of full deflection.
        assert!(step.steer[0] > 0.0 && step.steer[0] < 0.1);
        run(&mut ship, &input, 120);
        assert!(ship.arrow_kx > 0.99);
        // Releasing ramps back down four times faster.
        input.arrow_x = 0.0;
        run(&mut ship, &input, 30);
        assert!(ship.arrow_kx < 0.01, "arrow_kx = {}", ship.arrow_kx);
    }

    #[test]
    fn the_fine_aim_modifier_slows_the_ramp() {
        let r = rules();
        let mut coarse = new_ship();
        let mut fine = new_ship();
        let mut input = idle_input();
        input.arrow_y = 1.0;
        for _ in 0..20 {
            integrate(&mut coarse, &input, &r, Mode::Skirmish, TICK_DT);
        }
        input.arrow_fine = true;
        for _ in 0..20 {
            integrate(&mut fine, &input, &r, Mode::Skirmish, TICK_DT);
        }
        assert!(fine.arrow_ky < coarse.arrow_ky);
    }

    #[test]
    fn free_look_suppresses_steering() {
        let r = rules();
        let mut ship = new_ship();
        let mut input = idle_input();
        input.steer_x = 1.0;
        input.free_look = true;
        let step = integrate(&mut ship, &input, &r, Mode::Skirmish, TICK_DT);
        assert_eq!(step.steer, [0.0, 0.0]);
        assert_eq!(ship.quat, Quat::IDENTITY);
    }

    #[test]
    fn braking_buys_yaw_authority() {
        let r = rules();
        let mut plain = new_ship();
        let mut drifting = new_ship();
        let mut input = idle_input();
        input.steer_x = 1.0;
        integrate(&mut plain, &input, &r, Mode::Skirmish, TICK_DT);
        input.braking = true;
        integrate(&mut drifting, &input, &r, Mode::Skirmish, TICK_DT);
        // BRAKE_YAW_MULT is 1.7, so the braked ship turned further.
        assert!(forward(drifting.quat).x < forward(plain.quat).x);
    }

    #[test]
    fn nose_up_is_faster_than_nose_down() {
        let r = rules();
        let mut up_ship = new_ship();
        let mut down_ship = new_ship();
        let mut input = idle_input();
        input.steer_y = -1.0;
        integrate(&mut up_ship, &input, &r, Mode::Skirmish, TICK_DT);
        input.steer_y = 1.0;
        integrate(&mut down_ship, &input, &r, Mode::Skirmish, TICK_DT);
        // PITCH_UP_BOOST = 1.25 applies only to the nose-up direction.
        let up_angle = forward(up_ship.quat).y.abs();
        let down_angle = forward(down_ship.quat).y.abs();
        assert!(up_angle > down_angle * 1.2, "{up_angle} vs {down_angle}");
    }

    // -- Velocity ---------------------------------------------------------

    #[test]
    fn velocity_converges_on_throttle_along_the_nose() {
        let r = rules();
        let mut ship = new_ship();
        let mut input = idle_input();
        input.throttle_axis = 1.0;
        run(&mut ship, &input, 600);
        assert!((ship.vel.length() - r.ship.max_throttle).abs() < 0.5);
        assert!(ship.vel.normalize().abs_diff_eq(Vec3::Z, 1e-6));
        // And the position is the time integral of it.
        assert!(ship.pos.z > 0.0);
    }

    #[test]
    fn boost_multiplies_top_speed_and_drains_the_meter() {
        let r = rules();
        let mut ship = new_ship();
        let mut input = idle_input();
        input.throttle_axis = 1.0;
        input.boost = true;
        let before = ship.boost_meter;
        run(&mut ship, &input, 120);
        // BOOST_DRAIN is 2/s, so two seconds costs 4 of the 10-second meter.
        assert!((before - ship.boost_meter - 4.0).abs() < 0.05);
        run(&mut ship, &input, 400);
        // The meter runs dry after 5 s, so the ship falls back to plain top
        // speed; what matters is that it exceeded it while the meter lasted.
        assert_eq!(ship.boost_meter, 0.0);
        assert!(ship.pos.z > r.ship.max_throttle * 8.0);
    }

    #[test]
    fn a_full_meter_holds_the_boosted_top_speed() {
        let mut r = rules();
        r.ship.boost_drain = 0.0; // hold the meter open to observe the ceiling
        let mut ship = new_ship();
        ship.throttle = r.ship.max_throttle;
        ship.target_throttle = r.ship.max_throttle;
        let mut input = idle_input();
        input.throttle_axis = 1.0;
        input.boost = true;
        for _ in 0..600 {
            integrate(&mut ship, &input, &r, Mode::Skirmish, TICK_DT);
        }
        let top = r.ship.max_throttle * r.ship.boost_factor;
        assert!(
            (ship.vel.length() - top).abs() < 0.5,
            "{}",
            ship.vel.length()
        );
    }

    #[test]
    fn an_empty_boost_meter_stops_boosting_and_blocks_recharge() {
        let mut ship = new_ship();
        ship.boost_meter = 0.05;
        let mut input = idle_input();
        input.boost = true;
        run(&mut ship, &input, 10);
        assert_eq!(ship.boost_meter, 0.0);
        // Holding the key pins `boost_idle` at zero, so nothing refills.
        run(&mut ship, &input, 240);
        assert_eq!(ship.boost_meter, 0.0);
        // Releasing starts the delay, then the recharge.
        input.boost = false;
        run(&mut ship, &input, 30);
        assert_eq!(ship.boost_meter, 0.0, "still inside BOOST_REGEN_DELAY");
        run(&mut ship, &input, 120);
        assert!(ship.boost_meter > 0.0);
    }

    #[test]
    fn boost_recharges_to_the_cap_and_stops() {
        let r = rules();
        let mut ship = new_ship();
        ship.boost_meter = 0.0;
        let input = idle_input();
        run(&mut ship, &input, 1200);
        assert_eq!(ship.boost_meter, r.ship.max_boost);
    }

    #[test]
    fn boosting_and_braking_are_mutually_exclusive() {
        let r = rules();
        let mut ship = new_ship();
        let mut input = idle_input();
        input.boost = true;
        input.braking = true;
        let step = integrate(&mut ship, &input, &r, Mode::Skirmish, TICK_DT);
        assert!(!step.shift_boost);
        assert_eq!(
            ship.boost_meter, r.ship.max_boost,
            "the meter must not drain"
        );
    }

    // -- Drift ------------------------------------------------------------

    #[test]
    fn drifting_keeps_momentum_off_the_nose() {
        let r = rules();
        let mut ship = new_ship();
        ship.vel = Vec3::Z * 80.0;
        // Point the nose 90 degrees off the velocity, then drift.
        ship.quat = quat_from_axis_angle(Vec3::Y, std::f64::consts::FRAC_PI_2);
        let mut input = idle_input();
        input.braking = true;
        integrate(&mut ship, &input, &r, Mode::Skirmish, TICK_DT);
        // Grip has bent the velocity a little toward the nose (+x), but the ship
        // is still mostly travelling the way it was.
        assert!(ship.vel.x > 0.0, "grip must bend velocity toward the nose");
        assert!(ship.vel.z > ship.vel.x, "but not instantly");
    }

    #[test]
    fn drift_drag_bleeds_speed_and_the_hard_brake_bleeds_it_faster() {
        let r = rules();
        let mut soft = new_ship();
        let mut hard = new_ship();
        soft.vel = Vec3::Z * 80.0;
        hard.vel = Vec3::Z * 80.0;
        let mut input = idle_input();
        input.braking = true;
        for _ in 0..60 {
            integrate(&mut soft, &input, &r, Mode::Skirmish, TICK_DT);
        }
        input.hard_brake = true;
        for _ in 0..60 {
            integrate(&mut hard, &input, &r, Mode::Skirmish, TICK_DT);
        }
        // DRIFT_DRAG = 0.9/s, DRIFT_BRAKE = 0.1/s: one second leaves 90% and 10%.
        assert!(
            (soft.vel.length() - 72.0).abs() < 0.5,
            "{}",
            soft.vel.length()
        );
        assert!(
            (hard.vel.length() - 8.0).abs() < 0.5,
            "{}",
            hard.vel.length()
        );
    }

    // -- Brake charge and release boost ------------------------------------

    #[test]
    fn holding_the_brake_charges_and_releasing_it_boosts() {
        let r = rules();
        let mut ship = new_ship();
        let mut input = idle_input();
        input.braking = true;
        // BRAKE_FULL_TIME is 1.4 s, so 0.7 s is about half a charge.
        run(&mut ship, &input, 42);
        assert!(ship.brake_charge > 0.4 && ship.brake_charge < 0.6);
        let charge = ship.brake_charge;
        input.braking = false;
        integrate(&mut ship, &input, &r, Mode::Skirmish, TICK_DT);
        assert_eq!(ship.brake_charge, 0.0);
        assert!((ship.brake_boost_charge - charge).abs() < 1e-12);
        assert!(ship.brake_boost_timer > 0.0);
        // The release arms this frame and fires from the next one.
        let step = integrate(&mut ship, &input, &r, Mode::Skirmish, TICK_DT);
        assert!(step.brake_release_boost);
        assert!(step.boosting);
    }

    #[test]
    fn a_charge_below_the_minimum_yields_no_boost() {
        let r = rules();
        let mut ship = new_ship();
        let mut input = idle_input();
        input.braking = true;
        // BRAKE_BOOST_MIN is 0.18, reached at 0.25 s. Stop well short.
        run(&mut ship, &input, 6);
        assert!(ship.brake_charge < r.ship.brake_boost_min);
        input.braking = false;
        integrate(&mut ship, &input, &r, Mode::Skirmish, TICK_DT);
        assert_eq!(ship.brake_boost_timer, 0.0);
        assert_eq!(ship.brake_charge, 0.0);
    }

    #[test]
    fn the_release_boost_adds_speed_beyond_full_throttle() {
        let r = rules();
        let mut ship = new_ship();
        ship.throttle = r.ship.max_throttle;
        ship.target_throttle = r.ship.max_throttle;
        ship.brake_boost_timer = 1.0;
        ship.brake_boost_charge = 1.0;
        let mut input = idle_input();
        input.throttle_axis = 1.0;
        // The boost lasts 1 s; sample before it expires.
        run(&mut ship, &input, 50);
        assert!(ship.brake_boost_timer > 0.0);
        // The target is 80 + 50; the slack release blend gets partway there.
        assert!(ship.vel.z > r.ship.max_throttle, "{}", ship.vel.z);
        assert!(ship.vel.z < r.ship.max_throttle + r.ship.brake_boost_bonus_max + 1e-6);
    }

    #[test]
    fn overcharging_the_brake_costs_hit_points_in_whole_numbers() {
        let r = rules();
        let mut ship = new_ship();
        let mut input = idle_input();
        input.braking = true;
        // 1.4 s to full charge, then 2.0 s of grace: nothing before 3.4 s.
        let mut total = 0;
        for _ in 0..204 {
            total += integrate(&mut ship, &input, &r, Mode::Skirmish, TICK_DT).self_damage;
        }
        assert_eq!(total, 0, "damage must not start before the grace period");
        // Then 10 points a second.
        for _ in 0..60 {
            total += integrate(&mut ship, &input, &r, Mode::Skirmish, TICK_DT).self_damage;
        }
        assert!(
            (9..=11).contains(&total),
            "one second of overcharge = {total}"
        );
        // Releasing resets both the clock and the fractional accumulator.
        input.braking = false;
        integrate(&mut ship, &input, &r, Mode::Skirmish, TICK_DT);
        assert_eq!(ship.brake_overcharge_time, 0.0);
        assert_eq!(ship.self_damage_accum, 0.0);
    }

    #[test]
    fn the_tutorial_suppresses_overcharge_damage() {
        let r = rules();
        let mut ship = new_ship();
        let mut input = idle_input();
        input.braking = true;
        let mut total = 0;
        for _ in 0..600 {
            total += integrate(&mut ship, &input, &r, Mode::Tutorial, TICK_DT).self_damage;
        }
        assert_eq!(total, 0);
    }

    // -- Dead ships -------------------------------------------------------

    #[test]
    fn a_dead_ship_does_not_fly_but_its_meters_keep_running() {
        let mut ship = new_ship();
        ship.vel = Vec3::Z * 80.0;
        ship.boost_meter = 0.0;
        ship.brake_charge = 0.7;
        ship.alive = false;
        let mut input = idle_input();
        input.throttle_axis = 1.0;
        input.braking = true;
        let before = ship.pos;
        run(&mut ship, &input, 120);
        assert_eq!(ship.pos, before, "a corpse must not be integrated");
        assert_eq!(ship.brake_charge, 0.0, "brake state is cleared while dead");
        assert!(ship.boost_meter > 0.0, "the meter still refills");
    }

    // -- Damage, death, respawn -------------------------------------------

    #[test]
    fn damage_removes_hit_points_and_restarts_the_regen_clock() {
        let r = rules();
        let mut ship = new_ship();
        ship.health_idle_damage = 99.0;
        let out = apply_damage(&mut ship, 30, &r, Mode::Skirmish);
        assert_eq!(out.applied, 30);
        assert_eq!(out.hp, 70);
        assert!(!out.killed && !out.rejected);
        assert_eq!(ship.health_idle_damage, 0.0);
        assert_eq!(ship.hit_flash, 1.0);
    }

    #[test]
    fn spawn_protection_rejects_every_damage_path() {
        let r = rules();
        let mut ship = new_ship();
        ship.invuln_timer = r.combat.spawn_invuln;
        let out = apply_damage(&mut ship, 500, &r, Mode::Skirmish);
        assert!(out.rejected);
        assert_eq!(ship.hp, r.ship.max_hp);
        // This is the rule solo bots do not get in the JS — `applyHitToBot`
        // checks only `alive`. Here it is the same gate for everyone.
        let mut bot = Ship::spawn(7, ShipKind::Bot, Vec3::ZERO, Quat::IDENTITY, &r);
        assert!(apply_damage(&mut bot, 500, &r, Mode::Skirmish).rejected);
        assert!(bot.alive);
    }

    #[test]
    fn a_dead_ship_cannot_be_damaged_again() {
        let r = rules();
        let mut ship = new_ship();
        apply_damage(&mut ship, 1000, &r, Mode::Skirmish);
        assert!(!ship.alive);
        assert!(apply_damage(&mut ship, 10, &r, Mode::Skirmish).rejected);
    }

    #[test]
    fn death_zeroes_velocity_and_starts_the_respawn_clock() {
        let r = rules();
        let mut ship = new_ship();
        ship.vel = Vec3::Z * 120.0;
        let out = apply_damage(&mut ship, r.ship.max_hp, &r, Mode::Skirmish);
        assert!(out.killed);
        assert_eq!(ship.hp, 0);
        assert!(!ship.alive);
        assert_eq!(ship.vel, Vec3::ZERO);
        assert_eq!(ship.respawn_timer, r.combat.respawn_delay);
    }

    #[test]
    fn the_campaign_respawns_faster_and_hurt() {
        let r = rules();
        let mut ship = new_ship();
        apply_damage(&mut ship, 1000, &r, Mode::Campaign(1));
        assert_eq!(ship.respawn_timer, r.combat.campaign_respawn_delay);
        assert_eq!(campaign_respawn_hp(&r), 55);
    }

    #[test]
    fn the_respawn_countdown_fires_once() {
        let r = rules();
        let mut ship = new_ship();
        apply_damage(&mut ship, 1000, &r, Mode::Skirmish);
        let mut fired = 0;
        for _ in 0..200 {
            if tick_timers(&mut ship, &r, TICK_DT).respawn_due {
                fired += 1;
            }
        }
        assert_eq!(fired, 1);
        assert_eq!(ship.respawn_timer, 0.0);
    }

    #[test]
    fn respawn_restores_the_hull_the_loadout_and_the_spawn_window() {
        let r = rules();
        let mut ship = new_ship();
        ship.missiles_left = 0;
        ship.flares_left = 0;
        ship.throttle = 80.0;
        ship.vel = Vec3::ONE * 50.0;
        ship.touching_moon = true;
        apply_damage(&mut ship, 1000, &r, Mode::Skirmish);

        let pos = Vec3::new(0.0, 0.0, -540.0);
        respawn(&mut ship, pos, Quat::FLIP_Y, &r);
        assert!(ship.alive);
        assert_eq!(ship.hp, r.ship.max_hp);
        assert_eq!(ship.pos, pos);
        assert_eq!(ship.quat, Quat::FLIP_Y);
        assert_eq!(ship.vel, Vec3::ZERO);
        assert_eq!(ship.throttle, 0.0);
        assert_eq!(ship.missiles_left, r.weapons.missile_max);
        assert_eq!(ship.flares_left, r.weapons.flare_max);
        assert_eq!(ship.invuln_timer, r.combat.spawn_invuln);
        assert!(!ship.touching_moon);
        // And it is immune for the spawn window. 2.0 s is 120 ticks plus change:
        // `TICK_DT` is not exactly representable, so the countdown lands a few
        // ulps above zero and needs one more tick to clamp.
        assert!(apply_damage(&mut ship, 10, &r, Mode::Skirmish).rejected);
        for _ in 0..120 {
            tick_timers(&mut ship, &r, TICK_DT);
        }
        assert!(ship.invuln_timer < 1e-12);
        assert!(tick_timers(&mut ship, &r, TICK_DT).invuln_expired);
        assert_eq!(ship.invuln_timer, 0.0);
        assert!(!apply_damage(&mut ship, 10, &r, Mode::Skirmish).rejected);
    }

    #[test]
    fn invulnerability_expires_exactly_when_the_next_death_could_occur() {
        // RESPAWN_DELAY == SPAWN_INVULN, so a ship killed the instant its
        // protection ends comes back with a fresh window and no gap.
        let r = rules();
        assert_eq!(r.combat.respawn_delay, r.combat.spawn_invuln);
    }

    // -- Regeneration -----------------------------------------------------

    #[test]
    fn health_regenerates_only_after_both_clocks_clear() {
        let r = rules();
        let mut ship = new_ship();
        apply_damage(&mut ship, 40, &r, Mode::Skirmish);
        assert_eq!(ship.hp, 60);
        // The damage clock is at zero; nothing for two seconds.
        for _ in 0..119 {
            tick_timers(&mut ship, &r, TICK_DT);
        }
        assert_eq!(ship.hp, 60);
        // Then one point every 0.1 s.
        for _ in 0..60 {
            tick_timers(&mut ship, &r, TICK_DT);
        }
        assert!(ship.hp > 60 && ship.hp < 80, "hp = {}", ship.hp);
        // Firing suppresses it too.
        ship.health_idle_shot = 0.0;
        let hp = ship.hp;
        for _ in 0..60 {
            tick_timers(&mut ship, &r, TICK_DT);
        }
        assert_eq!(ship.hp, hp);
    }

    #[test]
    fn regeneration_stops_at_full_health() {
        let r = rules();
        let mut ship = new_ship();
        for _ in 0..2000 {
            tick_timers(&mut ship, &r, TICK_DT);
        }
        assert_eq!(ship.hp, r.ship.max_hp);
    }

    #[test]
    fn the_hit_flash_decays() {
        let r = rules();
        let mut ship = new_ship();
        apply_damage(&mut ship, 5, &r, Mode::Skirmish);
        assert_eq!(ship.hit_flash, 1.0);
        // 4 per second, so a quarter second clears it (plus a tick, because
        // `TICK_DT` is not exactly representable).
        for _ in 0..16 {
            tick_timers(&mut ship, &r, TICK_DT);
        }
        assert_eq!(ship.hit_flash, 0.0);
    }

    // -- Terrain ----------------------------------------------------------

    #[test]
    fn terrain_is_flat_outside_its_extent() {
        let r = rules();
        let edge = r.world.terrain_size * 0.5;
        assert_eq!(terrain_height(edge + 1.0, 0.0, &r), 0.0);
        assert_eq!(terrain_height(0.0, -edge - 1.0, &r), 0.0);
    }

    #[test]
    fn the_airfields_are_flat() {
        let r = rules();
        for cz in [-r.world.airfield_z, r.world.airfield_z] {
            // Dead centre of an apron: fully blended, so exactly zero.
            assert_eq!(
                terrain_height(0.0, cz, &r),
                0.0,
                "airfield at z = {cz} is not flat"
            );
        }
        // And well away from one, it is not.
        assert!(terrain_height(700.0, 0.0, &r) > 0.0);
    }

    #[test]
    fn terrain_height_is_never_negative_and_is_deterministic() {
        let r = rules();
        let mut rng = Rng::new(0x007E_88A1);
        for _ in 0..2000 {
            let x = rng.range_f64(-1800.0, 1800.0);
            let z = rng.range_f64(-1800.0, 1800.0);
            let h = terrain_height(x, z, &r);
            assert!(h >= 0.0, "negative height {h} at ({x}, {z})");
            assert!(h.is_finite());
            assert_eq!(h.to_bits(), terrain_height(x, z, &r).to_bits());
        }
    }

    #[test]
    fn the_kill_plane_sits_above_the_surface() {
        let r = rules();
        let (x, z) = (450.0, 220.0);
        assert_eq!(
            terrain_kill_altitude(x, z, &r),
            terrain_height(x, z, &r) + r.world.terrain_kill_clearance
        );
    }

    #[test]
    fn terrain_height_matches_the_javascript_it_was_ported_from() {
        // Reference values produced by running `terrain.js`'s `getTerrainHeight`
        // under Node against these exact coordinates. This is the test that
        // catches a transposed frequency or a dropped phase in the octave sum,
        // which no property test would.
        //
        // The tolerance is relative and loose enough to survive a libm that
        // rounds `sin` differently — the point is the transcription, not the
        // last bit. See `terrain_height` on why the last bit does not matter
        // here anyway.
        const CASES: [(f64, f64, f64); 8] = [
            (0.0, 0.0, 532.789_124_271_990_8),
            (600.0, 200.0, 328.275_495_291_581_2),
            (-1234.5, 987.25, 392.548_200_515_870_4),
            (450.0, 220.0, 351.276_079_467_126_6),
            (700.0, 0.0, 339.886_415_804_582_4),
            (-250.0, 1550.0, 119.358_095_791_100_25),
            (1799.0, 1799.0, 142.954_890_479_763_75),
            // On an airfield apron the blend is 1 and the result is a hard zero.
            (33.5, -1420.75, 0.0),
        ];
        let r = rules();
        for (x, z, expected) in CASES {
            let got = terrain_height(x, z, &r);
            let tol = 1e-12 * expected.abs().max(1.0);
            assert!(
                (got - expected).abs() <= tol,
                "getTerrainHeight({x}, {z}): got {got}, JS says {expected}"
            );
        }
    }

    // -- Collision --------------------------------------------------------

    fn rock(id: u32, pos: Vec3, radius: f64) -> Asteroid {
        Asteroid {
            id,
            pos,
            size: radius / 0.95,
            radius,
            hp: 5,
            tier: AsteroidTier::Small,
            variant: 0,
            rot: Vec3::ZERO,
            spin: Vec3::ZERO,
            hit_flash: 0.0,
        }
    }

    fn geom<'a>(
        asteroids: &'a [Asteroid],
        obstacles: &'a [Obstacle],
        boxes: &'a [BoxVolume],
        map: MapKind,
    ) -> WorldGeometry<'a> {
        WorldGeometry {
            asteroids,
            obstacles,
            boxes,
            map,
        }
    }

    /// `resolve_world_collisions` with the pre-step position snapshotted from
    /// the ship itself, for the many tests where the step had no motion.
    fn resolve_static(
        ship: &mut Ship,
        geom: WorldGeometry<'_>,
        rules: &Rules,
        mode: Mode,
        rng: &mut Rng,
    ) -> CollisionReport {
        let prev = ship.pos;
        resolve_world_collisions(ship, prev, geom, rules, mode, rng)
    }

    #[test]
    fn flying_into_a_rock_pushes_out_bounces_and_hurts_once() {
        let r = rules();
        let mut rng = Rng::new(1);
        let mut ship = new_ship();
        let rocks = [rock(0, Vec3::new(0.0, 0.0, 10.0), 8.0)];
        ship.pos = Vec3::new(0.0, 0.0, 5.0); // inside: 5 < 3.3 + 8
        ship.vel = Vec3::Z * 60.0;

        let out = resolve_world_collisions(
            &mut ship,
            Vec3::new(0.0, 0.0, 4.0),
            geom(&rocks, &[], &[], MapKind::Space),
            &r,
            Mode::Skirmish,
            &mut rng,
        );
        assert_eq!(out.asteroid_impacts, 1);
        assert!((15..=29).contains(&out.self_damage));
        // Pushed out to exactly the combined radius.
        assert!((ship.pos.distance(rocks[0].pos) - (r.ship.collide_radius + 8.0)).abs() < 1e-9);
        // And thrown back the way it came, at 0.3 of the speed it arrived with.
        // See `resolve_sphere`: the JS's 1.3 is `k` in `vel -= k * (v.n) * n`,
        // which is a coefficient of restitution of 0.3, not of 1.3.
        assert!(ship.vel.z < 0.0);
        assert!((ship.vel.z + 18.0).abs() < 1e-9, "vel.z = {}", ship.vel.z);
        assert_eq!(ship.touching_asteroids, vec![0]);

        // Sitting inside it costs nothing more.
        ship.pos = Vec3::new(0.0, 0.0, 5.0);
        let out = resolve_static(
            &mut ship,
            geom(&rocks, &[], &[], MapKind::Space),
            &r,
            Mode::Skirmish,
            &mut rng,
        );
        assert_eq!(out.self_damage, 0);
        assert_eq!(out.asteroid_impacts, 0);

        // Leaving and coming back charges again.
        ship.pos = Vec3::new(0.0, 0.0, -100.0);
        resolve_static(
            &mut ship,
            geom(&rocks, &[], &[], MapKind::Space),
            &r,
            Mode::Skirmish,
            &mut rng,
        );
        assert!(ship.touching_asteroids.is_empty());
        ship.pos = Vec3::new(0.0, 0.0, 5.0);
        let out = resolve_static(
            &mut ship,
            geom(&rocks, &[], &[], MapKind::Space),
            &r,
            Mode::Skirmish,
            &mut rng,
        );
        assert_eq!(out.asteroid_impacts, 1);
    }

    #[test]
    fn a_fast_ship_does_not_tunnel_through_a_rock() {
        // The regression `collision` exists for, applied to the hull: at the
        // maximum frame delta a boosting ship covers more ground than a small
        // rock is wide, and the JS point test never looks between frames.
        let r = rules();
        let mut rng = Rng::new(2);
        let rocks = [rock(0, Vec3::new(0.0, 0.0, 100.0), 4.75)];
        let prev = Vec3::new(0.0, 0.0, 90.0);
        let end = Vec3::new(0.0, 0.0, 110.0); // 20 units, straight through

        // Neither endpoint is in contact: the static test alone sees nothing.
        let reach = r.ship.collide_radius + 4.75;
        assert!(end.distance(rocks[0].pos) > reach);
        assert!(prev.distance(rocks[0].pos) > reach);

        let mut ship = new_ship();
        ship.pos = end;
        ship.vel = Vec3::Z * 320.0;
        let out = resolve_world_collisions(
            &mut ship,
            prev,
            geom(&rocks, &[], &[], MapKind::Space),
            &r,
            Mode::Skirmish,
            &mut rng,
        );
        assert!(out.tunnelled, "the sweep must catch the crossing");
        assert_eq!(out.asteroid_impacts, 1);
        assert!((15..=29).contains(&out.self_damage));
        // Stopped at the surface on the near side, not on the far side.
        assert!(ship.pos.z < rocks[0].pos.z, "ship ended at {:?}", ship.pos);
        assert!((ship.pos.distance(rocks[0].pos) - reach).abs() < 1e-9);
        assert!(ship.vel.z < 0.0, "and bounced");
        assert_eq!(ship.touching_asteroids, vec![0]);
    }

    #[test]
    fn the_static_pass_wins_when_both_could_fire() {
        // A step that both crosses one rock and ends inside another resolves
        // statically, exactly as the JS does. The sweep only covers the case the
        // JS misses entirely — see the docs on `resolve_world_collisions`.
        let r = rules();
        let mut rng = Rng::new(3);
        let rocks = [
            rock(0, Vec3::new(0.0, 0.0, 100.0), 4.75),
            rock(1, Vec3::new(0.0, 0.0, 120.0), 10.0),
        ];
        let mut ship = new_ship();
        ship.pos = Vec3::new(0.0, 0.0, 118.0);
        ship.vel = Vec3::Z * 300.0;
        let out = resolve_world_collisions(
            &mut ship,
            Vec3::new(0.0, 0.0, 92.0),
            geom(&rocks, &[], &[], MapKind::Space),
            &r,
            Mode::Skirmish,
            &mut rng,
        );
        assert!(!out.tunnelled);
        assert_eq!(out.asteroid_impacts, 1);
        assert_eq!(ship.touching_asteroids, vec![1]);
    }

    #[test]
    fn touching_the_moon_is_fatal() {
        let r = rules();
        let mut rng = Rng::new(4);
        let moon = [Obstacle {
            pos: r.world.moon_pos,
            radius: r.world.moon_radius,
        }];
        let mut ship = new_ship();
        ship.pos = Vec3::new(0.0, 0.0, 80.0);
        ship.vel = -Vec3::Z * 40.0;
        let out = resolve_static(
            &mut ship,
            geom(&[], &moon, &[], MapKind::Space),
            &r,
            Mode::Skirmish,
            &mut rng,
        );
        assert!(out.moon_impact);
        assert_eq!(out.self_damage, r.ship.max_hp);
        assert!(ship.touching_moon);
        // Which is exactly enough to kill from full health.
        assert!(apply_damage(&mut ship, out.self_damage, &r, Mode::Skirmish).killed);
    }

    #[test]
    fn a_fast_ship_does_not_tunnel_into_the_moon() {
        let r = rules();
        let mut rng = Rng::new(12);
        let moon = [Obstacle {
            pos: Vec3::ZERO,
            radius: 5.0, // a small sphere, to make the crossing fit in one step
        }];
        let mut ship = new_ship();
        ship.pos = Vec3::new(0.0, 0.0, 12.0);
        ship.vel = Vec3::Z * 480.0;
        let out = resolve_world_collisions(
            &mut ship,
            Vec3::new(0.0, 0.0, -12.0),
            geom(&[], &moon, &[], MapKind::Space),
            &r,
            Mode::Skirmish,
            &mut rng,
        );
        assert!(out.tunnelled && out.moon_impact);
        assert_eq!(out.self_damage, r.ship.max_hp);
        assert!(ship.pos.z < 0.0, "stopped on the approach side");
    }

    #[test]
    fn a_ship_is_pushed_out_of_a_mothership_hull() {
        let r = rules();
        let mut rng = Rng::new(5);
        let boxes = [BoxVolume {
            pos: Vec3::new(0.0, 0.0, -600.0),
            half: r.world.mothership_half,
        }];
        let mut ship = new_ship();
        // Just above the hull's top face, overlapping it.
        ship.pos = Vec3::new(0.0, r.world.mothership_half.y + 1.0, -600.0);
        ship.vel = -Vec3::Y * 50.0;
        let out = resolve_static(
            &mut ship,
            geom(&[], &[], &boxes, MapKind::Space),
            &r,
            Mode::Skirmish,
            &mut rng,
        );
        assert!(out.box_impact);
        assert_eq!(out.self_damage, 0, "hulls bounce, they do not hurt");
        assert!(ship.pos.y >= r.world.mothership_half.y + r.ship.collide_radius - 1e-9);
        assert!(ship.vel.y > 0.0);
    }

    #[test]
    fn a_ship_inside_a_hull_leaves_through_the_nearest_face() {
        let r = rules();
        let mut rng = Rng::new(6);
        let boxes = [BoxVolume {
            pos: Vec3::ZERO,
            half: r.world.mothership_half,
        }];
        let mut ship = new_ship();
        // Deep inside, but nearest the +y face (half-extents are 45/18/35).
        ship.pos = Vec3::new(0.0, 16.0, 0.0);
        resolve_static(
            &mut ship,
            geom(&[], &[], &boxes, MapKind::Space),
            &r,
            Mode::Skirmish,
            &mut rng,
        );
        assert!(ship.pos.y > r.world.mothership_half.y, "{:?}", ship.pos);
        assert_eq!(ship.pos.x, 0.0);
        assert_eq!(ship.pos.z, 0.0);
    }

    #[test]
    fn terrain_contact_is_edge_triggered_and_fatal() {
        let r = rules();
        let mut rng = Rng::new(8);
        let mut ship = new_ship();
        let (x, z) = (600.0, 200.0);
        let kill_y = terrain_kill_altitude(x, z, &r);
        ship.pos = Vec3::new(x, kill_y - 5.0, z);
        ship.vel = Vec3::new(0.0, -60.0, 0.0);

        let out = resolve_static(
            &mut ship,
            geom(&[], &[], &[], MapKind::Terrain),
            &r,
            Mode::Skirmish,
            &mut rng,
        );
        assert!(out.terrain_impact);
        assert_eq!(out.self_damage, r.ship.max_hp);
        assert!(
            (ship.pos.y - kill_y).abs() < 1e-9,
            "clamped to the kill plane"
        );
        // TERRAIN_BOUNCE is -0.5: half the descent, upward.
        assert!((ship.vel.y - 30.0).abs() < 1e-9);
        assert!(ship.touching_ground);

        // Staying there does not charge again.
        let out = resolve_static(
            &mut ship,
            geom(&[], &[], &[], MapKind::Terrain),
            &r,
            Mode::Skirmish,
            &mut rng,
        );
        assert!(!out.terrain_impact);
        assert_eq!(out.self_damage, 0);
    }

    #[test]
    fn a_short_step_samples_the_terrain_exactly_once() {
        // The ordinary case must be arithmetically identical to the JS.
        assert_eq!(terrain_sample_count(0.0), 1);
        assert_eq!(terrain_sample_count(TERRAIN_SWEEP_SPACING), 1);
        assert_eq!(terrain_sample_count(f64::NAN), 1);
        assert_eq!(terrain_sample_count(1e9), TERRAIN_SWEEP_MAX_SAMPLES);
        assert_eq!(terrain_sample_count(TERRAIN_SWEEP_SPACING * 2.5), 3);
    }

    #[test]
    fn terrain_is_ignored_on_the_space_map() {
        let r = rules();
        let mut rng = Rng::new(9);
        let mut ship = new_ship();
        ship.pos = Vec3::new(600.0, -900.0, 200.0);
        let out = resolve_static(
            &mut ship,
            geom(&[], &[], &[], MapKind::Space),
            &r,
            Mode::Skirmish,
            &mut rng,
        );
        assert!(!out.terrain_impact);
        assert_eq!(ship.pos.y, -900.0);
    }

    #[test]
    fn the_tutorial_suppresses_collision_damage_but_not_the_physics() {
        let r = rules();
        let mut rng = Rng::new(10);
        let rocks = [rock(0, Vec3::new(0.0, 0.0, 10.0), 8.0)];
        let mut ship = new_ship();
        ship.pos = Vec3::new(0.0, 0.0, 5.0);
        ship.vel = Vec3::Z * 60.0;
        let out = resolve_static(
            &mut ship,
            geom(&rocks, &[], &[], MapKind::Space),
            &r,
            Mode::Tutorial,
            &mut rng,
        );
        assert_eq!(out.self_damage, 0);
        assert_eq!(out.asteroid_impacts, 1, "the impact is still reported");
        assert!(ship.vel.z < 0.0, "and the bounce still happens");
    }

    #[test]
    fn a_dead_ship_is_not_collided() {
        let r = rules();
        let mut rng = Rng::new(11);
        let rocks = [rock(0, Vec3::new(0.0, 0.0, 10.0), 8.0)];
        let mut ship = new_ship();
        ship.alive = false;
        ship.pos = Vec3::new(0.0, 0.0, 5.0);
        let out = resolve_static(
            &mut ship,
            geom(&rocks, &[], &[], MapKind::Space),
            &r,
            Mode::Skirmish,
            &mut rng,
        );
        assert_eq!(out, CollisionReport::default());
        assert_eq!(ship.pos, Vec3::new(0.0, 0.0, 5.0));
    }

    #[test]
    fn collision_damage_still_rolls_for_an_invulnerable_ship() {
        // The RNG stream must advance identically whether or not the ship was
        // protected, or two clients disagree about every later draw.
        let r = rules();
        let rocks = [rock(0, Vec3::new(0.0, 0.0, 10.0), 8.0)];

        let mut a = new_ship();
        a.pos = Vec3::new(0.0, 0.0, 5.0);
        let mut rng_a = Rng::new(42);
        resolve_static(
            &mut a,
            geom(&rocks, &[], &[], MapKind::Space),
            &r,
            Mode::Skirmish,
            &mut rng_a,
        );

        let mut b = new_ship();
        b.invuln_timer = 2.0;
        b.pos = Vec3::new(0.0, 0.0, 5.0);
        let mut rng_b = Rng::new(42);
        resolve_static(
            &mut b,
            geom(&rocks, &[], &[], MapKind::Space),
            &r,
            Mode::Skirmish,
            &mut rng_b,
        );

        assert_eq!(
            rng_a, rng_b,
            "the stream must not depend on invulnerability"
        );
    }

    #[test]
    fn collision_damage_stays_inside_its_range() {
        let r = rules();
        let mut rng = Rng::new(0xC0111DE);
        let mut seen_min = false;
        let mut seen_max = false;
        for _ in 0..4000 {
            let d = roll_asteroid_collision_damage(&r, &mut rng);
            assert!((15..=29).contains(&d), "rolled {d}");
            seen_min |= d == 15;
            seen_max |= d == 29;
        }
        assert!(
            seen_min && seen_max,
            "the range endpoints must be reachable"
        );
    }

    // -- Determinism ------------------------------------------------------

    #[test]
    fn a_whole_flight_is_bit_reproducible() {
        let r = rules();
        let fly = || {
            let mut ship = new_ship();
            let mut input = idle_input();
            for i in 0..600 {
                input.throttle_axis = 1.0;
                input.steer_x = (f64::from(i % 37) / 37.0) * 2.0 - 1.0;
                input.steer_y = (f64::from(i % 23) / 23.0) * 2.0 - 1.0;
                input.roll = if i % 11 == 0 { 1.0 } else { 0.0 };
                input.boost = i % 7 < 3;
                input.braking = i % 53 < 9;
                integrate(&mut ship, &input, &r, Mode::Skirmish, TICK_DT);
            }
            ship
        };
        let a = fly();
        let b = fly();
        assert_eq!(
            a.pos.to_array().map(f64::to_bits),
            b.pos.to_array().map(f64::to_bits)
        );
        assert_eq!(
            a.vel.to_array().map(f64::to_bits),
            b.vel.to_array().map(f64::to_bits)
        );
        assert_eq!(
            a.quat.to_array().map(f64::to_bits),
            b.quat.to_array().map(f64::to_bits)
        );
    }

    #[test]
    fn the_pose_stays_finite_and_normalized_under_heavy_input() {
        let mut ship = new_ship();
        let mut input = idle_input();
        input.steer_x = 1.0;
        input.steer_y = -1.0;
        input.roll = -1.0;
        input.throttle_axis = 1.0;
        input.boost = true;
        run(&mut ship, &input, 5000);
        assert!(ship.pos.is_finite());
        assert!(ship.vel.is_finite());
        assert!(ship.quat.is_finite());
        let q = ship.quat;
        let len_sq = q.x * q.x + q.y * q.y + q.z * q.z + q.w * q.w;
        assert!((len_sq - 1.0).abs() < 1e-12, "quaternion drifted: {len_sq}");
    }
}
