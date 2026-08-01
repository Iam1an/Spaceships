//! Aim assist: a bounded rotational pull toward the shot the player is
//! *trying* to take.
//!
//! Port of `applyAimAssist` (`main.js:2054`–`:2149`) and the constants above it
//! (`main.js:1000`–`:1008`, now [`crate::rules::AimAssistRules`]). Once a tick,
//! for the local player only, this picks the best enemy inside a cone, solves
//! where a bullet fired now would meet them, and rotates [`Ship::quat`] a little
//! way toward that point.
//!
//! # Why this is in `sim` and not in the client
//!
//! Because it moves the ship. The pull is a rotation of the same quaternion the
//! flight model integrates, so a client-side copy would be a second flight model
//! — the browser would fly one ship and the server would simulate a different
//! one, and every prediction correction would fight the assist. It is
//! simulation, and it lives with the rest of the simulation.
//!
//! # How it composes
//!
//! [`crate::ship::integrate`] applies pitch, yaw and roll by *post*-multiplying,
//! because those are rotations about the ship's own axes. This pull is about a
//! world-space axis — the one perpendicular to the plane containing the nose and
//! the intercept point — so it *pre*-multiplies, exactly as `main.js:2148`
//! (`ship.quaternion.premultiply(...)`) does. `ship.rs`'s own docs name this
//! module's composition rule under "What this does not do".
//!
//! # The one bug fixed rather than ported
//!
//! `main.js:2074` passes `shipVelocity` as the shooter velocity to
//! `solveIntercept`, which solves for a projectile that carries the shooter's
//! momentum. A bolt in this game carries none — `bullets.js:44` gives it
//! `direction * SPEED` and nothing else — so the assist over-leads, and the
//! error grows with the player's own speed until a fast head-on pass is aimed at
//! empty space. `bot.js:172` passes a zero vector and has always been right.
//! [`crate::math::solve_intercept`] is now the single copy both callers use, and
//! this one passes [`Vec3::ZERO`]. Pinned by
//! `the_shooters_own_velocity_never_enters_the_intercept_solve`.
//!
//! # Deliberate differences from the JS
//!
//! - **Coarse-aim pilots have no off switch.** `main.js:996` initialises
//!   `aimAssistEnabled` to `true` for the keyboard, mobile and no-mouse schemes
//!   and then lets `C` toggle it like anyone else. Here the force is a rule, not
//!   an initial value: [`update`] rewrites
//!   [`AimAssistState::enabled`] to `true` for [`AimProfile::Coarse`] every
//!   tick, which also survives a snapshot, a rejoin, and a mid-match scheme
//!   change — none of which an initialiser does. `C` remains live for mouse
//!   pilots, which is the only scheme where turning assist off is a choice
//!   rather than an accident.
//! - **Candidates are filtered by [`World::can_damage`], not just by `alive`
//!   and team.** That adds spawn protection to the test, so the assist does not
//!   drag your nose onto a ship you cannot hurt yet. It is the same call
//!   `bot.rs` makes for the same reason (defect 3 in its module docs).
//! - **A blocked line of sight includes standing inside the blocker.**
//!   `raySphereDist` (`main.js:1080`) falls through to the far root when the
//!   origin is inside a sphere, so a player embedded in a rock still has line of
//!   sight through it. [`crate::collision::swept_sphere_sphere`] reports contact
//!   at zero, which reads as blocked. `bullets.rs` resolved the identical
//!   difference the same way.
//! - **`hasTarget` is not consulted.** `main.js:2071` skips remote ships that
//!   have never sent a pose; `bullets.rs` dropped that filter too, because
//!   `activateBossPhase` sets the flag on one boss hitbox out of twenty and
//!   hides the rest from everything that reads it.
//!
//! # Determinism
//!
//! - [`World::ships`], [`World::asteroids`] and [`World::obstacles`] are `Vec`s
//!   walked front to back, in that order. The winner is chosen with a strict
//!   `>`, so equal scores go to the earlier entry — insertion order, the same
//!   tie-break `bot::pick_target` documents. Nothing hashed is read.
//! - The only transcendental on this path is one [`det::acos`] per tick, plus
//!   the [`det::sin`]/[`det::cos`] pair inside
//!   [`crate::math::quat_from_axis_angle`] and one [`det::exp_neg`] per damp.
//!   All are the hand-rolled deterministic versions; libm is not called. The
//!   cone test, the sticky bonus and the winner comparison all stay in cosine
//!   space where the JS put them, so no inverse trig is involved in *choosing* a
//!   target. `acos` appears only where the behaviour genuinely depends on an
//!   angle: the falloff ramp between
//!   [`AimAssistTuning::dead_angle`] and [`AimAssistTuning::falloff_start`] is
//!   linear in radians, and no cosine comparison reproduces it.
//! - No randomness, no clock, no allocation.
//!
//! [`Ship::quat`]: crate::world::Ship::quat
//! [`AimAssistState::enabled`]: crate::world::AimAssistState::enabled

