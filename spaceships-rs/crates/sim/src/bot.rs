//! Bot decision-making: target selection, pursuit and evasion, obstacle
//! avoidance, and weapon use.
//!
//! This is the port of `public/src/bot.js` (`createBotAI`) plus the bot half of
//! `main.js` (`spawnBot`, `applyHitToBot`, `reviveBotLocal`). A bot is a
//! [`Ship`] with [`ShipKind::Bot`]; everything the AI remembers between ticks
//! lives in [`BotState`], so a bot is a plain world entity rather than a
//! closure with private variables.
//!
//! # Three defects fixed rather than ported
//!
//! **1. Bot bullets were simulated twice.** `bot.js:301` (`fireBullet`) spawned
//! a *visual* bolt through `bullets.fire(...)` **and** pushed a shadow
//! projectile into a private `myProjectiles` array, stepped by `bot.js:314`
//! (`updateProjectiles`). The visual bolt could never damage anything, because
//! `bullets.js:95`/`:137` gate both the asteroid and the ship test on
//! `b.isLocal`, and `bullets.js:37` sets `isLocal` only for
//! `faction === 'self'` — a bot fires as `'ally'` or `'enemy'`. So the bolt the
//! player saw (effective radius 6.5) was never the one that hit them (radius
//! 4.0, no swept test, different collision order). Here [`fire_bullet`] calls
//! [`crate::bullets::spawn_bullet`] with [`crate::bullets::BulletSpawn::gun`] —
//! the player's own launcher — so exactly one [`crate::world::Bullet`] lands in
//! [`World::bullets`] and `bullets.rs` steps and resolves it. This module
//! contains no ballistics at all.
//!
//! **2. Bots used their own ship hit radius.** `bot.js:31`+`:52` computed
//! `BULLET_HIT_R = SHIP_RADIUS + 0.5 = 4.0` against the player's 6.0/7.0, so a
//! bot had to close roughly 35 % nearer than a player for identical geometry.
//! There is no radius in this module: the spawned [`crate::world::Bullet`]
//! carries `owner_coarse_aim`, and `bullets.rs` asks [`Ship::hit_radius`],
//! which reads
//! [`crate::rules::ShipRules::hit_radius`].
//!
//! **3. Bots got no spawn invulnerability.** `main.js:3199` (`applyHitToBot`)
//! checked only `!r.alive`, and `spawnBot` never gave a bot record an
//! `invulnUntil`, while `server/index.js:942` gates every player hit on one.
//! [`Ship::invuln_timer`] now exists on every ship, so a bot is protected by
//! the same field the player is. The consequence *inside this module* is that
//! the gun and missile gates ask [`World::can_damage`] before firing, so a bot
//! pursues a freshly respawned enemy but holds fire until the enemy can
//! actually be hurt, instead of emptying a magazine into an immune target.
//!
//! # Determinism
//!
//! - Every collection this module reads is a `Vec` walked front to back
//!   ([`World::ships`], [`World::asteroids`], [`World::obstacles`],
//!   [`World::missiles`], [`World::flares`]). Target selection is the classic
//!   place iteration order leaks into behaviour — "nearest opponent" has ties,
//!   and the tie-break here is insertion order, exactly as the JS `Map` walk
//!   was. No hashed container is used or accepted.
//! - Every `Math.random()` in `bot.js` (`chooseEvadeDir` at `:57`, the aim
//!   wander at `:167`, the missile delay at `:25`) is routed through
//!   [`crate::world::WorldRng::bots`], the stream reserved for bot decisions.
//! - **No libm transcendental is called on a simulation path.** `bot.js` needs
//!   `sin`/`cos` (`setFromAxisAngle`), `acos` (`angleTo`) and `exp`
//!   (`MathUtils.damp`), none of which are guaranteed bit-identical across
//!   platforms or libm versions. All three are replaced in [`dmath`] by series
//!   built from `+ - * /` only, which IEEE-754 requires to be correctly
//!   rounded. `acos` is avoided outright: the "is the required turn larger than
//!   this tick's budget" test is done as a cosine comparison, and the
//!   unclamped rotation is built with the trig-free half-angle identity.
//!
//! # What this module deliberately does not do
//!
//! Ballistics, damage resolution, missile homing, flare bodies, respawn
//! scheduling, and campaign flow all belong to other modules. This one decides,
//! and writes its decisions into the shared state: it appends to
//! [`World::bullets`] and [`World::missiles`], reports through [`SimEvent`],
//! and moves its own ship.
//!
//! The bot flight model *is* implemented here, because it is not the player's.
//! `bot.js` never touches the ship physics in `main.js`: it turns its
//! quaternion directly at a fixed rate, chases a fixed cruise speed, and pushes
//! itself out of rocks. Folding that into `ship.rs` would give bots the
//! player's throttle, drift, boost, and brake model and change every
//! engagement.

use std::f64::consts::PI;

use crate::bullets::{spawn_bullet, BulletSpawn};
use crate::math::{
    quat_from_axis_angle as axis_angle, quat_mul, quat_normalize, solve_intercept, Vec3,
};
use crate::rng::Rng;
use crate::rules::Rules;
use crate::world::{
    BotFsm, BotState, EntityId, Missile, MissileTarget, Quat, Ship, ShipKind, SimEvent, Team,
    WeaponKind, World,
};

/// The deterministic transcendentals, under the name this module has always
/// used for them.
///
/// They were defined here, in a private `dmath` module, while `math` was
/// read-only to wave-1 agents. `missiles.rs` independently hand-rolled `acos`
/// and `pow` for the same reason. All of it now lives in [`crate::math::det`],
/// unchanged — [`crate::math::det::sin`] and [`crate::math::det::cos`] are
/// bit-identical to the versions this module shipped over the `[0, π]` domain
/// it uses — and this alias keeps every call site and test spelling intact.
use crate::math::det as dmath;

// ---------------------------------------------------------------------------
// Terrain
// ---------------------------------------------------------------------------

