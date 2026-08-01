//! The simulation step: one function that runs the whole game for `dt` seconds.
//!
//! [`tick`] is the implementation of [`crate::world::TickFn`]. Every other
//! module in this crate is a *behaviour* — flight, ballistics, homing, AI,
//! rocks, mission script — written against the shared state in
//! [`crate::world`]. None of them knows about any of the others. This module is
//! where they meet, and the ordering below is the entire content of that
//! meeting.
//!
//! # Phase order, and why it is this one
//!
//! ```text
//!   0  inbound authority   apply every NetEvent, in the order given
//!   1  pre-race hold       a running trials countdown skips the whole step
//!   2  clocks              cooldowns, invulnerability, respawn, regeneration
//!   3  projectiles         bullets, then missiles + flares      <-- before movers
//!   4  movers              player flight, aim assist, remotes, bots, capital ship
//!   5  weapons             triggers, launches, countermeasures  <-- after movers
//!   6  field               asteroid spin and flash decay
//!   7  match clock         countdown and end-of-match
//!   8  outbound            time advances, then the periodic NetIntents
//! ```
//!
//! Four of those placements are load-bearing.
//!
//! **Projectiles run before anything moves (3 before 4).** Both
//! [`crate::bullets::resolve_impact`] and [`crate::missiles::update`] sweep a
//! target ship over `Ship::pos .. Ship::pos + vel * dt`. A [`Ship`] carries no
//! `prev_pos` — unlike [`crate::world::Bullet`], which does — so that expression
//! is the step's true segment *only while `pos` is still the start-of-step
//! pose*. If ships moved first, `pos` would be the end of the step and every
//! projectile would test against the interval *after* the one it is crossing.
//!
//! Adding `Ship::prev_pos` and sweeping `prev_pos .. pos` was considered, and
//! would be strictly more accurate (it would pick up the displacement collision
//! push-out contributes, which `vel * dt` misses). It is not done, for one
//! concrete reason: `bullets.rs` pins the `vel * step_dt` convention in
//! `a_crossing_target_is_hit_where_it_will_be_not_where_it_was`, which builds a
//! [`crate::bullets::Sweep`] by hand against a ship whose `vel` is set and whose
//! `prev_pos` would necessarily equal its `pos`. Changing the convention means
//! rewriting that test, and a passing test that pins a documented contract is
//! not something a downstream assembly step gets to overrule. The cost of
//! keeping it is bounded by the difference between a ship's straight-line step
//! and its post-collision step — a fraction of a unit — and the ordering is
//! pinned by `tests/tick_integration.rs`
//! (`projectiles_resolve_before_ships_move`) so nobody can quietly pay it
//! twice.
//!
//! **Weapons run after movers (5 after 4).** `main.js` fires at `:1461`, after
//! the flight block at `:1200`–`:1330`, so a shot leaves the muzzle at the pose
//! the player can see. A projectile spawned in phase 5 therefore begins its life
//! at the end-of-step pose and is first resolved on the *following* tick, which
//! is exactly the invariant [`crate::bullets::spawn_bullet`] documents when it
//! seeds `prev_pos == pos`.
//!
//! **Aim assist runs inside the movers, immediately after the local flight
//! model and before anything else moves.** `main.js:1256` calls
//! `applyAimAssist` from the middle of the flight block, well before the remote
//! interpolation at `:1721` and the bot update, so the assist sees every other
//! ship at its *start-of-step* pose and solves the intercept against that.
//! Running it here reproduces that exactly. Running it after the remote
//! interpolation or the bot step would silently give the player half a tick of
//! extra prediction the JS never had — pinned by `tests/tick_integration.rs`
//! (`aim_assist_solves_against_the_start_of_step_poses`).
//!
//! It cannot go inside [`crate::ship::integrate`], which is the one place the
//! JS puts it, because that function is the whole of `main.js:1200`–`:1330` —
//! including the velocity and position integration the JS does *after* the
//! assist. Splitting it in two to slot the assist into the middle would make
//! every existing flight test read as a two-phase call for the sake of one
//! caller. Instead `ship.rs` documents the composition rule ("it composes by
//! premultiplying `Ship::quat` after this returns") and this phase honours it.
//! The cost is that the step's velocity follows the *pre*-assist nose by one
//! tick, which at 60 Hz and a 2.6 rad/s ceiling is a 0.04 rad heading error on
//! the drift term and nothing at all on where the gun points, since the gun is
//! fired from the post-assist pose in phase 5.
//!
//! **A campaign death is settled once, between the movers and the mission
//! script.** [`crate::bullets`] and [`crate::missiles`] apply generic damage and
//! set the generic [`crate::rules::CombatRules::respawn_delay`];
//! [`crate::campaign::on_player_death`] spends a life, arms the warp-in, and
//! overwrites that timer with the shorter campaign one. Rather than teach three
//! modules about lives, [`tick`] samples whether the local player was alive at
//! the top of the step and calls `on_player_death` at most once, after every
//! damage source has run and before [`crate::campaign::update`] reads the
//! result. One death, one path.
//!
//! # Determinism
//!
//! Every list is a `Vec` walked in index order. The only randomness is
//! [`crate::world::WorldRng`], drawn from the stream each subsystem is assigned.
//! No wall clock: the periodic [`crate::world::NetIntent::State`] cadence is
//! derived from [`crate::world::World::time`] crossing a multiple of
//! [`crate::rules::MatchRules::state_send_interval`], not from a timer field, so
//! it survives a snapshot and cannot drift. Transcendentals come from
//! [`crate::math::det`].
//!
//! [`Ship`]: crate::world::Ship