use crate::collision::{swept_sphere_sphere, Sphere};
use crate::math::{
    det, forward, quat_from_axis_angle, quat_mul, quat_normalize, solve_intercept, Vec3,
};
use crate::rules::{AimAssistTuning, AimProfile};
use crate::world::{EntityId, World};

/// Squared length below which `cross(nose, target)` carries no usable rotation
/// axis: the nose is already on the target, or exactly opposite it. `bot.rs`
/// uses the same threshold for the same degeneracy.
const AXIS_EPSILON_SQ: f64 = 1e-6;

/// Runs one tick of aim assist for [`World::local_id`].
///
/// `steer_mag` is `max(|steer_x|, |steer_y|)` from
/// [`crate::ship::FlightStep::steer`] — the steering the flight model actually
/// applied, after the deadzone, the response curve and the arrow-key ramp, which
/// is what `main.js:1257` reads. It is the release: the harder the player is
/// steering, the weaker the pull, so nobody is ever fought for control of their
/// own nose.
///
/// Writes [`crate::world::AimAssistState`] and, when it has a target it is
/// engaged on, premultiplies the local ship's orientation. Does nothing at all
/// on a world with no local player, which is every headless server.
pub fn update(world: &mut World, steer_mag: f64, dt: f64) {
    let Some(id) = world.local_id else {
        return;
    };
    let Some(ship) = world.ship(id) else {
        return;
    };
    let profile = ship.aim_profile();
    let alive = ship.alive;
    let self_pos = ship.pos;
    let fwd = forward(ship.quat);

    // `main.js:996`, hoisted from an initialiser to a rule — see the module
    // docs. A pilot aiming with arrow keys or a thumb does not get to switch
    // this off by leaning on `C`.
    if profile == AimProfile::Coarse {
        world.aim_assist.enabled = true;
    }
    if !world.aim_assist.enabled {
        // The JS simply does not call `applyAimAssist` (`main.js:1262`), which
        // freezes the whole state. Frozen is fine for the smoothing, but the
        // engagement flag also drives the HUD lock, and a disabled assist must
        // not draw one.
        world.aim_assist.has_target = false;
        return;
    }
    if !alive {
        // `main.js:2055`. The held target id survives, so respawning next to
        // whoever killed you reacquires them rather than starting cold.
        world.aim_assist.strength_smoothed = 0.0;
        world.aim_assist.has_target = false;
        return;
    }

    let rules = world.rules.aim_assist;
    let tuning = *rules.tuning(profile);

    // --- Intent. `main.js:2060`. -------------------------------------------
    // Squared, so the release is gentle at first and then decisive.
    let intent_damp = (1.0 - steer_mag / tuning.intent_break).max(0.0);
    let intent_factor = intent_damp * intent_damp;
    if intent_factor <= 0.0 {
        world.aim_assist.strength_smoothed = damp(
            world.aim_assist.strength_smoothed,
            0.0,
            rules.strength_damp_rate,
            dt,
        );
        world.aim_assist.has_target = false;
        return;
    }

    // --- Candidate selection. `main.js:2067`–`:2112`. ----------------------
    let bullet_speed = world.rules.weapons.bullet_speed;
    let held = world.aim_assist.target;
    let mut best_dot = tuning.cone_dot;
    let mut best: Option<(EntityId, Vec3)> = None;

    for other in &world.ships {
        if !world.can_damage(id, other) {
            continue;
        }

        // `Vec3::ZERO`, never the shooter's own velocity. See the module docs.
        let lead = match solve_intercept(other.pos, other.vel, self_pos, Vec3::ZERO, bullet_speed) {
            Some(t) if t > 0.0 && t.is_finite() => other.pos.add_scaled(other.vel, t),
            // No solution: aim at the target itself, which is still better than
            // not offering a pull at all. `main.js:2078`.
            _ => other.pos,
        };

        let to = lead - self_pos;
        let dist = to.length();
        if dist > rules.range || dist < rules.min_range || dist <= 0.0 {
            continue;
        }
        if line_of_sight_blocked(world, self_pos, to) {
            continue;
        }

        let mut dot = fwd.dot(to / dist);
        // The stickiness. Without it two candidates a hair apart trade the
        // pull every tick and the assist shakes between them.
        if held == Some(other.id) {
            dot += tuning.sticky_dot_bonus;
        }
        // Strict `>`: ties keep the earlier ship in `World::ships`.
        if dot > best_dot {
            best_dot = dot;
            best = Some((other.id, lead));
        }
    }

    // --- Engagement. `main.js:2113`–`:2128`. -------------------------------
    let presence = if best.is_some() { 1.0 } else { 0.0 };
    let state = &mut world.aim_assist;
    state.strength_smoothed = damp(
        state.strength_smoothed,
        presence,
        rules.strength_damp_rate,
        dt,
    );

    let Some((target_id, lead)) = best else {
        state.has_target = false;
        state.target = None;
        return;
    };

    let to = (lead - self_pos).normalize();
    if !state.has_target || state.target != Some(target_id) {
        // A fresh target snaps: smoothing in from wherever the last one was
        // would sweep the nose across everything in between.
        state.target_dir = to;
    } else {
        state.target_dir = state
            .target_dir
            .lerp(to, 1.0 - det::exp_neg(rules.dir_track_rate * dt))
            .normalize();
    }
    state.has_target = true;
    state.target = Some(target_id);

    if state.strength_smoothed < rules.engage_epsilon {
        return;
    }
    let dir = state.target_dir;
    let strength = state.strength_smoothed;

    // --- The pull. `main.js:2130`–`:2148`. ---------------------------------
    let Some(step) = pull_angle(fwd, dir, &tuning, strength, intent_factor, dt) else {
        return;
    };
    let axis = fwd.cross(dir);
    if axis.length_squared() < AXIS_EPSILON_SQ {
        return;
    }
    let rot = quat_from_axis_angle(axis.normalize(), step);
    if let Some(ship) = world.ship_mut(id) {
        // Premultiplied: a world-space correction, not a control input.
        ship.quat = quat_normalize(quat_mul(rot, ship.quat));
    }
}