/// Ground height under a world-space `(x, z)`.
///
/// `bot.js` takes this as the `terrainHeightFn` dependency (`main.js:2494`
/// passes `getTerrainHeight` on the terrain map and `null` in space) and uses
/// it twice: to pull the nose up when clearance runs out (`bot.js:195`) and as
/// a hard floor after integration (`bot.js:262`).
///
/// **Gap:** [`World`] has no heightfield. `terrain.js` is a pure function of
/// `(x, z)` — seven `sin`/`cos` octaves plus an airfield flattening blend — and
/// nothing in this crate owns it yet. Until it lands, the caller supplies it;
/// when it does, this trait should be deleted and the height read from the
/// world. Note that a `sin`-based heightfield is itself a determinism hazard
/// and will need the same treatment [`dmath`] gives the rotation math.
pub trait TerrainHeight {
    /// Ground height at `(x, z)`.
    fn height_at(&self, x: f64, z: f64) -> f64;
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Arms a freshly spawned bot.
///
/// [`Ship::spawn`] leaves [`Ship::bot`] at its default, which is a bot with no
/// missiles that would fire one immediately if it had any. This is `spawnBot`'s
/// share of `main.js:2471`–`:2497`: difficulty, load-out, and the randomized
/// delay before the first missile.
///
/// `hard_mode` is the whole of the JS difficulty model (`main.js:2495`): a
/// three-times-faster gun ([`crate::rules::BotRules::fire_cooldown_hard`]) and
/// three missiles instead of one. Campaign wave bots and multiplayer balance
/// bots are always hard (`main.js:2999`). There is no separate aggression
/// parameter in the JS — ranges, lead quality, and aim wander are identical at
/// both difficulties.
pub fn init(ship: &mut Ship, hard_mode: bool, is_campaign_bot: bool, rules: &Rules, rng: &mut Rng) {
    ship.kind = ShipKind::Bot;
    ship.bot = BotState {
        fsm: BotFsm::Seek,
        state_timer: 0.0,
        fire_timer: 0.0,
        missiles_left: rules.bot.missile_max_for(hard_mode),
        missile_timer: missile_delay(rules, rng),
        stuck_time: 0.0,
        evade_axis: Vec3::ZERO,
        aim_offset: Vec3::ZERO,
        tracked_lead: Vec3::ZERO,
        tracked_lead_seeded: false,
        hard_mode,
        is_campaign_bot,
    };
}

/// Reacts to surviving a hit: break off along a fresh random axis.
///
/// `bot.js:358` (`notifyHit`), called from `main.js:3227` — and only from the
/// branch where the bot lived, so a killing blow does not queue an evade the
/// corpse would run out of on respawn.
pub fn notify_hit(ship: &mut Ship, rng: &mut Rng) {
    if !ship.alive {
        return;
    }
    if ship.bot.fsm != BotFsm::Evade {
        ship.bot.fsm = BotFsm::Evade;
        ship.bot.state_timer = 0.0;
        ship.bot.evade_axis = choose_evade_dir(rng);
    }
}

/// Resets a bot's AI after it respawns. `bot.js:366` (`notifyRespawn`).
///
/// Two additions to the JS, both of them stale state the JS carries across a
/// death:
///
/// - `tracked_lead_seeded` is cleared. `bot.js` keeps the smoothed intercept
///   point it was aiming at when it died, and after respawning somewhere else
///   entirely it spends the smoothing time constant dragging that point across
///   the map.
/// - `stuck_time` is cleared, so a bot that died while wedged in a rock does
///   not immediately force an evade at its new spawn.
pub fn notify_respawn(ship: &mut Ship, rules: &Rules, rng: &mut Rng) {
    ship.bot.fsm = BotFsm::Seek;
    ship.bot.state_timer = 0.0;
    ship.bot.fire_timer = 0.0;
    ship.bot.missiles_left = rules.bot.missile_max_for(ship.bot.hard_mode);
    ship.bot.missile_timer = missile_delay(rules, rng);
    ship.bot.stuck_time = 0.0;
    ship.bot.tracked_lead_seeded = false;
    ship.vel = Vec3::ZERO;
}

/// The randomized delay before a bot's first missile. `bot.js:25`
/// (`2.5 + Math.random() * 4.0`).
fn missile_delay(rules: &Rules, rng: &mut Rng) -> f64 {
    let lo = rules.bot.missile_delay_min;
    rng.range_f64(lo, lo + rules.bot.missile_delay_range)
}

// ---------------------------------------------------------------------------
// The tick
// ---------------------------------------------------------------------------

/// Runs one AI step for every [`ShipKind::Bot`] in `world`, in
/// [`World::ships`] order.
///
/// Bots are stepped **sequentially**, exactly as `main.js:1702` walks its
/// `bots` array: the second bot sees the first one's new position this tick,
/// not last tick's. Deciding for all of them and applying the results together
/// would be a different simulation.
///
/// `events` is appended to, never cleared, so a caller can thread one buffer
/// through a whole tick.
pub fn update_bots(
    world: &mut World,
    dt: f64,
    terrain: Option<&dyn TerrainHeight>,
    events: &mut Vec<SimEvent>,
) {
    // Taken out and put back so the read-only planning pass can borrow the rest
    // of the world while still drawing from the bot stream. Cloning an `Rng` is
    // two words and reproduces the identical sequence.
    let mut rng = world.rng.bots.clone();
    for i in 0..world.ships.len() {
        if world.ships[i].kind != ShipKind::Bot {
            continue;
        }
        let plan = plan_bot(world, i, dt, terrain, &mut rng);
        apply_plan(world, i, &plan, events);
    }
    world.rng.bots = rng;
}

/// One bullet or missile a bot decided to launch.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Shot {
    /// Muzzle position.
    origin: Vec3,
    /// Unit direction.
    dir: Vec3,
}

/// Everything one bot decided this tick. Produced by a read-only pass over the
/// world, so the mutable pass never has to alias another ship.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Plan {
    pos: Vec3,
    vel: Vec3,
    quat: Quat,
    bot: BotState,
    bullet: Option<Shot>,
    missile: Option<(Shot, EntityId)>,
    flare: bool,
}

impl Plan {
    /// A plan that changes nothing but the AI bookkeeping.
    fn hold(ship: &Ship, bot: BotState) -> Plan {
        Plan {
            pos: ship.pos,
            vel: ship.vel,
            quat: ship.quat,
            bot,
            bullet: None,
            missile: None,
            flare: false,
        }
    }
}