use crate::aim_assist;
use crate::asteroids;
use crate::bot::{self, TerrainHeight};
use crate::bullets::{self, BulletOutput, BulletSpawn, HullPart, HullVolumes, ShipBasis};
use crate::campaign;
use crate::collision::Aabb;
use crate::math::{self, det, Vec3};
use crate::missiles::{self, Detonation, DetonationCause, Volume};
use crate::rules::Rules;
use crate::ship::{self, FlightStep, WorldGeometry};
use crate::world::{
    is_boss_hitbox, Authority, BossView, EntityId, Flare, FlareView, Frame, GunMode, HudState,
    Input, MapKind, Missile, Mode, NetEvent, NetIntent, ProjView, Quat, RockView, Ship, ShipFlags,
    ShipKind, ShipView, SimEvent, Team, TrialsHud, WeaponKind, World,
};

// ---------------------------------------------------------------------------
// The step
// ---------------------------------------------------------------------------

/// Advances `world` by `dt` and returns the frame the renderer draws.
///
/// This is the function [`crate::world::TickFn`] describes; see that type for
/// the contract on `dt`, `inputs`, and `events`, and the module docs above for
/// the phase order and why it is that one.
///
/// # Panics
///
/// Never, for any world reachable through this crate's own constructors.
pub fn tick(world: &mut World, inputs: &[Input], events: &[NetEvent], dt: f64) -> Frame {
    let mut out = Out::default();

    // -- 0. Inbound authority -------------------------------------------------
    for event in events {
        apply_net_event(world, *event, &mut out);
    }

    // -- 1. Pre-race hold -----------------------------------------------------
    // `main.js:1198` returns before the whole update block while a trials
    // countdown is running, which is control flow and not a HUD effect.
    if hold_for_countdown(world, dt) {
        world.time += dt;
        world.tick += 1;
        return build_frame(world, out, &[]);
    }

    // Sampled before any damage lands, so the campaign death settlement below
    // sees exactly one alive -> dead transition per step.
    let local_was_alive = world.local_ship().is_some_and(|s| s.alive);

    // -- 2. Clocks ------------------------------------------------------------
    advance_clocks(world, dt, &mut out);

    // -- 3. Projectiles, before anything moves --------------------------------
    let hull = boss_hull(world);
    step_bullets(world, dt, hull.as_ref(), &mut out);
    step_missiles(world, dt, hull.as_ref(), &mut out);

    // -- 4. Movers ------------------------------------------------------------
    let flights = integrate_players(world, inputs, dt, &mut out);
    steer_aim_assist(world, &flights, dt);
    interpolate_remotes(world, dt);
    step_bots(world, dt, &mut out);
    settle_campaign_death(world, local_was_alive, &mut out);
    campaign::update(world, dt, &mut out.events);

    // -- 5. Weapons -----------------------------------------------------------
    fire_weapons(world, inputs, dt, hull.as_ref(), &mut out);

    // -- 6. Field -------------------------------------------------------------
    asteroids::step(world, dt);

    // -- 7. Match clock -------------------------------------------------------
    advance_match_clock(world, dt, &mut out);

    // -- 8. Outbound ----------------------------------------------------------
    world.time += dt;
    world.tick += 1;
    emit_state_intents(world, dt, &flights, &mut out);

    build_frame(world, out, &flights)
}

/// [`tick`] is the [`crate::world::TickFn`]. If the signature ever drifts, this
/// fails to compile here rather than at a call site in another crate.
const _: crate::world::TickFn = tick;

/// The two output streams a step accumulates, threaded through every phase so
/// they end up in phase order.
#[derive(Debug, Default)]
struct Out {
    events: Vec<SimEvent>,
    net: Vec<NetIntent>,
}

// ---------------------------------------------------------------------------
// Phase 0 — inbound authority
// ---------------------------------------------------------------------------

/// Applies one authoritative message from the server.
///
/// Ports the WebSocket handler at `main.js:827`–`:975`. Order within a batch is
/// significant and is the caller's: an `Hp` after a `Death` means something
/// different from the reverse, which is why these are replayed one at a time in
/// the order handed in rather than being sorted or coalesced.
fn apply_net_event(world: &mut World, event: NetEvent, out: &mut Out) {
    match event {
        NetEvent::RemoteState {
            id,
            pos,
            quat,
            boost,
        } => ingest_remote_state(world, id, pos, quat, boost),
        NetEvent::Hp { id, hp } => {
            if let Some(s) = world.ship_mut(id) {
                if hp < s.hp {
                    s.hit_flash = 1.0;
                    s.health_idle_damage = 0.0;
                    s.health_regen_tick = 0.0;
                }
                s.hp = hp;
            }
        }
        NetEvent::Death { id, killer } => {
            let rules = world.rules;
            let mode = world.mode;
            let Some(s) = world.ship_mut(id) else { return };
            if !s.alive {
                return;
            }
            let pos = s.pos;
            ship::kill(s, &rules, mode);
            out.events.push(SimEvent::ShipDestroyed { id, killer, pos });
            credit_kill(world, id, killer);
        }
        NetEvent::Respawn { id, pos, quat } => {
            let rules = world.rules;
            let Some(index) = world.ships.iter().position(|s| s.id == id) else {
                return;
            };
            ship::respawn(&mut world.ships[index], pos, quat, &rules);
            if world.ships[index].kind == ShipKind::Bot {
                bot::notify_respawn(&mut world.ships[index], &rules, &mut world.rng.bots);
            }
            out.events.push(SimEvent::ShipRespawned { id, pos });
        }
        NetEvent::Fired {
            id,
            weapon,
            origin,
            dir,
            target,
        } => ingest_remote_shot(world, id, weapon, origin, dir, target, out),
        NetEvent::FlareBurst { id, .. } => {
            missiles::deploy_flares(world, id, &mut out.events);
        }
        NetEvent::PlayerRow {
            id,
            team,
            kills,
            deaths,
        } => {
            if let Some(s) = world.ship_mut(id) {
                s.team = team;
            }
            match world.match_state.scores.iter_mut().find(|r| r.id == id) {
                Some(row) => {
                    row.team = team;
                    row.kills = kills;
                    row.deaths = deaths;
                }
                None => world.match_state.scores.push(crate::world::Score {
                    id,
                    team,
                    kills,
                    deaths,
                }),
            }
        }
        NetEvent::MatchState { timer, team_kills } => {
            world.match_state.timer = timer;
            world.match_state.team_kills = team_kills;
        }
        NetEvent::MatchEnd {
            winner,
            team_kills, // authoritative final score
        } => {
            world.match_state.team_kills = team_kills;
            world.match_state.over = true;
            out.events.push(SimEvent::MatchEnded { winner });
        }
        NetEvent::AsteroidHp { id, hp } => {
            asteroids::set_hp(world, id, hp);
        }
        NetEvent::AsteroidDestroyed { id } => {
            asteroids::destroy(world, id, &mut out.events);
        }
        NetEvent::Disconnect { id } => {
            world.ships.retain(|s| s.id != id);
        }
    }
}