/// How far to rotate the nose this tick, or `None` if the answer is "not at
/// all".
///
/// `main.js:2130`–`:2143`. Three things shape it:
///
/// 1. **The dead angle.** Inside it the shot is already good and the assist
///    keeps its hands off, which is what stops a mouse pilot feeling the pull
///    fight their micro-corrections.
/// 2. **The falloff.** Between the dead angle and
///    [`AimAssistTuning::falloff_start`] the strength ramps linearly from zero
///    to full, so the pull fades out as it arrives rather than stopping dead.
/// 3. **The budget.** The step never exceeds the error itself, so the assist
///    cannot overshoot and oscillate no matter how large `dt` or `strength` is.
///
/// The angle is real work — the ramp in (2) is linear in radians and has no
/// cosine-space equivalent — so this is where [`det::acos`] is spent, once.
fn pull_angle(
    fwd: Vec3,
    dir: Vec3,
    tuning: &AimAssistTuning,
    strength: f64,
    intent_factor: f64,
    dt: f64,
) -> Option<f64> {
    // Both are unit by construction, so `THREE.Vector3.angleTo`'s division by
    // `sqrt(lenSq * lenSq)` is a division by one; the clamp is what it exists
    // for and is kept.
    let angle = det::acos(fwd.dot(dir).clamp(-1.0, 1.0));
    if angle <= tuning.dead_angle {
        return None;
    }
    let ramp = tuning.falloff_start - tuning.dead_angle;
    let strength_mult = if angle >= tuning.falloff_start {
        1.0
    } else {
        (angle - tuning.dead_angle) / ramp
    };
    let budget = tuning.strength * strength * strength_mult * intent_factor * dt;
    Some((angle - tuning.dead_angle).min(budget))
}

/// Whether anything solid sits between `origin` and `origin + offset`.
///
/// `main.js:2084`–`:2103`: asteroids first, then [`World::obstacles`] — the
/// moon. Motherships and airfields are not tested, because the JS never handed
/// them to the assist either; flying at an enemy through a spawn platform is
/// rare enough that adding the boxes is a balance change, not a fix.
///
/// The JS normalises the direction and compares `raySphereDist(...) < dist`.
/// This passes the whole offset as the sweep's motion, which is the same
/// segment with one rounding step fewer, and asks only whether contact happens
/// within it.
fn line_of_sight_blocked(world: &World, origin: Vec3, offset: Vec3) -> bool {
    for a in &world.asteroids {
        if swept_sphere_sphere(origin, offset, 0.0, Sphere::new(a.pos, a.radius)).is_some() {
            return true;
        }
    }
    for o in &world.obstacles {
        if swept_sphere_sphere(origin, offset, 0.0, Sphere::new(o.pos, o.radius)).is_some() {
            return true;
        }
    }
    false
}