/// The read-only half of a bot's tick. `bot.js:130` (`update`), minus
/// `updateProjectiles`, which no longer exists.
fn plan_bot(
    world: &World,
    index: usize,
    dt: f64,
    terrain: Option<&dyn TerrainHeight>,
    rng: &mut Rng,
) -> Plan {
    let ship = &world.ships[index];
    let rules = &world.rules;
    let br = &rules.bot;

    let mut bot = ship.bot;
    if !ship.alive {
        // `bot.js:132` returns before advancing any timer, so a dead bot's
        // cooldowns are frozen rather than ticking down in the grave.
        return Plan::hold(ship, bot);
    }

    bot.state_timer += dt;
    bot.fire_timer -= dt;
    bot.missile_timer -= dt;

    let accel_t = 1.0 - dmath::exp_neg(br.accel * dt);

    let Some(target_id) = pick_target(world, ship) else {
        // `bot.js:137`: nothing to fight, so cruise straight ahead at a
        // fraction of cruise speed. The FSM is left alone.
        let fwd = forward(ship.quat);
        let want = fwd * (br.speed * br.idle_speed_fraction);
        let vel = ship.vel.lerp(want, accel_t);
        let mut plan = Plan::hold(ship, bot);
        plan.vel = vel;
        plan.pos = ship.pos.add_scaled(vel, dt);
        return plan;
    };
    let target = &world.ships[target_index(world, target_id)];

    let target_pos = target.pos;
    let target_vel = target.vel;
    // Captured before the bot moves. `bot.js` reads `dist` once, at the top of
    // the tick, and reuses it for both weapon range gates at the bottom.
    let dist = target_pos.distance(ship.pos);

    // --- FSM (`bot.js:151`) -------------------------------------------------
    match bot.fsm {
        BotFsm::Seek => {
            if dist < br.seek_dist {
                bot.fsm = BotFsm::Attack;
                bot.state_timer = 0.0;
            }
        }
        BotFsm::Attack => {
            if dist < br.attack_too_close {
                bot.fsm = BotFsm::Evade;
                bot.state_timer = 0.0;
                bot.evade_axis = choose_evade_dir(rng);
            } else if dist > br.seek_dist * br.seek_exit_multiplier {
                bot.fsm = BotFsm::Seek;
                bot.state_timer = 0.0;
            }
        }
        BotFsm::Evade => {
            if bot.state_timer >= br.evade_duration {
                bot.fsm = BotFsm::Seek;
                bot.state_timer = 0.0;
            }
        }
    }

    // --- Aim (`bot.js:167`) -------------------------------------------------
    // A random walk, clamped to a ball, scaled by range at the point of use.
    // This is the bot's deliberate inaccuracy; without it a bot holding a
    // perfect intercept solution never misses.
    bot.aim_offset += Vec3::new(
        (rng.next_f64() - 0.5) * br.aim_offset_drift * dt,
        (rng.next_f64() - 0.5) * br.aim_offset_drift * dt,
        (rng.next_f64() - 0.5) * br.aim_offset_drift * dt,
    );
    bot.aim_offset = bot.aim_offset.clamp_length(br.aim_offset_max);

    // The intercept solve. The shooter velocity is deliberately zero: a bullet
    // in this game is spawned with `direction * bullet_speed` and inherits
    // nothing from the shooter (`bullets.js:44`), so the correct relative
    // velocity is the target's alone. `bot.js:172` gets this right, and so does
    // [`crate::aim_assist`], which shares this solver — see
    // [`crate::math::solve_intercept`] for the JS call site that does not.
    let lead_t = solve_intercept(
        target_pos,
        target_vel,
        ship.pos,
        Vec3::ZERO,
        rules.weapons.bullet_speed,
    );
    let lead_point = match lead_t {
        Some(t) if t.is_finite() => target_pos.add_scaled(target_vel, t),
        _ => target_pos,
    };
    if bot.tracked_lead_seeded {
        bot.tracked_lead = bot
            .tracked_lead
            .lerp(lead_point, 1.0 - dmath::exp_neg(br.aim_track_rate * dt));
    } else {
        bot.tracked_lead = lead_point;
        bot.tracked_lead_seeded = true;
    }
    let error_scale = dist / br.aim_ref_dist;
    let aim_world = bot.tracked_lead.add_scaled(bot.aim_offset, error_scale);

    // --- Steering (`bot.js:184`) -------------------------------------------
    let mut desired = if bot.fsm == BotFsm::Evade {
        bot.evade_axis
    } else {
        (aim_world - ship.pos).normalize()
    };

    let fwd = forward(ship.quat);
    if let Some(avoid) = compute_avoidance(world, ship.pos, fwd) {
        desired = desired.add_scaled(avoid, br.avoid_weight).normalize();
    }

    if let Some(ground) = terrain {
        let below = ground.height_at(ship.pos.x, ship.pos.z);
        let ahead = ship
            .pos
            .add_scaled(fwd, br.speed * br.terrain_lookahead_seconds);
        let ahead_h = ground.height_at(ahead.x, ahead.z);
        let clearance = (ship.pos.y - below).min(ship.pos.y - ahead_h);
        if clearance < br.terrain_margin {
            let pull = (br.terrain_margin - clearance) / br.terrain_margin;
            desired.y += pull * br.terrain_pull;
            if desired.length() > 0.001 {
                desired = desired.normalize();
            }
        }
    }

    let quat = rotate_toward(ship.quat, fwd, desired, br.turn_rate * dt);

    // --- Kinematics (`bot.js:210`) -----------------------------------------
    let fwd = forward(quat);
    let mut vel = ship.vel.lerp(fwd * br.speed, accel_t);
    let mut pos = ship.pos.add_scaled(vel, dt);

    // --- Push-out (`bot.js:214` and `:237`) --------------------------------
    // `bot.js` resets the evade timer on *every* frame it overlaps an obstacle
    // (`:232`) but guards the asteroid case with `if (state !== 'evade')`
    // (`:255`), so a bot grinding along the moon could never leave the evade
    // state. Both are guarded here.
    let mut bumped = false;
    for o in &world.obstacles {
        bumped |= push_out(&mut pos, &mut vel, o.pos, o.radius, rules);
    }
    for a in &world.asteroids {
        bumped |= push_out(&mut pos, &mut vel, a.pos, a.radius, rules);
    }
    if bumped && bot.fsm != BotFsm::Evade {
        bot.fsm = BotFsm::Evade;
        bot.state_timer = 0.0;
        bot.evade_axis = choose_evade_dir(rng);
    }

    if let Some(ground) = terrain {
        let floor = ground.height_at(pos.x, pos.z) + br.terrain_min_clearance;
        if pos.y < floor {
            pos.y = floor;
            if vel.y < 0.0 {
                vel.y *= rules.combat.terrain_bounce;
            }
        }
    }

    // --- Stuck detection (`bot.js:272`) ------------------------------------
    let stuck_thresh = br.stuck_speed_threshold;
    if vel.length_squared() < stuck_thresh * stuck_thresh && bot.fsm != BotFsm::Evade {
        bot.stuck_time += dt;
        if bot.stuck_time >= br.stuck_time {
            bot.fsm = BotFsm::Evade;
            bot.state_timer = 0.0;
            bot.evade_axis = choose_evade_dir(rng);
            bot.stuck_time = 0.0;
        }
    } else {
        bot.stuck_time = 0.0;
    }

    // --- Weapons ------------------------------------------------------------
    // Defect 3: a bot no longer shoots at something it cannot hurt. Everything
    // else about the gates is `bot.js:283` and `:290`, including the detail
    // that the alignment tests use the *post-move* position (`botPos` is a live
    // reference to `record.ship.position` in the JS, mutated at `:213`) while
    // the range tests use the pre-move `dist`.
    let engageable = world.can_damage(ship.id, target);

    let mut bullet = None;
    if bot.fsm == BotFsm::Attack && bot.fire_timer <= 0.0 && dist < br.fire_range && engageable {
        let ideal = (aim_world - pos).normalize();
        if fwd.dot(ideal) > br.fire_dot {
            bullet = Some(Shot {
                origin: pos.add_scaled(fwd, br.muzzle_offset),
                dir: fwd,
            });
            bot.fire_timer = br.fire_cooldown_for(bot.hard_mode);
        }
    }

    let mut missile = None;
    if bot.missiles_left > 0
        && bot.fsm == BotFsm::Attack
        && bot.missile_timer <= 0.0
        && dist > br.missile_min_range
        && dist < br.missile_max_range
        && engageable
    {
        let to_target = (target_pos - pos).normalize();
        if fwd.dot(to_target) > br.missile_fire_dot {
            missile = Some((
                Shot {
                    origin: pos.add_scaled(fwd, rules.weapons.missile_spawn_offset),
                    dir: fwd,
                },
                target_id,
            ));
            bot.missiles_left -= 1;
            bot.missile_timer = br.missile_cooldown;
        }
    }

    let flare = wants_flare(world, ship, pos);

    Plan {
        pos,
        vel,
        quat,
        bot,
        bullet,
        missile,
        flare,
    }
}

/// The mutable half of a bot's tick: write the pose, spawn the projectiles,
/// report the events.
fn apply_plan(world: &mut World, index: usize, plan: &Plan, events: &mut Vec<SimEvent>) {
    let (id, team) = {
        let ship = &mut world.ships[index];
        ship.pos = plan.pos;
        ship.vel = plan.vel;
        ship.quat = plan.quat;
        ship.bot = plan.bot;
        // Locally driven bots are stored as remote records in the JS
        // (`getOrCreateRemote`, `main.js:2472`), and `bot.js:269` keeps their
        // interpolation targets pinned to the authoritative pose so the remote
        // interpolator is a no-op for them. Same here.
        ship.interp.target_pos = plan.pos;
        ship.interp.target_quat = plan.quat;
        ship.interp.has_target = true;
        if plan.flare {
            ship.flares_left = ship.flares_left.saturating_sub(1);
        }
        (ship.id, ship.team)
    };

    if let Some(shot) = plan.bullet {
        fire_bullet(world, index, shot, events);
    }
    if let Some((shot, target)) = plan.missile {
        fire_missile(world, id, team, shot, target, events);
    }
    if plan.flare {
        events.push(SimEvent::FlareBurst {
            owner: id,
            origin: plan.pos,
        });
    }
}

/// Fires one round from the bot at `index` through the *player's* launcher.
///
/// This is the whole of the defect-1 fix, and it is now literally one call.
/// `bot.js:301` spawned a render bolt *and* a shadow projectile; wave-1 got as
/// far as one list but still built the [`crate::world::Bullet`] record by hand,
/// duplicating [`BulletSpawn::gun`] field for field. It goes through
/// [`spawn_bullet`] instead, so speed, life, damage, the coarse-aim flag, the
/// key allocation and the `prev_pos == pos` invariant all come from the one
/// place the player's gun reads them, and a change there cannot leave bots
/// behind. No ballistics live in this module.
fn fire_bullet(world: &mut World, index: usize, shot: Shot, events: &mut Vec<SimEvent>) {
    let rules = world.rules;
    let spawn = BulletSpawn::gun(&rules, shot.origin, shot.dir, &world.ships[index]);
    let owner = spawn.owner;
    spawn_bullet(world, spawn);
    events.push(SimEvent::Fired {
        owner,
        weapon: WeaponKind::Bullet,
        origin: shot.origin,
        dir: shot.dir,
    });
}

/// Appends one missile to [`World::missiles`]. `main.js:2500` (`fireMissile`),
/// which spawns 6 units ahead of the nose and locks the chosen target.
fn fire_missile(
    world: &mut World,
    owner: EntityId,
    owner_team: Option<Team>,
    shot: Shot,
    target: EntityId,
    events: &mut Vec<SimEvent>,
) {
    let key = world.take_projectile_key();
    let life = world.rules.weapons.missile_life;
    world.missiles.push(Missile {
        key,
        pos: shot.origin,
        dir: shot.dir,
        target: Some(MissileTarget::Ship(target)),
        life,
        age: 0.0,
        owner,
        owner_team,
    });
    events.push(SimEvent::Fired {
        owner,
        weapon: WeaponKind::Missile,
        origin: shot.origin,
        dir: shot.dir,
    });
}

// ---------------------------------------------------------------------------
// Countermeasures
// ---------------------------------------------------------------------------