/// Ingests a remote pose and re-estimates that ship's velocity.
///
/// `main.js:827`–`:853`. The JS timestamps arrivals with `performance.now()`,
/// which is a wall clock and therefore banned here; [`World::time`] takes its
/// place, which is what [`crate::world::RemoteInterp::last_state_time`] is for.
fn ingest_remote_state(world: &mut World, id: EntityId, pos: Vec3, quat: Quat, boost: bool) {
    let rules = world.rules;
    let now = world.time;
    let index = match world.ships.iter().position(|s| s.id == id) {
        Some(i) => i,
        None => {
            // `getOrCreateRemote` (`main.js:687`): a pose for an unknown id
            // creates the ship rather than being dropped.
            world
                .ships
                .push(Ship::spawn(id, ShipKind::Remote, pos, quat, &rules));
            world.ships.len() - 1
        }
    };
    let s = &mut world.ships[index];

    if s.interp.has_target {
        let gap = (now - s.interp.last_state_time).clamp(
            rules.match_rules.remote_vel_dt_min,
            rules.match_rules.remote_vel_dt_max,
        );
        let sampled = (pos - s.interp.last_state_pos) / gap;
        if s.interp.vel_seeded {
            s.vel = s.vel.lerp(sampled, rules.match_rules.remote_vel_blend);
        } else {
            s.vel = sampled;
            s.interp.vel_seeded = true;
        }
    } else {
        // First pose: snap rather than interpolate (`main.js:849`).
        s.pos = pos;
        s.quat = quat;
        s.interp.has_target = true;
    }

    s.interp.target_pos = pos;
    s.interp.target_quat = quat;
    s.interp.last_state_pos = pos;
    s.interp.last_state_time = now;
    s.interp.boost = boost;
}

/// Replays a shot another client reported.
///
/// A bullet and a missile are re-simulated locally from the origin and
/// direction on the wire, which is the whole reason
/// [`crate::bullets::BulletSpawn::gun`] refuses to inherit the shooter's
/// velocity: given those two vectors, every observer's copy of the bolt follows
/// the identical path. A beam is hitscan and has already been resolved by the
/// shooter, so only the visual is reported.
fn ingest_remote_shot(
    world: &mut World,
    id: EntityId,
    weapon: WeaponKind,
    origin: Vec3,
    dir: Vec3,
    target: Option<EntityId>,
    out: &mut Out,
) {
    match weapon {
        WeaponKind::Bullet => {
            let rules = world.rules;
            let Some(index) = world.ships.iter().position(|s| s.id == id) else {
                return;
            };
            let spawn = BulletSpawn::gun(&rules, origin, dir, &world.ships[index]);
            bullets::spawn_bullet(world, spawn);
        }
        WeaponKind::Missile => {
            missiles::fire(world, id, target);
        }
        WeaponKind::Beam => {}
    }
    out.events.push(SimEvent::Fired {
        owner: id,
        weapon,
        origin,
        dir,
    });
}

// ---------------------------------------------------------------------------
// Phase 1 — the pre-race hold
// ---------------------------------------------------------------------------

/// Runs down a trials countdown, and reports whether the rest of the step is
/// suppressed. `main.js:1198`.
fn hold_for_countdown(world: &mut World, dt: f64) -> bool {
    let Some(trials) = world.trials.as_mut() else {
        return false;
    };
    if !trials.countdown_active {
        return false;
    }
    trials.countdown -= dt;
    if trials.countdown <= 0.0 {
        trials.countdown = 0.0;
        trials.countdown_active = false;
    }
    true
}

// ---------------------------------------------------------------------------
// Phase 2 — clocks
// ---------------------------------------------------------------------------

/// Runs every ship's cooldowns, invulnerability, respawn countdown, health
/// regeneration and damage flash, and respawns whatever came due.
///
/// A ship that reaches zero on its respawn timer is placed immediately, before
/// the next ship's clocks run, so the order depends only on
/// [`World::ships`] order.
fn advance_clocks(world: &mut World, dt: f64, out: &mut Out) {
    let rules = world.rules;
    for i in 0..world.ships.len() {
        let due = {
            let s = &mut world.ships[i];
            bullets::tick_weapon_cooldown(s, dt);
            ship::tick_timers(s, &rules, dt).respawn_due
        };
        if due {
            respawn_ship(world, i, out);
        }
    }
}