/// `THREE.MathUtils.damp(x, y, lambda, dt)`, built on [`det::exp_neg`].
///
/// The interpolation is written `(1 - t) * x + t * y` to match
/// `THREE.MathUtils.lerp`, which is the one the JS `damp` calls; the vector
/// smoothing above uses [`Vec3::lerp`], which is `THREE.Vector3.lerp`'s
/// `x + (y - x) * t`. The two differ in the last bit and the JS uses each in a
/// different place, so both spellings are kept.
#[inline]
fn damp(x: f64, y: f64, lambda: f64, dt: f64) -> f64 {
    let t = 1.0 - det::exp_neg(lambda * dt);
    (1.0 - t) * x + t * y
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::Rules;
    use crate::world::{
        Asteroid, AsteroidTier, MapKind, Mode, Obstacle, Quat, Ship, ShipKind, Team,
    };

    const SEED: u64 = 0xA551_5757;
    const DT: f64 = 1.0 / 60.0;

    fn v(x: f64, y: f64, z: f64) -> Vec3 {
        Vec3::new(x, y, z)
    }

    /// A world with nothing in it but the ships the test adds.
    ///
    /// The terrain map, for the reason `bot.rs` uses it: [`MapKind::Space`] puts
    /// an 80-unit moon at the origin and every fixture here aims through the
    /// origin. The two airfield boxes it does add are at `z = ±1500`, and aim
    /// assist never looks at boxes.
    fn world() -> World {
        let mut w = World::new(SEED, Rules::DEFAULT, Mode::Skirmish, MapKind::Terrain);
        w.local_id = Some(1);
        w.aim_assist.enabled = true;
        w
    }

    /// Adds a ship, alive and out of its spawn window.
    fn add(w: &mut World, id: EntityId, kind: ShipKind, team: Team, pos: Vec3) -> usize {
        let rules = w.rules;
        let mut s = Ship::spawn(id, kind, pos, Quat::IDENTITY, &rules);
        s.team = Some(team);
        s.invuln_timer = 0.0;
        w.ships.push(s);
        w.ships.len() - 1
    }

    /// The local player at the origin, nose down `+z`, and nothing else.
    fn pilot() -> World {
        let mut w = world();
        add(&mut w, 1, ShipKind::Local, Team::Zero, Vec3::ZERO);
        w
    }

    /// A bearing `deg` degrees off the local ship's nose, in the `xz` plane.
    fn bearing(deg: f64, dist: f64) -> Vec3 {
        let rad = deg * std::f64::consts::PI / 180.0;
        v(det::sin(rad) * dist, 0.0, det::cos(rad) * dist)
    }

    /// An enemy at `dist` units, `deg` degrees off the nose.
    fn enemy_at(w: &mut World, id: EntityId, dist: f64, deg: f64) -> usize {
        add(w, id, ShipKind::Bot, Team::One, bearing(deg, dist))
    }

    fn rock(w: &mut World, pos: Vec3, radius: f64) {
        let id = w.asteroids.len() as u32;
        w.asteroids.push(Asteroid {
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

    /// The local ship's nose.
    fn nose(w: &World) -> Vec3 {
        forward(w.ship(1).unwrap().quat)
    }

    /// How far the nose moved over one call, in radians.
    fn pull_of(w: &mut World, steer_mag: f64) -> f64 {
        let before = nose(w);
        update(w, steer_mag, DT);
        det::acos(before.dot(nose(w)).clamp(-1.0, 1.0))
    }

    /// What `tick::hud_state` would put in `HudState::assist_target`.
    fn hud_target(w: &World) -> EntityId {
        w.aim_assist.locked_target().unwrap_or(-1)
    }

    // --- Selection: cone and range ----------------------------------------

    #[test]
    fn an_enemy_inside_the_cone_is_acquired() {
        let mut w = pilot();
        enemy_at(&mut w, 2, 300.0, 20.0);
        update(&mut w, 0.0, DT);
        assert_eq!(w.aim_assist.target, Some(2));
        assert!(w.aim_assist.has_target);
    }

    #[test]
    fn an_enemy_outside_the_cone_is_rejected() {
        // `cone_dot` 0.60 is a half-angle of 53.13 degrees. 70 is outside it and
        // 50 is inside, and the bearing is the only difference between the runs.
        let mut wide = pilot();
        enemy_at(&mut wide, 2, 300.0, 70.0);
        update(&mut wide, 0.0, DT);
        assert_eq!(
            wide.aim_assist.target, None,
            "70 degrees is outside a 53 degree cone"
        );

        let mut narrow = pilot();
        enemy_at(&mut narrow, 2, 300.0, 50.0);
        update(&mut narrow, 0.0, DT);
        assert_eq!(narrow.aim_assist.target, Some(2), "50 degrees is inside it");
    }

    #[test]
    fn the_coarse_cone_is_wider_than_the_precise_one() {
        // 57 degrees: outside the mouse pilot's 53.13, inside the keyboard
        // pilot's 60. This is the accessibility difference, and it is the only
        // legitimate reason two players see different assist geometry.
        for (coarse, want) in [(false, None), (true, Some(2))] {
            let mut w = pilot();
            w.ship_mut(1).unwrap().coarse_aim = coarse;
            enemy_at(&mut w, 2, 300.0, 57.0);
            update(&mut w, 0.0, DT);
            assert_eq!(w.aim_assist.target, want, "coarse_aim = {coarse}");
        }
    }

    #[test]
    fn an_enemy_beyond_max_range_is_rejected() {
        let mut far = pilot();
        enemy_at(&mut far, 2, 1200.0, 0.0);
        update(&mut far, 0.0, DT);
        assert_eq!(
            far.aim_assist.target, None,
            "1200 units is past the 1000 unit limit"
        );

        let mut near = pilot();
        enemy_at(&mut near, 2, 900.0, 0.0);
        update(&mut near, 0.0, DT);
        assert_eq!(near.aim_assist.target, Some(2), "900 units is inside it");
    }

    #[test]
    fn range_is_measured_to_the_lead_point_not_to_the_target() {
        // 980 units downrange and crossing fast enough that the intercept point
        // is past the limit even though the ship itself is not. The JS measures
        // `dist` from `_assistLead` (`main.js:2080`), and so does this.
        let mut w = pilot();
        let i = enemy_at(&mut w, 2, 980.0, 0.0);
        w.ships[i].vel = v(400.0, 0.0, 0.0);
        update(&mut w, 0.0, DT);
        assert_eq!(w.aim_assist.target, None);
    }

    #[test]
    fn a_teammate_a_corpse_and_a_protected_ship_are_never_candidates() {
        for setup in 0..3 {
            let mut w = pilot();
            let i = enemy_at(&mut w, 2, 300.0, 0.0);
            match setup {
                0 => w.ships[i].team = Some(Team::Zero),
                1 => w.ships[i].alive = false,
                _ => w.ships[i].invuln_timer = 2.0,
            }
            update(&mut w, 0.0, DT);
            assert_eq!(w.aim_assist.target, None, "setup {setup}");
        }
    }

    // --- Selection: line of sight -----------------------------------------

    #[test]
    fn an_asteroid_between_you_and_the_target_breaks_the_lock() {
        let mut w = pilot();
        enemy_at(&mut w, 2, 400.0, 0.0);
        rock(&mut w, v(0.0, 0.0, 200.0), 20.0);
        update(&mut w, 0.0, DT);
        assert_eq!(w.aim_assist.target, None, "the rock is on the line");

        // The same rock, slid clear of the line: the lock comes back.
        let mut clear = pilot();
        enemy_at(&mut clear, 2, 400.0, 0.0);
        rock(&mut clear, v(60.0, 0.0, 200.0), 20.0);
        update(&mut clear, 0.0, DT);
        assert_eq!(clear.aim_assist.target, Some(2));
    }

    #[test]
    fn a_rock_behind_the_target_does_not_break_the_lock() {
        // The blocker has to be *between*: the test is the segment to the lead
        // point, not an infinite ray.
        let mut w = pilot();
        enemy_at(&mut w, 2, 400.0, 0.0);
        rock(&mut w, v(0.0, 0.0, 600.0), 20.0);
        update(&mut w, 0.0, DT);
        assert_eq!(w.aim_assist.target, Some(2));
    }

    #[test]
    fn the_moon_breaks_the_lock_too() {
        // `World::obstacles` is the moon and nothing else, walked after the
        // asteroids (`main.js:2094`).
        let mut w = pilot();
        enemy_at(&mut w, 2, 400.0, 0.0);
        w.obstacles.push(Obstacle {
            pos: v(0.0, 0.0, 200.0),
            radius: 80.0,
        });
        update(&mut w, 0.0, DT);
        assert_eq!(w.aim_assist.target, None);

        w.obstacles[0].pos = v(300.0, 0.0, 200.0);
        update(&mut w, 0.0, DT);
        assert_eq!(w.aim_assist.target, Some(2));
    }

    // --- Selection: stickiness --------------------------------------------

    #[test]
    fn the_sticky_bonus_stops_the_target_flickering_between_close_candidates() {
        // Two enemies whose bearings straddle the nose by a hair. Ship 2 is
        // marginally better on the first tick and ship 3 marginally better on
        // every tick after — four degrees, well inside what 0.05 of dot buys.
        // Without the bonus the pull would change hands once per tick.
        let mut w = pilot();
        enemy_at(&mut w, 2, 300.0, 10.0);
        enemy_at(&mut w, 3, 300.0, 14.0);
        update(&mut w, 0.0, DT);
        assert_eq!(
            w.aim_assist.target,
            Some(2),
            "the closer bearing wins first"
        );

        for step in 0..30 {
            w.ships[1].pos = bearing(14.0, 300.0);
            w.ships[2].pos = bearing(10.0, 300.0);
            update(&mut w, 0.0, DT);
            assert_eq!(w.aim_assist.target, Some(2), "flickered on tick {step}");
        }
    }

    #[test]
    fn without_the_bonus_the_same_pair_does_flicker() {
        // The control for the test above: identical geometry, bonus zeroed.
        let mut w = pilot();
        w.rules.aim_assist.precise.sticky_dot_bonus = 0.0;
        enemy_at(&mut w, 2, 300.0, 10.0);
        enemy_at(&mut w, 3, 300.0, 14.0);
        update(&mut w, 0.0, DT);
        assert_eq!(w.aim_assist.target, Some(2));

        w.ships[1].pos = bearing(14.0, 300.0);
        w.ships[2].pos = bearing(10.0, 300.0);
        update(&mut w, 0.0, DT);
        assert_eq!(
            w.aim_assist.target,
            Some(3),
            "the bonus is what was holding 2"
        );
    }

    #[test]
    fn a_clearly_better_target_still_takes_the_lock() {
        // Stickiness is a tie-break, not a lock-in: it is worth 0.05 of dot and
        // a rival past that wins.
        let mut w = pilot();
        enemy_at(&mut w, 2, 300.0, 40.0);
        enemy_at(&mut w, 3, 300.0, 0.0);
        w.aim_assist.target = Some(2);
        w.aim_assist.has_target = true;
        update(&mut w, 0.0, DT);
        assert_eq!(w.aim_assist.target, Some(3));
    }

    #[test]
    fn ties_go_to_the_earlier_ship_in_insertion_order() {
        // Determinism: two identical candidates must resolve the same way on
        // every machine, and the rule is the one `bot::pick_target` documents.
        let mut w = pilot();
        enemy_at(&mut w, 7, 300.0, 12.0);
        enemy_at(&mut w, 4, 300.0, 12.0);
        update(&mut w, 0.0, DT);
        assert_eq!(w.aim_assist.target, Some(7));
    }

    // --- The lead solution -------------------------------------------------

    #[test]
    fn the_assist_leads_a_crossing_target() {
        let mut w = pilot();
        let i = enemy_at(&mut w, 2, 300.0, 0.0);
        let target_pos = w.ships[i].pos;
        let target_vel = v(200.0, 0.0, 0.0);
        w.ships[i].vel = target_vel;
        update(&mut w, 0.0, DT);

        let t = solve_intercept(target_pos, target_vel, Vec3::ZERO, Vec3::ZERO, 780.0).unwrap();
        let lead = target_pos.add_scaled(target_vel, t);
        assert!(lead.x > 0.0, "the aim point is ahead of the target");
        // The lead point is exactly reachable at bullet speed in `t`.
        assert!((lead.length() - 780.0 * t).abs() < 1e-9);
        // And it, not the ship, is what the assist points at.
        assert!(w.aim_assist.target_dir.abs_diff_eq(lead.normalize(), 1e-12));
        assert!(
            !w.aim_assist
                .target_dir
                .abs_diff_eq(target_pos.normalize(), 1e-6),
            "aiming at the ship rather than the lead point would be no assist at all"
        );
    }

    #[test]
    fn a_target_that_outruns_the_bullet_is_aimed_at_directly() {
        // `solve_intercept` returns `None` and `main.js:2078` falls back to the
        // target's own position rather than dropping the candidate.
        let mut w = pilot();
        let i = enemy_at(&mut w, 2, 300.0, 0.0);
        let target_pos = w.ships[i].pos;
        w.ships[i].vel = v(0.0, 0.0, 900.0);
        update(&mut w, 0.0, DT);
        assert_eq!(w.aim_assist.target, Some(2));
        assert!(w
            .aim_assist
            .target_dir
            .abs_diff_eq(target_pos.normalize(), 1e-12));
    }

    #[test]
    fn the_shooters_own_velocity_never_enters_the_intercept_solve() {
        // The JS bug, pinned. `main.js:2074` passes `shipVelocity` as the
        // shooter velocity, but `bullets.js:44` gives a bolt
        // `direction * SPEED` with no inheritance at all, so feeding the
        // shooter's motion in over-leads by an amount that grows with its own
        // speed. Flying fast must not move the aim point at all.
        let target_pos = v(0.0, 0.0, 300.0);
        let target_vel = v(200.0, 0.0, 0.0);

        let dir_at = |self_vel: Vec3| {
            let mut w = pilot();
            let i = add(&mut w, 2, ShipKind::Bot, Team::One, target_pos);
            w.ships[i].vel = target_vel;
            w.ships[0].vel = self_vel;
            update(&mut w, 0.0, DT);
            w.aim_assist.target_dir
        };

        let parked = dir_at(Vec3::ZERO);
        for speed in [v(0.0, 0.0, 80.0), v(-260.0, 0.0, 0.0), v(0.0, 136.0, 0.0)] {
            let moved = dir_at(speed);
            assert_eq!(
                moved.x.to_bits(),
                parked.x.to_bits(),
                "{speed:?} moved the aim point"
            );
            assert_eq!(
                moved.y.to_bits(),
                parked.y.to_bits(),
                "{speed:?} moved the aim point"
            );
            assert_eq!(
                moved.z.to_bits(),
                parked.z.to_bits(),
                "{speed:?} moved the aim point"
            );
        }

        // And the assertion can fail: the buggy solve really does give a
        // different answer for one of those velocities, so this is not vacuous.
        let bugged = solve_intercept(
            target_pos,
            target_vel,
            Vec3::ZERO,
            v(-260.0, 0.0, 0.0),
            780.0,
        )
        .unwrap();
        let bugged_dir = target_pos.add_scaled(target_vel, bugged).normalize();
        assert!(
            !bugged_dir.abs_diff_eq(parked, 1e-6),
            "the JS solve must differ here, or this test proves nothing"
        );
    }

    // --- The pull ----------------------------------------------------------

    #[test]
    fn the_pull_turns_the_nose_toward_the_target() {
        let mut w = pilot();
        enemy_at(&mut w, 2, 300.0, 40.0);
        let want = w.ships[1].pos.normalize();
        let before = nose(&w).dot(want);
        update(&mut w, 0.0, DT);
        let after = nose(&w).dot(want);
        assert!(after > before, "{after} should be nearer 1 than {before}");
    }

    #[test]
    fn the_pull_scales_down_with_deliberate_steering() {
        // `intent_break` is 1.8 for a mouse pilot and the damp is squared, so
        // steering at 0.9 leaves (1 - 0.5)^2 = a quarter of the pull.
        let mut idle = pilot();
        enemy_at(&mut idle, 2, 300.0, 40.0);
        let free = pull_of(&mut idle, 0.0);

        let mut steering = pilot();
        enemy_at(&mut steering, 2, 300.0, 40.0);
        let fought = pull_of(&mut steering, 0.9);

        assert!(free > 0.0, "an idle stick gets the full pull");
        assert!(fought > 0.0 && fought < free);
        assert!(
            (fought / free - 0.25).abs() < 1e-9,
            "expected a quarter of the pull, got {}",
            fought / free
        );
    }

    #[test]
    fn steering_past_the_break_point_releases_the_assist_entirely() {
        let mut w = pilot();
        enemy_at(&mut w, 2, 300.0, 40.0);
        update(&mut w, 0.0, DT);
        assert!(w.aim_assist.has_target);

        let moved = pull_of(&mut w, 1.8);
        assert_eq!(moved, 0.0, "a pilot at full deflection is not fought");
        assert!(!w.aim_assist.has_target, "and the HUD lock goes out");
        assert_eq!(
            w.aim_assist.target,
            Some(2),
            "but the memory of who survives, so settling the stick reacquires them"
        );
        assert_eq!(hud_target(&w), -1);
    }

    #[test]
    fn the_pull_never_overshoots_the_error() {
        // A step budget vastly larger than the error. The step is clamped to the
        // error itself (less the dead angle), so the nose arrives and stops
        // rather than swinging past and oscillating for the rest of the match.
        let dead = Rules::DEFAULT.aim_assist.precise.dead_angle;
        let mut w = pilot();
        enemy_at(&mut w, 2, 300.0, 30.0);
        w.aim_assist.strength_smoothed = 1.0;
        update(&mut w, 0.0, 10.0);

        let want = w.ships[1].pos.normalize();
        let residual = det::acos(nose(&w).dot(want).clamp(-1.0, 1.0));
        assert!(residual <= dead + 1e-12, "stopped {residual} rad short");
        assert!(
            nose(&w).cross(want).y > 0.0,
            "the nose came round the near way and did not pass the target"
        );
    }

    #[test]
    fn a_shot_already_on_target_is_left_alone() {
        // Inside `dead_angle` the assist keeps its hands off, which is what
        // stops a mouse pilot feeling it fight their own micro-corrections.
        let mut w = pilot();
        enemy_at(&mut w, 2, 300.0, 0.1);
        let moved = pull_of(&mut w, 0.0);
        assert_eq!(moved, 0.0);
    }

    #[test]
    fn the_falloff_weakens_the_pull_as_the_nose_arrives() {
        // Two errors either side of `falloff_start` (0.28 rad, about 16
        // degrees), with the same smoothed strength so only the ramp differs.
        let pull_at = |deg: f64| {
            let mut w = pilot();
            enemy_at(&mut w, 2, 300.0, deg);
            w.aim_assist.strength_smoothed = 1.0;
            w.aim_assist.has_target = true;
            w.aim_assist.target = Some(2);
            pull_of(&mut w, 0.0)
        };
        let full = pull_at(30.0);
        let ramped = pull_at(8.0);
        assert!(ramped < full, "{ramped} should be gentler than {full}");
    }

    // --- Enablement --------------------------------------------------------

    #[test]
    fn a_coarse_pilot_has_no_off_switch() {
        let mut w = pilot();
        w.aim_assist.enabled = false;
        w.ship_mut(1).unwrap().coarse_aim = true;
        enemy_at(&mut w, 2, 300.0, 20.0);
        update(&mut w, 0.0, DT);
        assert!(w.aim_assist.enabled, "the rule rewrites the flag");
        assert_eq!(w.aim_assist.target, Some(2));
    }

    #[test]
    fn a_disabled_assist_neither_pulls_nor_reports_a_lock() {
        let mut w = pilot();
        enemy_at(&mut w, 2, 300.0, 20.0);
        update(&mut w, 0.0, DT);
        assert_eq!(hud_target(&w), 2);

        w.aim_assist.enabled = false;
        let moved = pull_of(&mut w, 0.0);
        assert_eq!(moved, 0.0);
        assert_eq!(hud_target(&w), -1);
    }

    #[test]
    fn a_dead_pilot_gets_no_assist() {
        let mut w = pilot();
        enemy_at(&mut w, 2, 300.0, 20.0);
        update(&mut w, 0.0, DT);
        assert!(w.aim_assist.strength_smoothed > 0.0);

        w.ship_mut(1).unwrap().alive = false;
        let moved = pull_of(&mut w, 0.0);
        assert_eq!(moved, 0.0);
        assert_eq!(w.aim_assist.strength_smoothed, 0.0);
        assert_eq!(hud_target(&w), -1);
    }

    #[test]
    fn a_world_with_no_local_player_does_nothing() {
        // The headless server case: no `local_id`, no assist, no panic.
        let mut w = world();
        w.local_id = None;
        add(&mut w, 1, ShipKind::Remote, Team::Zero, Vec3::ZERO);
        enemy_at(&mut w, 2, 300.0, 0.0);
        let before = w.clone();
        update(&mut w, 0.0, DT);
        assert_eq!(w, before);
    }

    // --- Determinism -------------------------------------------------------

    #[test]
    fn a_run_of_ticks_is_bit_reproducible() {
        let run = || {
            let mut w = pilot();
            let a = enemy_at(&mut w, 2, 400.0, 18.0);
            let b = enemy_at(&mut w, 3, 380.0, 22.0);
            w.ships[a].vel = v(120.0, 5.0, -30.0);
            w.ships[b].vel = v(-90.0, 12.0, 40.0);
            rock(&mut w, v(140.0, 0.0, 260.0), 22.0);
            for i in 0..120 {
                update(&mut w, f64::from(i % 7) * 0.1, DT);
                for j in 1..w.ships.len() {
                    let vel = w.ships[j].vel;
                    w.ships[j].pos = w.ships[j].pos.add_scaled(vel, DT);
                }
            }
            w.aim_assist
        };
        let a = run();
        let b = run();
        assert_eq!(a.target, b.target);
        assert!(a.target.is_some(), "the fixture must actually engage");
        assert_eq!(a.strength_smoothed.to_bits(), b.strength_smoothed.to_bits());
        assert_eq!(a.target_dir.x.to_bits(), b.target_dir.x.to_bits());
        assert_eq!(a.target_dir.y.to_bits(), b.target_dir.y.to_bits());
        assert_eq!(a.target_dir.z.to_bits(), b.target_dir.z.to_bits());
    }
}