/// Whether the bot should release a flare burst this tick.
///
/// **This behaviour is new.** `bot.js` never deploys a flare — `deployFlare`
/// (`missiles.js:236`) is reachable only from the player's `Q` binding
/// (`main.js:1446`), so in the shipped game a missile fired at a bot always
/// connects while a missile fired at the player can be decoyed. Bots carry
/// [`Ship::flares_left`] like everyone else, so the asymmetry was never a rule,
/// just a missing decision.
///
/// The trigger is derived entirely from constants that already exist, because
/// [`crate::rules::BotRules`] has no countermeasure fields: a burst is released
/// when a missile that is *locked onto this bot* and *not its own* has closed
/// to [`crate::rules::WeaponRules::flare_seduction_dist`] — which is exactly
/// the range at which a flare can steal that lock (`missiles.js:316`).
/// Releasing earlier burns a charge on a missile the flare cannot reach.
///
/// The rate limit is likewise state-free: a bot will not release a second burst
/// while one of its own flares is still burning. [`BotState`] has no flare
/// timer to hold a cooldown in, and "a decoy is already up" is the condition a
/// timer would be approximating anyway.
///
/// Only the *decision* is made here. The burst bodies are the countermeasure
/// module's job — one `deployFlare` shared by players and bots, which is the
/// point of the port. This reports [`SimEvent::FlareBurst`] and spends the
/// charge.
fn wants_flare(world: &World, ship: &Ship, pos: Vec3) -> bool {
    if ship.flares_left == 0 {
        return false;
    }
    if world.flares.iter().any(|f| f.owner == ship.id) {
        return false;
    }
    let reach = world.rules.weapons.flare_seduction_dist;
    world.missiles.iter().any(|m| {
        m.owner != ship.id
            && m.target == Some(MissileTarget::Ship(ship.id))
            && m.pos.distance_squared(pos) <= reach * reach
    })
}

// ---------------------------------------------------------------------------
// Target selection
// ---------------------------------------------------------------------------

/// The nearest opponent, or `None` if the bot has nothing to fight.
/// `bot.js:119` (`pickTarget`).
///
/// Ties go to the earlier entry in [`World::ships`], which is insertion order —
/// the same tie-break the JS `Map` walk had, and the reason this must never
/// become a hashed lookup.
#[must_use]
pub fn pick_target(world: &World, bot: &Ship) -> Option<EntityId> {
    let mut best: Option<(EntityId, f64)> = None;
    for other in &world.ships {
        if !is_opponent(bot, other) {
            continue;
        }
        // Squared distance: `sqrt` is monotonic, so it cannot change the
        // ordering the JS `distanceTo` comparison produced.
        let d2 = other.pos.distance_squared(bot.pos);
        if best.is_none_or(|(_, b)| d2 < b) {
            best = Some((other.id, d2));
        }
    }
    best.map(|(id, _)| id)
}

/// Whether `other` is something `bot` would pursue.
///
/// Alive, not itself, not a team-mate, and not one of the boss's hitboxes. The
/// last is implicit in the JS: the solo `getOpponents` (`main.js:2510`) walks
/// the player entity and the `bots` array, and the multiplayer one
/// (`main.js:3033`) walks `remotePlayers` — the boss's fake records
/// (`main.js:2945`) are in neither list a bot can see, and a bot shooting its
/// own capital ship would be nonsense anyway.
fn is_opponent(bot: &Ship, other: &Ship) -> bool {
    if other.id == bot.id || !other.alive || other.kind == ShipKind::BossHitbox {
        return false;
    }
    match (bot.team, other.team) {
        // `bot.js:125` compares raw team values, so an unassigned team on
        // either side reads as hostile. `server/index.js:941` makes the same
        // choice for friendly fire.
        (Some(a), Some(b)) => a != b,
        _ => true,
    }
}