/// Places a ship whose respawn countdown expired.
///
/// [`crate::ship::TimerStep::respawn_due`] deliberately reports rather than
/// acts, because *where* a ship comes back is a mode question. The four answers:
///
/// - **Boss hitboxes never respawn here.** Their liveness belongs to
///   [`crate::campaign::activate_boss`] and [`crate::campaign::end_victory`].
/// - **Campaign wave bots never respawn** (`main.js:3281`) — a wave that
///   refilled itself could never be cleared.
/// - **Under [`Authority::Server`] nothing respawns locally.** The pose arrives
///   as [`NetEvent::Respawn`]; guessing one here would fight the server.
/// - Otherwise: the campaign checkpoint for the local player, or the team
///   anchor with spawn jitter for everyone else.
fn respawn_ship(world: &mut World, index: usize, out: &mut Out) {
    let rules = world.rules;
    let (id, kind, is_campaign_bot) = {
        let s = &world.ships[index];
        (s.id, s.kind, s.bot.is_campaign_bot)
    };
    if kind == ShipKind::BossHitbox || is_campaign_bot || world.authority != Authority::Local {
        return;
    }

    let campaign_local = world.campaign.is_some() && world.local_id == Some(id);
    let (pos, quat, hp) = if campaign_local {
        match campaign::respawn_pose(world) {
            // Out of lives, or the mission is over: stay down.
            None => return,
            Some((pos, quat, hp)) => (pos, quat, Some(hp)),
        }
    } else {
        let (pos, quat) = team_spawn(world, index);
        (pos, quat, None)
    };

    ship::respawn(&mut world.ships[index], pos, quat, &rules);
    if let Some(hp) = hp {
        // `main.js:3343` overwrites the hit points two lines after reviving.
        world.ships[index].hp = hp;
    }
    if kind == ShipKind::Bot {
        bot::notify_respawn(&mut world.ships[index], &rules, &mut world.rng.bots);
    }
    out.events.push(SimEvent::ShipRespawned { id, pos });
}

/// The team anchor for a ship, with spawn jitter drawn from
/// [`crate::world::WorldRng::spawn`].
///
/// Team 0 spawns at `-z` facing `+z`; team 1 at `+z` rotated 180° about `y`
/// (`server/index.js:480`). x, y and z jitter are drawn in that order.
fn team_spawn(world: &mut World, index: usize) -> (Vec3, Quat) {
    let rules = world.rules;
    let team = world.ships[index].team.unwrap_or(Team::Zero);
    let (anchor_z, anchor_y, jitter) = match world.map {
        MapKind::Space => (
            rules.spawn.space_z,
            rules.spawn.space_y,
            rules.spawn.space_jitter,
        ),
        MapKind::Terrain => (
            rules.spawn.terrain_z,
            rules.spawn.terrain_y,
            rules.spawn.terrain_jitter,
        ),
    };
    // `*_jitter` is documented as the **full width** of the scatter box, so a
    // signed draw over [-1, 1) has to be halved. Without the halving this
    // scattered twice as wide as `rules.rs` says and as the JS does
    // (`(rand - 0.5) * range`), which matters: the tight box was tuned against
    // the mothership hangar mouth, and the doc records that a wider one drops
    // players outside it and sometimes clips the hull on frame one.
    let rng = &mut world.rng.spawn;
    let jx = rng.next_f64_signed() * jitter.x * 0.5;
    let jy = rng.next_f64_signed() * jitter.y * 0.5;
    let jz = rng.next_f64_signed() * jitter.z * 0.5;
    let (sign, quat) = match team {
        Team::Zero => (-1.0, Quat::IDENTITY),
        Team::One => (1.0, Quat::FLIP_Y),
    };
    (Vec3::new(jx, anchor_y + jy, sign * anchor_z + jz), quat)
}

// ---------------------------------------------------------------------------
// Phase 3 — projectiles
// ---------------------------------------------------------------------------

/// The capital ship's hull, as world boxes, or `None` when there is no boss to
/// shoot at.
///
/// Computed once per step and handed to both weapons, so a bullet and a missile
/// cannot disagree about where the boss is.
fn boss_hull(world: &World) -> Option<[Aabb; 4]> {
    if !campaign::boss_is_engageable(world) {
        return None;
    }
    Some(campaign::boss_hull_boxes(world.campaign.as_ref()?.boss_pos))
}

/// Wraps the hull boxes in the shape [`crate::bullets`] asks for.
fn hull_volumes(hull: Option<&[Aabb; 4]>) -> HullVolumes<'_> {
    match hull {
        Some(boxes) => HullVolumes {
            spheres: &[],
            boxes: boxes.as_slice(),
        },
        None => HullVolumes::EMPTY,
    }
}

/// Advances and resolves every bullet, then routes what it produced.
fn step_bullets(world: &mut World, dt: f64, hull: Option<&[Aabb; 4]>, out: &mut Out) {
    let mut bullet_out = BulletOutput::new();
    bullets::step(world, dt, hull_volumes(hull), &mut bullet_out);
    drain_bullet_output(world, &mut bullet_out, out);
}

/// Moves the events, intents and boss hits a bullet pass produced into the
/// step's own streams.
///
/// [`crate::bullets`] deliberately does not damage the capital ship: it reports
/// a [`crate::bullets::HullHit`] and lets the owner of the hit-point pool decide
/// what it costs. Both shapes of hit — a caller-supplied hull box and one of the
/// twenty hitbox ships — land in [`crate::campaign::apply_boss_damage`], so
/// there is one boss damage path however the shot resolved.
fn drain_bullet_output(world: &mut World, bullet_out: &mut BulletOutput, out: &mut Out) {
    out.events.append(&mut bullet_out.events);
    out.net.append(&mut bullet_out.net_out);
    for hit in bullet_out.hull_hits.drain(..) {
        debug_assert!(matches!(
            hit.part,
            HullPart::Sphere(_) | HullPart::Box(_) | HullPart::Hitbox(_)
        ));
        campaign::apply_boss_damage(world, hit.damage, Some(hit.owner), &mut out.events);
    }
}

/// Advances every missile and flare, then routes each detonation.
fn step_missiles(world: &mut World, dt: f64, hull: Option<&[Aabb; 4]>, out: &mut Out) {
    let storage = hull.map(|b| {
        [
            Volume::Aabb(b[0]),
            Volume::Aabb(b[1]),
            Volume::Aabb(b[2]),
            Volume::Aabb(b[3]),
        ]
    });
    let volumes: &[Volume] = storage.as_ref().map_or(&[][..], <[Volume; 4]>::as_slice);

    let mut detonations: Vec<Detonation> = Vec::new();
    missiles::update(world, dt, volumes, &mut out.events, &mut detonations);
    for detonation in detonations {
        route_detonation(world, detonation, out);
    }
}

/// Turns one missile detonation into score, boss damage and a hit report.
///
/// [`crate::missiles`] applies the hit points and emits the damage events, but
/// it does not know what a scoreboard or a socket is — so the kill credit that
/// [`crate::bullets::apply_ship_damage`] does inline is done here instead, from
/// the same rules, and the `hit` message is raised here too.
fn route_detonation(world: &mut World, detonation: Detonation, out: &mut Out) {
    let missile_damage = world.rules.weapons.missile_damage;
    match detonation.cause {
        DetonationCause::Volume { .. } => {
            campaign::apply_boss_damage(
                world,
                missile_damage,
                Some(detonation.owner),
                &mut out.events,
            );
        }
        DetonationCause::Ship { id, damage, killed } => {
            if is_boss_hitbox(id) {
                // The hitboxes share the boss's pool; `missiles` reports the
                // contact without touching hit points, exactly as `bullets`
                // does.
                campaign::apply_boss_damage(world, damage, Some(detonation.owner), &mut out.events);
            } else {
                if killed {
                    credit_kill(world, id, Some(detonation.owner));
                }
                report_hit(world, detonation.owner, id, WeaponKind::Missile, out);
            }
        }
        DetonationCause::Expired
        | DetonationCause::Flare { .. }
        | DetonationCause::Asteroid { .. }
        | DetonationCause::Obstacle { .. } => {}
    }
}

/// Claims a hit for the server, when the server is the one who decides.
///
/// Only a shot from something simulated on this machine is claimed: a remote
/// player's projectile is re-simulated here for display, and reporting its hits
/// would double-count them. `server/index.js:917` wants a bot's own id spelled
/// out separately.
fn report_hit(world: &World, owner: EntityId, target: EntityId, weapon: WeaponKind, out: &mut Out) {
    if world.authority != Authority::Server {
        return;
    }
    let Some(shooter) = world.ship(owner) else {
        return;
    };
    let from_bot = match shooter.kind {
        ShipKind::Local => None,
        ShipKind::Bot => Some(owner),
        ShipKind::Remote | ShipKind::BossHitbox => return,
    };
    out.net.push(NetIntent::Hit {
        target,
        weapon,
        from_bot,
    });
}

/// Books a kill: team score, the two scoreboard rows, and the solo bot counter.
///
/// The same arithmetic [`crate::bullets::apply_ship_damage`] performs inline.
/// It is repeated rather than shared because that function owns the damage as
/// well as the scoring, and the paths that arrive here — a missile, a collision,
/// an authoritative `death` message — have already applied theirs.
fn credit_kill(world: &mut World, victim: EntityId, killer: Option<EntityId>) {
    let victim_is_bot = world.ship(victim).is_some_and(|s| s.kind == ShipKind::Bot);
    let killer_team = killer.and_then(|k| world.ship(k)).and_then(|s| s.team);
    if world.match_state.active {
        if let Some(team) = killer_team {
            // A self-inflicted death scores for nobody.
            if Some(victim) != killer {
                world.match_state.team_kills[team.index()] += 1;
            }
        }
    }
    if let Some(killer) = killer {
        if let Some(row) = world.match_state.scores.iter_mut().find(|r| r.id == killer) {
            row.kills += 1;
        }
        if world.mode.is_solo() && victim_is_bot && world.local_id == Some(killer) {
            world.match_state.solo_bots_killed += 1;
        }
    }
    if let Some(row) = world.match_state.scores.iter_mut().find(|r| r.id == victim) {
        row.deaths += 1;
    }
}

// ---------------------------------------------------------------------------
// Phase 4 — movers
// ---------------------------------------------------------------------------

/// Flies every [`ShipKind::Local`] ship, resolves it against the world, and
/// charges it for whatever it hit.
///
/// Only `Local` ships use the player flight model. `Remote` ships are
/// interpolated from network state, `Bot` ships have their own model in
/// [`crate::bot`], and `BossHitbox` entries are slaved to the capital ship.
///
/// A ship with no [`Input`] this step gets [`Input::default`]: neutral *input*,
/// not a neutral *ship*. Throttle, velocity and orientation are ship state and
/// survive, so the ship coasts on exactly as [`crate::world::TickFn`] requires;
/// what drops is the held modifiers — trigger, boost, brake — which is the right
/// answer for a frame whose input never arrived.
fn integrate_players(
    world: &mut World,
    inputs: &[Input],
    dt: f64,
    out: &mut Out,
) -> Vec<(EntityId, FlightStep)> {
    let rules = world.rules;
    let mode = world.mode;
    let mut flights = Vec::new();

    for i in 0..world.ships.len() {
        if world.ships[i].kind != ShipKind::Local {
            continue;
        }
        let id = world.ships[i].id;
        let input = inputs
            .iter()
            .find(|input| input.id == id)
            .copied()
            .unwrap_or_default();

        let step = ship::integrate(&mut world.ships[i], &input, &rules, mode, dt);
        let geometry = WorldGeometry {
            asteroids: &world.asteroids,
            obstacles: &world.obstacles,
            boxes: &world.boxes,
            map: world.map,
        };
        let report = ship::resolve_world_collisions(
            &mut world.ships[i],
            step.prev_pos,
            geometry,
            &rules,
            mode,
            &mut world.rng.combat,
        );

        let self_damage = step.self_damage + report.self_damage;
        if self_damage > 0 {
            apply_self_damage(world, i, self_damage, out);
        }
        flights.push((id, step));
    }
    flights
}