/// Index of `id` in [`World::ships`]. Only ever called with an id
/// [`pick_target`] just returned, so the fallback is unreachable in practice.
fn target_index(world: &World, id: EntityId) -> usize {
    world
        .ships
        .iter()
        .position(|s| s.id == id)
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Avoidance and push-out
// ---------------------------------------------------------------------------

/// A unit direction away from anything the bot is about to fly into, or `None`
/// if the path ahead is clear. `bot.js:80` (`computeAvoidance`).
///
/// Walks [`World::asteroids`] then [`World::obstacles`], in that order, exactly
/// as the JS does. Motherships and airfields ([`World::boxes`]) are *not*
/// considered: `main.js:2494` hands the bot only the moon, so bots have never
/// avoided the spawn platforms and fly straight into the push-out volume.
/// Preserved rather than fixed — adding them changes every bot's path near
/// spawn, which is a balance decision.
fn compute_avoidance(world: &World, origin: Vec3, dir: Vec3) -> Option<Vec3> {
    let br = &world.rules.bot;
    let ship_r = world.rules.ship.collide_radius;
    let mut push = Vec3::ZERO;
    let mut any = false;

    // Scoped so the closure's borrow of `push` and `any` ends before they are
    // read back.
    {
        let mut consider = |center: Vec3, radius: f64| {
            let to_center = center - origin;
            let t = to_center.dot(dir);
            let look_ahead = br.avoid_lookahead.max(radius * 2.5);
            if t < 0.0 || t > look_ahead {
                return;
            }
            let closest = origin.add_scaled(dir, t);
            let off = closest - center;
            let d2 = off.length_squared();
            let threshold = radius + ship_r + br.avoid_margin;
            if d2 > threshold * threshold {
                return;
            }
            let urgency = 1.0 - t / look_ahead;
            let d = d2.sqrt();
            if d < 1e-3 {
                // Dead centre: there is no side to push toward, so pick one
                // perpendicular to the heading in the xz plane. `bot.js:102`.
                push.x += -dir.z * urgency;
                push.z += dir.x * urgency;
            } else {
                push += off * (urgency / d);
            }
            any = true;
        };

        for a in &world.asteroids {
            consider(a.pos, a.radius);
        }
        for o in &world.obstacles {
            consider(o.pos, o.radius);
        }
    }

    if any {
        Some(push.normalize())
    } else {
        None
    }
}

/// Pushes the bot out of a sphere it ended the tick inside, and kicks its
/// velocity back out. `bot.js:214`/`:237`.
///
/// Returns whether a collision happened. The restitution is
/// [`crate::rules::CombatRules::collision_restitution`] — greater than one, so
/// a bounce adds energy, which is what makes rocks feel like they kick.
fn push_out(pos: &mut Vec3, vel: &mut Vec3, center: Vec3, radius: f64, rules: &Rules) -> bool {
    let off = *pos - center;
    let d2 = off.length_squared();
    let min_dist = rules.ship.collide_radius + radius;
    if d2 >= min_dist * min_dist || d2 <= 0.0001 {
        return false;
    }
    let d = d2.sqrt();
    let n = off / d;
    *pos = pos.add_scaled(n, min_dist - d);
    let v_dot_n = vel.dot(n);
    if v_dot_n < 0.0 {
        *vel -= n * (rules.combat.collision_restitution * v_dot_n);
    }
    true
}

/// A random unit direction to break off along. `bot.js:57` (`chooseEvadeDir`).
///
/// Three signed uniforms then a normalize, which samples a cube rather than a
/// sphere and is therefore slightly biased toward the corners. Preserved: it is
/// an evasion jink, the bias is invisible, and changing the number of draws
/// would shift every subsequent value on the bot stream.
fn choose_evade_dir(rng: &mut Rng) -> Vec3 {
    Vec3::new(
        rng.next_f64_signed(),
        rng.next_f64_signed(),
        rng.next_f64_signed(),
    )
    .normalize()
}

// ---------------------------------------------------------------------------
// Orientation
// ---------------------------------------------------------------------------

// The nose direction, the quaternion product, the renormalize and the
// axis-angle constructor all live in `math` now — see the note on the `dmath`
// alias above. `axis_angle` keeps its local name because that is what
// `bot.js:70`/`:75` call the operation.
pub use crate::math::forward;

/// `cos(1e-3)`: the JS `angle < 1e-3` early-out (`bot.js:66`) rewritten as a
/// cosine comparison so no inverse trigonometry is needed. A literal rather
/// than an evaluation, so it is the same bits everywhere.
const COS_MIN_TURN: f64 = 0.999_999_5;

/// Turns `q` from `from` toward `to` by at most `max_angle` radians.
/// `bot.js:64` (`rotateToward`).
///
/// # Why this avoids `acos`, and usually `sin`/`cos` too
///
/// The JS computes `angle = from.angleTo(to)` (an `acos`), clamps it, and calls
/// `setFromAxisAngle` (a `sin` and a `cos`). Three transcendentals, none of
/// them bit-identical across platforms. Here:
///
/// - "Is the required turn bigger than this tick's budget?" is
///   `dot(from, to) < cos(max_angle)`, because cosine decreases monotonically
///   on `[0, π]`. That is one cosine of a *small, known* argument, from
///   [`dmath`].
/// - When the turn fits inside the budget, the rotation taking `from` to `to`
///   is `normalize((from × to, 1 + from · to))` — the half-angle identity, with
///   no trigonometry at all. It is exactly equal to `setFromAxisAngle`: the
///   vector part works out to `n · sin(θ/2)` and the scalar to `cos(θ/2)`.
/// - Only the clamped case needs a `sin` and a `cos`, of `max_angle / 2`.
///
/// The degenerate near-180° case keeps the JS's arbitrary fallback axis
/// (`bot.js:70`): with `from` and `to` antiparallel there is no preferred
/// rotation plane, and any axis breaks the deadlock.
fn rotate_toward(q: Quat, from: Vec3, to: Vec3, max_angle: f64) -> Quat {
    if max_angle <= 0.0 {
        return q;
    }
    let dot = from.dot(to).clamp(-1.0, 1.0);
    if dot >= COS_MIN_TURN {
        return q;
    }
    let cross = from.cross(to);
    let degenerate = cross.length_squared() < 1e-6;

    let step = if degenerate {
        // `from` and `to` are antiparallel — a parallel pair was caught above,
        // and a zero-length `to` lands here too, which is strictly better than
        // the JS's `NaN`.
        let axis = if from.y.abs() > 0.9 { Vec3::X } else { Vec3::Y };
        axis_angle(axis, max_angle.min(PI))
    } else if max_angle < PI && dot < dmath::cos(max_angle) {
        axis_angle(cross.normalize(), max_angle)
    } else {
        // The turn fits in this tick's budget: rotate the whole way, exactly,
        // without touching a transcendental.
        quat_normalize(Quat::new(cross.x, cross.y, cross.z, 1.0 + dot))
    };

    quat_normalize(quat_mul(step, q))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{Asteroid, AsteroidTier, Flare, MapKind, Mode};

    const SEED: u64 = 0x5EED_5EED;
    const DT: f64 = 1.0 / 60.0;

    /// A world with no static geometry near the origin.
    ///
    /// Deliberately the terrain map: [`MapKind::Space`] puts an 80-unit moon at
    /// the origin (`World::new`), and a fixture that spawned a bot there would
    /// be testing collision response rather than AI. The two airfield boxes the
    /// terrain map does add are at `z = ±1500` and are invisible to bots
    /// anyway — see [`compute_avoidance`].
    fn world() -> World {
        World::new(SEED, Rules::DEFAULT, Mode::Skirmish, MapKind::Terrain)
    }

    fn v(x: f64, y: f64, z: f64) -> Vec3 {
        Vec3::new(x, y, z)
    }

    fn add(w: &mut World, id: EntityId, kind: ShipKind, team: Team, pos: Vec3) -> usize {
        let mut s = Ship::spawn(id, kind, pos, Quat::IDENTITY, &w.rules);
        s.team = Some(team);
        s.invuln_timer = 0.0;
        w.ships.push(s);
        w.ships.len() - 1
    }

    /// A bot at the origin facing `+z`, with an enemy dead ahead at `range`.
    fn duel(range: f64) -> World {
        let mut w = world();
        add(&mut w, 1, ShipKind::Bot, Team::Zero, Vec3::ZERO);
        add(&mut w, 2, ShipKind::Local, Team::One, v(0.0, 0.0, range));
        let rules = w.rules;
        let mut rng = Rng::new(1);
        init(&mut w.ships[0], false, false, &rules, &mut rng);
        w.ships[0].bot.fsm = BotFsm::Attack;
        w
    }

    fn tick(w: &mut World, n: usize) -> Vec<SimEvent> {
        let mut ev = Vec::new();
        for _ in 0..n {
            update_bots(w, DT, None, &mut ev);
        }
        ev
    }

    fn rock(w: &mut World, pos: Vec3, radius: f64) {
        let id = w.next_asteroid_id;
        w.next_asteroid_id += 1;
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

    // --- deterministic transcendentals -----------------------------------

    #[test]
    fn dmath_sin_cos_match_libm_over_the_whole_domain() {
        for i in 0..=2000 {
            let x = PI * f64::from(i) / 2000.0;
            assert!((dmath::sin(x) - x.sin()).abs() < 1e-14, "sin({x}) drifted");
            assert!((dmath::cos(x) - x.cos()).abs() < 1e-14, "cos({x}) drifted");
        }
    }

    #[test]
    fn dmath_exp_neg_matches_libm() {
        for i in 0..=1000 {
            let x = f64::from(i) / 100.0;
            let want = (-x).exp();
            assert!(
                (dmath::exp_neg(x) - want).abs() <= want * 1e-13,
                "exp(-{x}) drifted"
            );
        }
        assert_eq!(dmath::exp_neg(0.0), 1.0);
        assert_eq!(dmath::exp_neg(-1.0), 1.0);
        assert_eq!(dmath::exp_neg(1000.0), 0.0);
        assert_eq!(dmath::exp_neg(f64::NAN), 1.0);
    }

    #[test]
    fn dmath_is_bit_reproducible() {
        let run = || (dmath::sin(0.31), dmath::cos(1.7), dmath::exp_neg(0.05));
        assert_eq!(run(), run());
    }

    // --- orientation ------------------------------------------------------

    #[test]
    fn forward_of_identity_is_plus_z() {
        assert_eq!(forward(Quat::IDENTITY), Vec3::Z);
        // FLIP_Y is the team-1 spawn orientation: nose down -z.
        assert!(forward(Quat::FLIP_Y).abs_diff_eq(-Vec3::Z, 1e-12));
    }

    #[test]
    fn rotate_toward_respects_the_angle_budget() {
        // A 90-degree turn requested with a 0.1 rad budget advances 0.1 rad.
        let q = rotate_toward(Quat::IDENTITY, Vec3::Z, Vec3::X, 0.1);
        let cos_step = forward(q).dot(Vec3::Z);
        assert!(
            (cos_step - 0.1_f64.cos()).abs() < 1e-12,
            "turned {cos_step}"
        );
        assert!(forward(q).is_normalized(1e-12));
    }

    #[test]
    fn rotate_toward_snaps_when_the_turn_fits() {
        let q = rotate_toward(Quat::IDENTITY, Vec3::Z, Vec3::X, 2.0);
        assert!(forward(q).abs_diff_eq(Vec3::X, 1e-12));
    }

    #[test]
    fn rotate_toward_is_a_no_op_when_already_aligned() {
        assert_eq!(
            rotate_toward(Quat::IDENTITY, Vec3::Z, Vec3::Z, 0.1),
            Quat::IDENTITY
        );
        assert_eq!(
            rotate_toward(Quat::IDENTITY, Vec3::Z, Vec3::X, 0.0),
            Quat::IDENTITY
        );
    }

    #[test]
    fn rotate_toward_survives_antiparallel_and_degenerate_input() {
        // A 180-degree reversal has no defined rotation plane; it must still
        // produce a finite unit quaternion and make progress.
        let q = rotate_toward(Quat::IDENTITY, Vec3::Z, -Vec3::Z, 0.1);
        assert!(q.is_finite());
        assert!(forward(q).is_normalized(1e-12));
        assert!(forward(q).dot(Vec3::Z) < 1.0);
        // A zero target direction is `NaN` in the JS; here it is a jink.
        assert!(rotate_toward(Quat::IDENTITY, Vec3::Z, Vec3::ZERO, 0.1).is_finite());
    }

    #[test]
    fn rotate_toward_converges_on_the_target() {
        let mut q = Quat::IDENTITY;
        let goal = v(1.0, 1.0, -1.0).normalize();
        for _ in 0..400 {
            q = rotate_toward(q, forward(q), goal, 0.02);
        }
        assert!(forward(q).abs_diff_eq(goal, 1e-9));
    }

    // --- intercept --------------------------------------------------------

    #[test]
    fn solve_intercept_handles_a_stationary_target() {
        // 780 units away at 780 u/s: one second.
        let t = solve_intercept(
            v(0.0, 0.0, 780.0),
            Vec3::ZERO,
            Vec3::ZERO,
            Vec3::ZERO,
            780.0,
        )
        .unwrap();
        assert!((t - 1.0).abs() < 1e-12);
    }

    #[test]
    fn solve_intercept_leads_a_crossing_target() {
        let target_pos = v(0.0, 0.0, 100.0);
        let target_vel = v(50.0, 0.0, 0.0);
        let t = solve_intercept(target_pos, target_vel, Vec3::ZERO, Vec3::ZERO, 780.0).unwrap();
        let lead = target_pos.add_scaled(target_vel, t);
        // The aim point sits ahead of the target along its travel...
        assert!(lead.x > 0.0);
        // ...and is exactly reachable in `t` at bullet speed.
        assert!((lead.length() - 780.0 * t).abs() < 1e-9);
    }

    #[test]
    fn solve_intercept_gives_up_on_an_unreachable_target() {
        // Running away faster than the bullet flies.
        assert!(solve_intercept(
            v(0.0, 0.0, 100.0),
            v(0.0, 0.0, 900.0),
            Vec3::ZERO,
            Vec3::ZERO,
            780.0,
        )
        .is_none());
    }

    // --- target selection -------------------------------------------------

    #[test]
    fn pick_target_takes_the_nearest_living_enemy() {
        let mut w = world();
        add(&mut w, 1, ShipKind::Bot, Team::Zero, Vec3::ZERO);
        add(&mut w, 2, ShipKind::Local, Team::One, v(0.0, 0.0, 500.0));
        add(&mut w, 3, ShipKind::Bot, Team::One, v(0.0, 0.0, 100.0));
        let bot = w.ships[0].clone();
        assert_eq!(pick_target(&w, &bot), Some(3));
    }

    #[test]
    fn pick_target_ignores_team_mates_dead_ships_and_boss_hitboxes() {
        let mut w = world();
        add(&mut w, 1, ShipKind::Bot, Team::Zero, Vec3::ZERO);
        add(&mut w, 2, ShipKind::Bot, Team::Zero, v(0.0, 0.0, 10.0));
        let dead = add(&mut w, 3, ShipKind::Local, Team::One, v(0.0, 0.0, 20.0));
        w.ships[dead].alive = false;
        add(
            &mut w,
            9000,
            ShipKind::BossHitbox,
            Team::One,
            v(0.0, 0.0, 30.0),
        );
        add(&mut w, 4, ShipKind::Local, Team::One, v(0.0, 0.0, 900.0));
        let bot = w.ships[0].clone();
        assert_eq!(pick_target(&w, &bot), Some(4));
    }

    #[test]
    fn pick_target_breaks_ties_by_insertion_order() {
        let mut w = world();
        add(&mut w, 1, ShipKind::Bot, Team::Zero, Vec3::ZERO);
        add(&mut w, 7, ShipKind::Local, Team::One, v(0.0, 0.0, 100.0));
        add(&mut w, 8, ShipKind::Local, Team::One, v(0.0, 0.0, -100.0));
        let bot = w.ships[0].clone();
        // Equidistant; the earlier entry wins, every run.
        for _ in 0..8 {
            assert_eq!(pick_target(&w, &bot), Some(7));
        }
    }

    #[test]
    fn a_bot_with_no_enemy_cruises_instead_of_stopping() {
        let mut w = world();
        add(&mut w, 1, ShipKind::Bot, Team::Zero, Vec3::ZERO);
        let rules = w.rules;
        let mut rng = Rng::new(1);
        init(&mut w.ships[0], false, false, &rules, &mut rng);
        tick(&mut w, 60);
        assert!(w.ships[0].pos.z > 0.0, "should have drifted forward");
        assert!(w.bullets.is_empty());
    }

    // --- defect 1: one bullet path ---------------------------------------

    #[test]
    fn firing_produces_exactly_one_world_bullet_and_no_shadow_projectile() {
        let mut w = duel(200.0);
        let events = tick(&mut w, 30);
        assert!(!w.bullets.is_empty(), "the bot never fired");
        let fired = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    SimEvent::Fired {
                        weapon: WeaponKind::Bullet,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            fired,
            w.bullets.len(),
            "one Fired event per bullet, and no second simulation"
        );
    }

    #[test]
    fn bot_bullets_carry_the_shared_weapon_rules() {
        let mut w = duel(200.0);
        tick(&mut w, 30);
        let b = *w.bullets.first().expect("no bullet");
        let r = w.rules.weapons;
        assert_eq!(b.owner, 1);
        assert_eq!(b.owner_team, Some(Team::Zero));
        assert_eq!(b.damage, r.gun_damage, "bot.js:20 DAMAGE = 10");
        assert!((b.vel.length() - r.bullet_speed).abs() < 1e-9);
        assert_eq!(b.life, r.bullet_life);
        assert_eq!(b.prev_pos, b.pos, "the sweep starts at the muzzle");
        assert!(b.key > 0, "a renderer key must be allocated");
    }

    // --- defect 2: the shared hit radius ---------------------------------

    #[test]
    fn bot_bullets_resolve_against_the_unified_ship_radius() {
        let mut w = duel(200.0);
        tick(&mut w, 30);
        let b = *w.bullets.first().expect("no bullet");
        // The bullet carries no radius of its own: the target's radius plus the
        // bullet's is what a hit test uses, and it is the same 6.0 the player's
        // shots get. The bot-only 4.0 is gone.
        let target = w.ship(2).unwrap();
        assert!(!b.owner_coarse_aim);
        assert_eq!(target.hit_radius(&w.rules, b.owner_coarse_aim), 6.0);
        assert_eq!(w.rules.weapons.bullet_radius, 0.5);
    }

    // --- defect 3: spawn invulnerability ---------------------------------

    #[test]
    fn a_bot_holds_fire_against_an_invulnerable_target() {
        let mut w = duel(200.0);
        w.ships[1].invuln_timer = w.rules.combat.spawn_invuln;
        tick(&mut w, 60);
        assert!(
            w.bullets.is_empty(),
            "an invulnerable target is not shootable, so do not shoot it"
        );
        // ...and the bot opens up the moment protection lapses.
        w.ships[1].invuln_timer = 0.0;
        tick(&mut w, 30);
        assert!(!w.bullets.is_empty());
    }

    #[test]
    fn a_bot_still_pursues_an_invulnerable_target() {
        let mut w = duel(400.0);
        w.ships[1].invuln_timer = 10.0;
        let before = w.ships[0].pos.distance(w.ships[1].pos);
        tick(&mut w, 60);
        let after = w.ships[0].pos.distance(w.ships[1].pos);
        assert!(after < before, "the bot should still be closing");
    }

    // --- FSM --------------------------------------------------------------

    #[test]
    fn seek_becomes_attack_inside_seek_dist() {
        let mut w = duel(200.0);
        w.ships[0].bot.fsm = BotFsm::Seek;
        tick(&mut w, 1);
        assert_eq!(w.ships[0].bot.fsm, BotFsm::Attack);
    }

    #[test]
    fn attack_becomes_evade_when_the_target_is_too_close() {
        let mut w = duel(20.0);
        tick(&mut w, 1);
        assert_eq!(w.ships[0].bot.fsm, BotFsm::Evade);
        assert!(w.ships[0].bot.evade_axis.is_normalized(1e-9));
    }

    #[test]
    fn evade_lapses_back_to_seek() {
        let mut w = duel(20.0);
        tick(&mut w, 1);
        assert_eq!(w.ships[0].bot.fsm, BotFsm::Evade);
        // Far enough away that the too-close rule cannot retrigger.
        w.ships[1].pos = v(0.0, 0.0, 900.0);
        tick(&mut w, 60);
        assert_eq!(w.ships[0].bot.fsm, BotFsm::Seek);
    }

    #[test]
    fn attack_falls_back_to_seek_past_the_hysteresis_band() {
        let mut w = duel(200.0);
        let exit = w.rules.bot.seek_dist * w.rules.bot.seek_exit_multiplier;
        w.ships[1].pos = v(0.0, 0.0, exit + 50.0);
        tick(&mut w, 1);
        assert_eq!(w.ships[0].bot.fsm, BotFsm::Seek);
    }

    #[test]
    fn a_dead_bot_freezes_its_timers_and_does_nothing() {
        let mut w = duel(200.0);
        w.ships[0].alive = false;
        let before = w.ships[0].bot;
        let pos = w.ships[0].pos;
        tick(&mut w, 60);
        assert_eq!(w.ships[0].bot, before);
        assert_eq!(w.ships[0].pos, pos);
        assert!(w.bullets.is_empty());
    }

    // --- difficulty -------------------------------------------------------

    #[test]
    fn hard_mode_shortens_the_gun_cooldown_and_adds_missiles() {
        let rules = Rules::DEFAULT;
        let mut rng = Rng::new(3);
        let mut easy = Ship::spawn(1, ShipKind::Bot, Vec3::ZERO, Quat::IDENTITY, &rules);
        let mut hard = Ship::spawn(2, ShipKind::Bot, Vec3::ZERO, Quat::IDENTITY, &rules);
        init(&mut easy, false, false, &rules, &mut rng);
        init(&mut hard, true, false, &rules, &mut rng);
        assert_eq!(easy.bot.missiles_left, 1);
        assert_eq!(hard.bot.missiles_left, 3);
        assert_eq!(rules.bot.fire_cooldown_for(false), 0.15);
        assert_eq!(rules.bot.fire_cooldown_for(true), 0.05);
    }

    #[test]
    fn hard_bots_fire_more_often_than_easy_ones() {
        let count = |hard: bool| {
            let mut w = duel(200.0);
            let rules = w.rules;
            let mut rng = Rng::new(9);
            init(&mut w.ships[0], hard, false, &rules, &mut rng);
            w.ships[0].bot.fsm = BotFsm::Attack;
            // No missiles, so the count is purely gun cadence.
            w.ships[0].bot.missiles_left = 0;
            tick(&mut w, 240);
            w.bullets.len()
        };
        assert!(count(true) > count(false));
    }

    #[test]
    fn the_first_missile_is_delayed_by_a_random_interval() {
        let rules = Rules::DEFAULT;
        let mut rng = Rng::new(11);
        for _ in 0..64 {
            let mut s = Ship::spawn(1, ShipKind::Bot, Vec3::ZERO, Quat::IDENTITY, &rules);
            init(&mut s, true, false, &rules, &mut rng);
            let d = s.bot.missile_timer;
            assert!(
                d >= rules.bot.missile_delay_min
                    && d < rules.bot.missile_delay_min + rules.bot.missile_delay_range,
                "delay {d} out of range"
            );
        }
    }

    // --- missiles ---------------------------------------------------------

    #[test]
    fn a_bot_launches_a_locked_missile_inside_the_range_band() {
        let mut w = duel(300.0);
        w.ships[0].bot.missile_timer = 0.0;
        let events = tick(&mut w, 30);
        let m = *w.missiles.first().expect("no missile launched");
        assert_eq!(m.target, Some(MissileTarget::Ship(2)));
        assert_eq!(m.owner, 1);
        assert_eq!(m.owner_team, Some(Team::Zero));
        assert_eq!(m.life, w.rules.weapons.missile_life);
        assert!(m.dir.is_normalized(1e-9));
        assert_eq!(w.ships[0].bot.missiles_left, 0);
        assert!(events.iter().any(|e| matches!(
            e,
            SimEvent::Fired {
                weapon: WeaponKind::Missile,
                ..
            }
        )));
    }

    #[test]
    fn a_bot_does_not_launch_outside_the_range_band() {
        // Inside missile_min_range.
        let mut near = duel(100.0);
        near.ships[0].bot.missile_timer = 0.0;
        tick(&mut near, 30);
        assert!(near.missiles.is_empty());

        // Beyond missile_max_range — and beyond seek_dist, so it never even
        // reaches the attack state.
        let mut far = duel(900.0);
        far.ships[0].bot.missile_timer = 0.0;
        tick(&mut far, 30);
        assert!(far.missiles.is_empty());
    }

    #[test]
    fn a_bot_never_launches_more_missiles_than_it_carries() {
        let mut w = duel(300.0);
        let rules = w.rules;
        let mut rng = Rng::new(5);
        init(&mut w.ships[0], true, false, &rules, &mut rng);
        for _ in 0..40 {
            // Reset the duel each round so the gates stay open.
            w.ships[0].pos = Vec3::ZERO;
            w.ships[0].bot.fsm = BotFsm::Attack;
            w.ships[0].bot.missile_timer = 0.0;
            w.ships[1].pos = v(0.0, 0.0, 300.0);
            tick(&mut w, 30);
        }
        assert_eq!(w.missiles.len(), 3);
        assert_eq!(w.ships[0].bot.missiles_left, 0);
    }

    // --- flares -----------------------------------------------------------

    fn incoming_missile(w: &mut World, at: Vec3, target: EntityId) {
        let key = w.take_projectile_key();
        w.missiles.push(Missile {
            key,
            pos: at,
            dir: Vec3::Z,
            target: Some(MissileTarget::Ship(target)),
            life: 8.0,
            age: 0.0,
            owner: 2,
            owner_team: Some(Team::One),
        });
    }

    #[test]
    fn a_bot_flares_a_missile_that_has_closed_to_seduction_range() {
        let mut w = duel(300.0);
        incoming_missile(&mut w, v(0.0, 0.0, 100.0), 1);
        let before = w.ships[0].flares_left;
        let events = tick(&mut w, 1);
        assert_eq!(w.ships[0].flares_left, before - 1);
        assert!(events
            .iter()
            .any(|e| matches!(e, SimEvent::FlareBurst { owner: 1, .. })));
    }

    #[test]
    fn a_bot_ignores_a_missile_that_is_far_away_or_not_aimed_at_it() {
        let mut w = duel(300.0);
        // Locked on, but well outside seduction range.
        incoming_missile(&mut w, v(0.0, 0.0, 900.0), 1);
        // Close, but chasing someone else.
        incoming_missile(&mut w, v(0.0, 0.0, 20.0), 2);
        let before = w.ships[0].flares_left;
        tick(&mut w, 1);
        assert_eq!(w.ships[0].flares_left, before);
    }

    #[test]
    fn a_bot_does_not_stack_bursts_while_a_decoy_is_already_burning() {
        let mut w = duel(300.0);
        incoming_missile(&mut w, v(0.0, 0.0, 100.0), 1);
        tick(&mut w, 1);
        let after_first = w.ships[0].flares_left;
        w.flares.push(Flare {
            key: 999,
            pos: w.ships[0].pos,
            vel: Vec3::ZERO,
            life: 1.8,
            age: 0.0,
            owner: 1,
        });
        tick(&mut w, 10);
        assert_eq!(w.ships[0].flares_left, after_first);
    }

    #[test]
    fn a_bot_out_of_flares_does_not_underflow() {
        let mut w = duel(300.0);
        w.ships[0].flares_left = 0;
        incoming_missile(&mut w, v(0.0, 0.0, 100.0), 1);
        tick(&mut w, 10);
        assert_eq!(w.ships[0].flares_left, 0);
    }

    // --- avoidance --------------------------------------------------------

    #[test]
    fn avoidance_pushes_away_from_a_rock_dead_ahead() {
        let mut w = world();
        rock(&mut w, v(0.0, 0.0, 40.0), 10.0);
        let avoid = compute_avoidance(&w, Vec3::ZERO, Vec3::Z).expect("no avoidance");
        assert!(avoid.is_normalized(1e-9));
        // Dead on the line, so the push is the lateral fallback, and it must
        // not be along the heading.
        assert!(avoid.z.abs() < 1e-9);
    }

    #[test]
    fn avoidance_ignores_what_is_behind_or_out_of_reach() {
        let mut behind = world();
        rock(&mut behind, v(0.0, 0.0, -40.0), 10.0);
        assert!(compute_avoidance(&behind, Vec3::ZERO, Vec3::Z).is_none());

        let mut distant = world();
        rock(&mut distant, v(0.0, 0.0, 5000.0), 10.0);
        assert!(compute_avoidance(&distant, Vec3::ZERO, Vec3::Z).is_none());

        let mut wide = world();
        // Beside the flight path by more than radius + ship + margin.
        rock(&mut wide, v(200.0, 0.0, 40.0), 10.0);
        assert!(compute_avoidance(&wide, Vec3::ZERO, Vec3::Z).is_none());
    }

    #[test]
    fn avoidance_sees_the_moon_as_well_as_the_rocks() {
        // The space map's only obstacle, and the second list `compute_avoidance`
        // walks (`bot.js:114`).
        let mut w = World::new(SEED, Rules::DEFAULT, Mode::Skirmish, MapKind::Space);
        assert_eq!(w.obstacles.len(), 1);
        let moon = w.obstacles[0];
        // Inside the 200-unit lookahead (`max(avoid_lookahead, radius * 2.5)`)
        // and passing close enough overhead to trip the clearance threshold.
        let origin = v(0.0, 60.0, -150.0);
        let avoid = compute_avoidance(&w, origin, Vec3::Z).expect("the moon was not avoided");
        assert!(avoid.is_normalized(1e-9));
        assert!(avoid.y > 0.0, "should climb over the moon, got {avoid:?}");
        // Nothing to avoid once it is astern.
        assert!(compute_avoidance(&w, origin, -Vec3::Z).is_none());
        w.obstacles.clear();
        assert!(compute_avoidance(&w, origin, Vec3::Z).is_none());
        assert_eq!(moon.radius, 80.0);
    }

    #[test]
    fn avoidance_steers_off_an_offset_obstacle() {
        let mut w = world();
        rock(&mut w, v(6.0, 0.0, 40.0), 10.0);
        let avoid = compute_avoidance(&w, Vec3::ZERO, Vec3::Z).expect("no avoidance");
        assert!(
            avoid.x < 0.0,
            "should push away from the rock, got {avoid:?}"
        );
    }

    #[test]
    fn a_bot_is_pushed_out_of_a_rock_it_ends_up_inside() {
        let mut w = duel(200.0);
        rock(&mut w, v(0.0, 0.0, 1.0), 12.0);
        tick(&mut w, 1);
        let d = w.ships[0].pos.distance(v(0.0, 0.0, 1.0));
        assert!(
            d >= 12.0 + w.rules.ship.collide_radius - 1e-9,
            "still inside the rock: {d}"
        );
        assert_eq!(w.ships[0].bot.fsm, BotFsm::Evade);
    }

    #[test]
    fn push_out_uses_the_unified_collide_radius_not_the_old_bot_value() {
        // rules.rs resolved 3.5 (bot.js:31) against 3.3 (main.js:980) in favour
        // of the player's derived value.
        let rules = Rules::DEFAULT;
        assert_eq!(rules.ship.collide_radius, 3.3);
        let mut pos = v(0.0, 0.0, 5.0);
        let mut vel = v(0.0, 0.0, -10.0);
        assert!(push_out(&mut pos, &mut vel, Vec3::ZERO, 10.0, &rules));
        assert!((pos.length() - 13.3).abs() < 1e-9);
        assert!(vel.z > 0.0, "the bounce should reverse the approach");
    }

    // --- stuck ------------------------------------------------------------

    #[test]
    fn a_wedged_bot_eventually_forces_an_evade() {
        let mut w = duel(200.0);
        w.ships[0].bot.fsm = BotFsm::Seek;
        let mut ev = Vec::new();
        let mut evaded = false;
        for _ in 0..300 {
            // Wedge it: no forward progress, so speed stays under the
            // threshold every tick.
            w.ships[0].pos = Vec3::ZERO;
            w.ships[0].vel = Vec3::ZERO;
            update_bots(&mut w, DT, None, &mut ev);
            if w.ships[0].bot.fsm == BotFsm::Evade {
                evaded = true;
                break;
            }
        }
        assert!(evaded, "stuck detection never fired");
    }

    // --- terrain ----------------------------------------------------------

    struct FlatGround(f64);
    impl TerrainHeight for FlatGround {
        fn height_at(&self, _x: f64, _z: f64) -> f64 {
            self.0
        }
    }

    #[test]
    fn terrain_clearance_pulls_the_nose_up_and_clamps_the_floor() {
        let mut w = duel(400.0);
        w.ships[0].pos = v(0.0, 2.0, 0.0);
        w.ships[1].pos = v(0.0, 2.0, 400.0);
        let ground = FlatGround(0.0);
        let mut ev = Vec::new();
        for _ in 0..120 {
            update_bots(&mut w, DT, Some(&ground), &mut ev);
        }
        let floor = w.rules.bot.terrain_min_clearance;
        assert!(w.ships[0].pos.y >= floor - 1e-9, "sank through the floor");
        assert!(w.ships[0].pos.y > 5.0, "never climbed away from the ground");
    }

    // --- lifecycle --------------------------------------------------------

    #[test]
    fn notify_hit_breaks_the_bot_off() {
        let mut w = duel(200.0);
        let mut rng = Rng::new(2);
        notify_hit(&mut w.ships[0], &mut rng);
        assert_eq!(w.ships[0].bot.fsm, BotFsm::Evade);
        assert!(w.ships[0].bot.evade_axis.is_normalized(1e-9));
        // A dead bot is left alone.
        w.ships[0].alive = false;
        w.ships[0].bot.fsm = BotFsm::Seek;
        notify_hit(&mut w.ships[0], &mut rng);
        assert_eq!(w.ships[0].bot.fsm, BotFsm::Seek);
    }

    #[test]
    fn notify_respawn_clears_the_stale_lead_the_js_kept() {
        let mut w = duel(200.0);
        tick(&mut w, 10);
        assert!(w.ships[0].bot.tracked_lead_seeded);
        let rules = w.rules;
        let mut rng = Rng::new(4);
        notify_respawn(&mut w.ships[0], &rules, &mut rng);
        assert!(!w.ships[0].bot.tracked_lead_seeded);
        assert_eq!(w.ships[0].bot.fsm, BotFsm::Seek);
        assert_eq!(w.ships[0].bot.missiles_left, 1);
        assert_eq!(w.ships[0].bot.stuck_time, 0.0);
        assert_eq!(w.ships[0].vel, Vec3::ZERO);
    }

    // --- aim --------------------------------------------------------------

    #[test]
    fn the_aim_error_stays_inside_its_ball() {
        let mut w = duel(300.0);
        let max = w.rules.bot.aim_offset_max;
        for _ in 0..600 {
            tick(&mut w, 1);
            assert!(w.ships[0].bot.aim_offset.length() <= max + 1e-9);
        }
    }

    #[test]
    fn the_bot_leads_a_crossing_target_rather_than_aiming_at_it() {
        let mut w = duel(300.0);
        w.ships[1].vel = v(60.0, 0.0, 0.0);
        // One tick seeds the tracked lead straight from the intercept solve.
        tick(&mut w, 1);
        let lead = w.ships[0].bot.tracked_lead;
        assert!(
            lead.x > 10.0,
            "aim point should sit ahead of the target, got {lead:?}"
        );
    }

    // --- determinism ------------------------------------------------------

    fn brawl() -> World {
        let mut w = duel(260.0);
        rock(&mut w, v(10.0, 4.0, 60.0), 14.0);
        add(&mut w, 3, ShipKind::Bot, Team::One, v(80.0, 20.0, 120.0));
        let rules = w.rules;
        let mut rng = Rng::new(7);
        init(&mut w.ships[2], true, false, &rules, &mut rng);
        w
    }

    #[test]
    fn two_identical_worlds_stay_identical() {
        let mut a = brawl();
        let mut b = brawl();
        let ea = tick(&mut a, 600);
        let eb = tick(&mut b, 600);
        assert_eq!(a.ships, b.ships);
        assert_eq!(a.bullets, b.bullets);
        assert_eq!(a.missiles, b.missiles);
        assert_eq!(a.rng, b.rng);
        assert_eq!(ea.len(), eb.len());
        // Bit-exact, not merely close.
        for (x, y) in a.ships.iter().zip(&b.ships) {
            assert_eq!(
                x.pos.to_array().map(f64::to_bits),
                y.pos.to_array().map(f64::to_bits)
            );
        }
    }

    #[test]
    fn everything_stays_finite_under_a_long_run() {
        let mut w = brawl();
        rock(&mut w, v(0.0, 0.0, 120.0), 30.0);
        tick(&mut w, 3600);
        for s in &w.ships {
            assert!(s.pos.is_finite(), "{:?}", s.pos);
            assert!(s.vel.is_finite());
            assert!(s.quat.is_finite());
            let q = s.quat;
            let n = (q.x * q.x + q.y * q.y + q.z * q.z + q.w * q.w).sqrt();
            assert!((n - 1.0).abs() < 1e-9, "quaternion drifted to {n}");
        }
    }
}