/// Runs [`crate::aim_assist::update`] for the local player.
///
/// The only thing this adds is the release input: `main.js:1257` measures the
/// player's intent as `max(|sx|, |sy|)` of the *processed* steering — after the
/// deadzone, the response curve and the arrow-key ramp — which is exactly what
/// [`crate::ship::integrate`] hands back in [`FlightStep::steer`]. Reading it
/// from the flight step rather than from the raw [`Input`] is what keeps a
/// gamepad's 0.04 stick drift from being mistaken for a deliberate correction.
///
/// A world with no local player (a headless server) does no work here.
fn steer_aim_assist(world: &mut World, flights: &[(EntityId, FlightStep)], dt: f64) {
    let Some(id) = world.local_id else {
        return;
    };
    let Some((_, step)) = flights.iter().find(|(flown, _)| *flown == id) else {
        return;
    };
    let steer_mag = step.steer[0].abs().max(step.steer[1].abs());
    aim_assist::update(world, steer_mag, dt);
}

/// Routes brake-overcharge and collision damage through the one damage gate.
///
/// [`crate::ship::integrate`] and [`crate::ship::resolve_world_collisions`] both
/// *report* self-damage rather than applying it, precisely so that the
/// invulnerability check, the death, and the reporting happen once, here.
fn apply_self_damage(world: &mut World, index: usize, amount: i32, out: &mut Out) {
    let rules = world.rules;
    let mode = world.mode;
    let id = world.ships[index].id;
    let result = ship::apply_damage(&mut world.ships[index], amount, &rules, mode);
    if result.applied <= 0 {
        return;
    }
    out.events.push(SimEvent::Damaged {
        id,
        amount: result.applied,
        new_hp: result.hp,
        source: None,
    });
    if world.authority == Authority::Server && world.local_id == Some(id) {
        out.net.push(NetIntent::SelfDamage {
            amount: result.applied,
        });
    }
    if result.killed {
        let pos = world.ships[index].pos;
        out.events.push(SimEvent::ShipDestroyed {
            id,
            killer: None,
            pos,
        });
        credit_kill(world, id, None);
    }
}

/// Chases every remote ship toward the last pose the network reported.
///
/// `main.js:1721`–`:1726`. The position damp is
/// [`crate::rules::MatchRules::remote_lerp_rate`] through
/// [`crate::math::det::exp_neg`]; the orientation uses a normalized lerp rather
/// than the JS's `slerp`, because a slerp needs an `acos` and a `sin` per ship
/// per frame and the two agree to well under a pixel over a 50 ms correction.
/// The shortest arc is taken, so a sign-flipped quaternion does not spin the
/// long way round.
fn interpolate_remotes(world: &mut World, dt: f64) {
    let t = 1.0 - det::exp_neg(world.rules.match_rules.remote_lerp_rate * dt);
    for s in &mut world.ships {
        if s.kind != ShipKind::Remote || !s.interp.has_target {
            continue;
        }
        s.pos = s.pos.lerp(s.interp.target_pos, t);
        s.quat = math::quat_normalize(nlerp(s.quat, s.interp.target_quat, t));
    }
}

/// Normalized linear interpolation between two orientations, along the shorter
/// arc.
fn nlerp(a: Quat, b: Quat, t: f64) -> Quat {
    let dot = a.x * b.x + a.y * b.y + a.z * b.z + a.w * b.w;
    let sign = if dot < 0.0 { -1.0 } else { 1.0 };
    Quat::new(
        a.x + (b.x * sign - a.x) * t,
        a.y + (b.y * sign - a.y) * t,
        a.z + (b.z * sign - a.z) * t,
        a.w + (b.w * sign - a.w) * t,
    )
}

/// The heightfield, in the shape [`crate::bot`] asks for.
///
/// `bot.js` takes ground height as an injected dependency because `terrain.js`
/// is a renderer module in the JS. Here it is [`crate::ship::terrain_height`],
/// and the adapter owns a copy of the rules so it does not borrow the world the
/// bots are about to mutate.
struct RulesTerrain(Rules);

impl TerrainHeight for RulesTerrain {
    fn height_at(&self, x: f64, z: f64) -> f64 {
        ship::terrain_height(x, z, &self.0)
    }
}

/// Runs bot AI, with the heightfield attached on the map that has one.
fn step_bots(world: &mut World, dt: f64, out: &mut Out) {
    if world.map == MapKind::Terrain {
        let ground = RulesTerrain(world.rules);
        bot::update_bots(world, dt, Some(&ground), &mut out.events);
    } else {
        bot::update_bots(world, dt, None, &mut out.events);
    }
}

/// Spends a campaign life if the local player died during this step.
///
/// See the module docs: this is the single point at which a campaign death
/// diverges from a generic one, and it fires at most once per step because the
/// alive-before flag is sampled before any damage lands.
fn settle_campaign_death(world: &mut World, local_was_alive: bool, out: &mut Out) {
    if !local_was_alive || world.campaign.is_none() {
        return;
    }
    let Some(local) = world.local_id else { return };
    if world.ship(local).is_none_or(|s| s.alive) {
        return;
    }
    campaign::on_player_death(world, &mut out.events);
}

// ---------------------------------------------------------------------------
// Phase 5 — weapons
// ---------------------------------------------------------------------------

/// Handles every trigger, launch and countermeasure, then regenerates ammo.
///
/// Order within a ship is `main.js:1459`–`:1529`: cool down (phase 2), fire,
/// then regenerate — so a shot fired this step cannot also be paid for by this
/// step's regeneration.
fn fire_weapons(
    world: &mut World,
    inputs: &[Input],
    dt: f64,
    hull: Option<&[Aabb; 4]>,
    out: &mut Out,
) {
    let rules = world.rules;
    let mut bullet_out = BulletOutput::new();

    for i in 0..world.ships.len() {
        let (id, kind) = {
            let s = &world.ships[i];
            (s.id, s.kind)
        };
        if kind != ShipKind::Local {
            continue;
        }
        let Some(input) = inputs.iter().find(|input| input.id == id).copied() else {
            continue;
        };

        if input.toggle_gun {
            let s = &mut world.ships[i];
            s.gun_mode = match s.gun_mode {
                GunMode::Bullet => GunMode::Beam,
                GunMode::Beam => GunMode::Bullet,
            };
        }
        if input.toggle_aim_assist && world.local_id == Some(id) {
            world.aim_assist.enabled = !world.aim_assist.enabled;
        }
        if !world.ships[i].alive {
            continue;
        }

        if input.fire {
            let basis = basis_of(world.ships[i].quat);
            // The outcome — cooling, overheated, or a shot — is already
            // reflected in the ship's own state and in `bullet_out`; the HUD
            // reads `ammo01` rather than a per-frame verdict.
            let _ = bullets::fire_gun(world, id, basis, hull_volumes(hull), &mut bullet_out);
        }
        if input.fire_missile {
            launch_missile(world, id, out);
        }
        if input.deploy_flare && missiles::deploy_flares(world, id, &mut out.events) > 0 {
            let s = &world.ships[i];
            let (pos, quat) = (s.pos, s.quat);
            if world.authority == Authority::Server && world.local_id == Some(id) {
                out.net.push(NetIntent::Flare { pos, quat });
            }
        }
    }

    drain_bullet_output(world, &mut bullet_out, out);

    for s in &mut world.ships {
        bullets::regen_ammo(s, &rules, dt);
    }
}

/// The `E` key: acquire a lock, spend a round, report the launch.
///
/// [`crate::missiles::fire`] is deliberately silent — it is the launcher a bot
/// uses too — so the [`SimEvent::Fired`] and the outbound `fire` message are
/// raised here, where the target that was locked is still in hand.
fn launch_missile(world: &mut World, id: EntityId, out: &mut Out) {
    let Some(target) = missiles::acquire_lock(world, id) else {
        // `main.js:1423` keeps the round on the rail when there is nothing to
        // shoot at.
        return;
    };
    if missiles::fire(world, id, Some(target)).is_none() {
        return;
    }
    let Some(missile) = world.missiles.last() else {
        return;
    };
    let (origin, dir) = (missile.pos, missile.dir);
    out.events.push(SimEvent::Fired {
        owner: id,
        weapon: WeaponKind::Missile,
        origin,
        dir,
    });
    if world.authority == Authority::Server && world.local_id == Some(id) {
        out.net.push(NetIntent::Fire {
            weapon: WeaponKind::Missile,
            origin,
            dir,
            target: Some(target),
        });
    }
}

/// A ship's local axes in world space, for the muzzle transform.
fn basis_of(quat: Quat) -> ShipBasis {
    ShipBasis {
        right: math::right(quat),
        up: math::up(quat),
        forward: math::forward(quat),
    }
}

// ---------------------------------------------------------------------------
// Phase 7 — the match clock
// ---------------------------------------------------------------------------

/// Counts the match clock down, and ends the match at zero.
///
/// Skipped under [`Authority::Server`], where the timer is overwritten by
/// [`NetEvent::MatchState`] and the end is announced by
/// [`NetEvent::MatchEnd`] (`main.js:876`); counting locally as well would make
/// the HUD stutter every time a message landed.
fn advance_match_clock(world: &mut World, dt: f64, out: &mut Out) {
    if !world.match_state.active || world.match_state.over || world.authority != Authority::Local {
        return;
    }
    world.match_state.timer -= dt;
    if world.match_state.timer > 0.0 {
        return;
    }
    world.match_state.timer = 0.0;
    world.match_state.over = true;
    let [zero, one] = world.match_state.team_kills;
    let winner = match zero.cmp(&one) {
        std::cmp::Ordering::Greater => Some(Team::Zero),
        std::cmp::Ordering::Less => Some(Team::One),
        std::cmp::Ordering::Equal => None,
    };
    out.events.push(SimEvent::MatchEnded { winner });
}

// ---------------------------------------------------------------------------
// Phase 8 — outbound
// ---------------------------------------------------------------------------

/// Emits the periodic pose updates, at
/// [`crate::rules::MatchRules::state_send_interval`].
///
/// The cadence is derived from [`World::time`] crossing a multiple of the
/// interval rather than from an accumulator field, so it needs no state, cannot
/// drift, and survives a snapshot: two worlds at the same `time` send on the
/// same steps.
fn emit_state_intents(world: &World, dt: f64, flights: &[(EntityId, FlightStep)], out: &mut Out) {
    if world.authority != Authority::Server {
        return;
    }
    let interval = world.rules.match_rules.state_send_interval;
    if interval <= 0.0 || dt <= 0.0 {
        return;
    }
    let now = world.time;
    if (now / interval).floor() <= ((now - dt) / interval).floor() {
        return;
    }

    if let Some(local) = world.local_ship() {
        let boost = flights
            .iter()
            .find(|(id, _)| *id == local.id)
            .is_some_and(|(_, step)| step.boosting);
        out.net.push(NetIntent::State {
            pos: local.pos,
            quat: local.quat,
            boost,
        });
    }
    // The host drives the bots and relays their poses (`main.js:1712`).
    for s in &world.ships {
        if s.kind == ShipKind::Bot {
            out.net.push(NetIntent::BotState {
                id: s.id,
                pos: s.pos,
                quat: s.quat,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// The frame
// ---------------------------------------------------------------------------

/// Narrows the world to the flat `f32` snapshot the renderer consumes.
///
/// One-way, always: nothing here is ever read back into [`World`], which is what
/// makes the narrowing free of simulation consequences. See the [`crate::world`]
/// module docs.
fn build_frame(world: &World, out: Out, flights: &[(EntityId, FlightStep)]) -> Frame {
    let mut frame = Frame::new();
    frame.tick = world.tick;
    frame.time = world.time;

    for s in &world.ships {
        let flight = flights
            .iter()
            .find(|(id, _)| *id == s.id)
            .map(|(_, step)| *step);
        let boosting = flight.is_some_and(|f| f.boosting) || s.interp.boost;
        let flags = ShipFlags::NONE
            .with_if(s.alive, ShipFlags::ALIVE)
            .with_if(boosting, ShipFlags::BOOSTING)
            .with_if(flight.is_some_and(|f| f.braking), ShipFlags::BRAKING)
            .with_if(s.invuln_timer > 0.0, ShipFlags::INVULN)
            .with_if(s.kind == ShipKind::Bot, ShipFlags::BOT)
            .with_if(world.local_id == Some(s.id), ShipFlags::LOCAL)
            .with_if(s.kind == ShipKind::BossHitbox, ShipFlags::BOSS_HITBOX);
        frame.ships.push(ShipView {
            id: s.id,
            team: s.team.map_or(-1, |t| t.index() as i32),
            hp: s.hp,
            flags,
            pos: vec3f(s.pos),
            quat: quatf(s.quat),
            vel: vec3f(s.vel),
            hit_flash: s.hit_flash as f32,
        });
    }

    for b in &world.bullets {
        frame.bullets.push(ProjView {
            key: b.key,
            pos: vec3f(b.pos),
            dir: vec3f(b.vel.normalize()),
        });
    }
    for m in &world.missiles {
        frame.missiles.push(missile_view(m));
    }
    for f in &world.flares {
        frame.flares.push(flare_view(f));
    }
    for a in &world.asteroids {
        frame.asteroids.push(RockView {
            id: a.id,
            hp: a.hp,
            pos: vec3f(a.pos),
            rot: vec3f(a.rot),
            size: a.size as f32,
            hit_flash: a.hit_flash as f32,
        });
    }

    frame.boss = boss_view(world);
    frame.hud = hud_state(world, flights);
    frame.events = out.events;
    frame.net_out = out.net;
    frame
}

fn missile_view(m: &Missile) -> ProjView {
    ProjView {
        key: m.key,
        pos: vec3f(m.pos),
        dir: vec3f(m.dir),
    }
}

fn flare_view(f: &Flare) -> FlareView {
    FlareView {
        key: f.key,
        pos: vec3f(f.pos),
        age: f.age as f32,
        life: f.life as f32,
    }
}

/// The capital ship, drawn only while the campaign is running.
fn boss_view(world: &World) -> Option<BossView> {
    if !matches!(world.mode, Mode::Campaign(_)) {
        return None;
    }
    campaign::boss_view(world)
}

/// Everything the HUD and the cockpit dash read.
fn hud_state(world: &World, flights: &[(EntityId, FlightStep)]) -> HudState {
    let rules = &world.rules;
    let mut hud = HudState {
        match_timer: world.match_state.timer as f32,
        team_kills: world.match_state.team_kills,
        assist_target: world.aim_assist.locked_target().unwrap_or(-1),
        trials: trials_hud(world),
        campaign: campaign::campaign_hud(world),
        ..HudState::default()
    };

    let Some(local) = world.local_ship() else {
        return hud;
    };
    hud.throttle01 = (local.throttle / rules.ship.max_throttle) as f32;
    hud.speed = local.vel.length() as f32;
    hud.hp = local.hp;
    hud.hp01 = (f64::from(local.hp) / f64::from(rules.ship.max_hp)) as f32;
    hud.ammo01 = (local.ammo / rules.weapons.max_ammo) as f32;
    hud.boost01 = (local.boost_meter / rules.ship.max_boost) as f32;
    hud.charge01 = local.brake_charge as f32;
    hud.overcharge01 = (local.brake_overcharge_time / rules.ship.brake_overcharge_damage_delay)
        .clamp(0.0, 1.0) as f32;
    hud.missiles = local.missiles_left;
    hud.flares = local.flares_left;
    hud.gun_mode = local.gun_mode;
    hud.invuln = local.invuln_timer > 0.0;
    hud.missile_lock_warning = missiles::is_targeting(world, local.id);
    if let Some((_, step)) = flights.iter().find(|(id, _)| *id == local.id) {
        hud.steer = [step.steer[0] as f32, step.steer[1] as f32];
    }
    hud
}

/// The trials readout, zeroed outside a trial.
fn trials_hud(world: &World) -> TrialsHud {
    let Some(trials) = world.trials.as_ref() else {
        return TrialsHud::default();
    };
    let next = trials
        .checkpoints
        .get(trials.next_cp)
        .copied()
        .unwrap_or(Vec3::ZERO);
    TrialsHud {
        active: true,
        running: trials.running,
        lap: trials.lap,
        timer: trials.timer as f32,
        // A negative value is "no time yet", which is what the HUD field
        // documents; `Option<f64>` does not survive the `#[repr(C)]` narrowing.
        best_lap: trials.best_lap.map_or(-1.0, |t| t as f32),
        last_lap: trials.last_lap.map_or(-1.0, |t| t as f32),
        next_cp: trials.next_cp as u32,
        next_cp_pos: vec3f(next),
        countdown: trials.countdown as f32,
    }
}

fn vec3f(v: Vec3) -> [f32; 3] {
    [v.x as f32, v.y as f32, v.z as f32]
}

fn quatf(q: Quat) -> [f32; 4] {
    [q.x as f32, q.y as f32, q.z as f32, q.w as f32]
}
