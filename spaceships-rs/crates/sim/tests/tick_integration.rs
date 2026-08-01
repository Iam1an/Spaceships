//! Integration tests for the assembled tick.
//!
//! Every module in `spaceships-sim` has its own unit tests, and all of them
//! pass with the modules wired together wrongly — or not wired at all, which is
//! how the crate shipped before [`tick`] existed. These tests only ever call
//! `tick`, through the crate's public API, and assert on things that can only be
//! true if the phases run in the right order with the right data flowing
//! between them:
//!
//! - the whole match is reproducible from a seed (`determinism`);
//! - a bullet fired through the loop damages what it is aimed at;
//! - a bot engages, fires, and connects, end to end;
//! - a missile locks, flies, and detonates, end to end;
//! - spawn protection survives every path, not just the one module that checks
//!   it;
//! - a campaign death spends exactly one life;
//! - and the phase order that all of the above rests on is pinned, so
//!   rearranging it fails here rather than in a desync six months later.

use spaceships_sim::math::Vec3;
use spaceships_sim::rules::Rules;
use spaceships_sim::tick::tick;
use spaceships_sim::world::{
    Bullet, CampaignPhase, CampaignState, ExplosionKind, Input, MapKind, Mode, Quat, Ship,
    ShipKind, SimEvent, Team, Turret, WeaponKind, World, TICK_DT,
};
use spaceships_sim::{asteroids, bot};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A space world with the static geometry removed.
///
/// The moon sits at the origin with radius 80 and the motherships at `z = ±600`,
/// which is the correct world and the wrong test rig: most of what follows puts
/// ships within a few tens of units of the origin, where they would be inside
/// the moon. The tests that care about world geometry are `ship.rs`'s.
fn bare_world(seed: u64, mode: Mode) -> World {
    let mut world = World::new(seed, Rules::DEFAULT, mode, MapKind::Space);
    world.obstacles.clear();
    world.boxes.clear();
    world
}

/// Adds a ship that can be hit and can be flown, and returns its index.
fn push_ship(world: &mut World, id: i32, team: Team, pos: Vec3) -> usize {
    let rules = world.rules;
    let mut ship = Ship::spawn(id, ShipKind::Local, pos, Quat::IDENTITY, &rules);
    ship.team = Some(team);
    // Spawn protection is real and universal; the tests that are *about* it
    // switch it back on explicitly.
    ship.invuln_timer = 0.0;
    world.ships.push(ship);
    world.ships.len() - 1
}

/// A shooter at the origin facing `+z` and a target `distance` downrange.
fn duel(distance: f64) -> World {
    let mut world = bare_world(0xD0E1, Mode::Skirmish);
    push_ship(&mut world, 1, Team::Zero, Vec3::ZERO);
    push_ship(&mut world, 2, Team::One, Vec3::new(0.0, 0.0, distance));
    world.local_id = Some(1);
    world
}

fn hold_trigger(id: i32) -> Input {
    Input {
        id,
        fire: true,
        ..Input::default()
    }
}

fn idle(id: i32) -> Input {
    Input {
        id,
        ..Input::default()
    }
}

/// Runs `steps` ticks and returns every event they produced, in order.
fn run(world: &mut World, inputs: &[Input], steps: u32) -> Vec<SimEvent> {
    let mut events = Vec::new();
    for _ in 0..steps {
        events.extend(tick(world, inputs, &[], TICK_DT).events);
    }
    events
}

fn hp_of(world: &World, id: i32) -> i32 {
    world.ship(id).expect("ship is missing").hp
}

// ---------------------------------------------------------------------------
// Determinism — the property everything else rests on
// ---------------------------------------------------------------------------

/// A busy solo match: a player, three bots, and a full asteroid field.
///
/// Every random stream is exercised — the field at construction, spawn jitter on
/// respawn, combat rolls on rock collisions, bot decisions every tick, and flare
/// directions when the script presses `Q`.
fn scripted_match(seed: u64) -> World {
    let mut world = World::new(seed, Rules::DEFAULT, Mode::Skirmish, MapKind::Space);
    asteroids::populate(&mut world);
    let rules = world.rules;

    let mut player = Ship::spawn(
        1,
        ShipKind::Local,
        Vec3::new(0.0, 0.0, -420.0),
        Quat::IDENTITY,
        &rules,
    );
    player.team = Some(Team::Zero);
    world.ships.push(player);
    world.local_id = Some(1);

    for (i, z) in [180.0, 220.0, 260.0].into_iter().enumerate() {
        let mut b = Ship::spawn(
            10 + i as i32,
            ShipKind::Bot,
            Vec3::new(i as f64 * 25.0, 0.0, z),
            Quat::FLIP_Y,
            &rules,
        );
        b.team = Some(Team::One);
        bot::init(&mut b, i % 2 == 0, false, &rules, &mut world.rng.bots);
        world.ships.push(b);
    }
    world
}

/// Deterministic pilot input: no clock, no randomness, just the tick number.
fn scripted_input(step: u64) -> Input {
    Input {
        id: 1,
        steer_x: (step % 37) as f64 / 37.0 - 0.5,
        steer_y: (step % 23) as f64 / 23.0 - 0.5,
        roll: if step % 5 == 0 { 1.0 } else { 0.0 },
        throttle_axis: 1.0,
        fire: step % 3 == 0,
        boost: step % 11 < 4,
        braking: step % 29 < 3,
        fire_missile: step % 197 == 40,
        deploy_flare: step % 211 == 90,
        ..Input::default()
    }
}

/// Every `f64` in the world that a divergence could hide in, as raw bits.
///
/// `PartialEq` on `World` is already a deep comparison, but it compares floats
/// by value: `0.0 == -0.0`, and a `NaN` never equals itself, so a world full of
/// `NaN` would compare unequal for the wrong reason and a sign-flipped zero
/// would compare equal for the wrong one. Bits are the property this crate
/// actually promises.
fn state_bits(world: &World) -> Vec<u64> {
    fn push(bits: &mut Vec<u64>, v: Vec3) {
        bits.extend(v.to_array().iter().map(|c| c.to_bits()));
    }

    let mut bits = Vec::new();
    for s in &world.ships {
        push(&mut bits, s.pos);
        push(&mut bits, s.vel);
        bits.extend(s.quat.to_array().iter().map(|c| c.to_bits()));
        bits.push(s.throttle.to_bits());
        bits.push(s.ammo.to_bits());
        bits.push(s.boost_meter.to_bits());
        bits.push(s.hp as u64);
    }
    for b in &world.bullets {
        push(&mut bits, b.pos);
        push(&mut bits, b.vel);
        bits.push(b.life.to_bits());
    }
    for m in &world.missiles {
        push(&mut bits, m.pos);
        push(&mut bits, m.dir);
    }
    for f in &world.flares {
        push(&mut bits, f.pos);
        push(&mut bits, f.vel);
    }
    for a in &world.asteroids {
        push(&mut bits, a.pos);
        push(&mut bits, a.rot);
    }
    bits.push(world.time.to_bits());
    bits
}

#[test]
fn a_fixed_seed_match_replays_bit_identically() {
    const STEPS: u64 = 900; // fifteen seconds

    let replay = |seed: u64| {
        let mut world = scripted_match(seed);
        let mut last = None;
        for step in 0..STEPS {
            last = Some(tick(&mut world, &[scripted_input(step)], &[], TICK_DT));
        }
        (world, last.expect("at least one frame"))
    };

    let (a, frame_a) = replay(0x5EED);
    let (b, frame_b) = replay(0x5EED);

    assert_eq!(a.tick, STEPS, "the tick counter must advance once per step");
    assert_eq!(
        state_bits(&a),
        state_bits(&b),
        "the same seed and the same inputs must reproduce the same bits"
    );
    assert!(
        a == b,
        "the whole world must compare equal, not just the floats"
    );
    assert_eq!(
        frame_a, frame_b,
        "and so must the frame handed to the renderer"
    );

    // A different seed must actually produce a different match, or the test
    // above would pass on a simulation that ignores its inputs.
    let (c, _) = replay(0x5EEE);
    assert_ne!(state_bits(&a), state_bits(&c));
}

#[test]
fn a_fixed_seed_campaign_replays_bit_identically() {
    // The campaign adds the boss patrol, twenty hitbox ships, turret fire drawn
    // from the combat stream, and the wave spawner drawn from the spawn stream.
    let replay = || {
        let mut world = bare_world(0xB055, Mode::Campaign(3));
        push_ship(&mut world, 1, Team::Zero, Vec3::new(0.0, 0.0, 380.0));
        world.local_id = Some(1);
        spaceships_sim::campaign::init(&mut world, true);
        spaceships_sim::campaign::activate_boss(&mut world, &mut Vec::new());
        for step in 0..600u64 {
            tick(&mut world, &[scripted_input(step)], &[], TICK_DT);
        }
        world
    };
    assert_eq!(state_bits(&replay()), state_bits(&replay()));
}

// ---------------------------------------------------------------------------
// A bullet, through the assembled loop
// ---------------------------------------------------------------------------

#[test]
fn a_bullet_fired_through_the_tick_damages_the_ship_it_is_aimed_at() {
    let mut world = duel(40.0);
    let events = run(&mut world, &[hold_trigger(1)], 20);

    assert!(
        hp_of(&world, 2) < Rules::DEFAULT.ship.max_hp,
        "a ship shot at 40 units must lose hit points"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            SimEvent::Damaged {
                id: 2,
                source: Some(1),
                ..
            }
        )),
        "the damage must be attributed to the shooter"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            SimEvent::Fired {
                owner: 1,
                weapon: WeaponKind::Bullet,
                ..
            }
        )),
        "and the shot itself must be reported for the muzzle flash"
    );
    // The shooter is never its own target, however close the muzzle sits.
    assert_eq!(hp_of(&world, 1), Rules::DEFAULT.ship.max_hp);
}

#[test]
fn friendly_fire_still_does_not_land_once_the_phases_are_wired_together() {
    let mut world = bare_world(7, Mode::Skirmish);
    push_ship(&mut world, 1, Team::Zero, Vec3::ZERO);
    push_ship(&mut world, 2, Team::Zero, Vec3::new(0.0, 0.0, 40.0));
    world.local_id = Some(1);
    run(&mut world, &[hold_trigger(1)], 20);
    assert_eq!(hp_of(&world, 2), Rules::DEFAULT.ship.max_hp);
}

// ---------------------------------------------------------------------------
// A bot, end to end
// ---------------------------------------------------------------------------

#[test]
fn a_bot_engages_fires_and_scores_a_hit_end_to_end() {
    let rules = Rules::DEFAULT;
    let mut world = bare_world(0xB07, Mode::Training);
    push_ship(&mut world, 1, Team::Zero, Vec3::ZERO);
    world.local_id = Some(1);

    let mut enemy = Ship::spawn(
        2,
        ShipKind::Bot,
        Vec3::new(0.0, 0.0, 260.0),
        Quat::FLIP_Y,
        &rules,
    );
    enemy.team = Some(Team::One);
    enemy.invuln_timer = 0.0;
    bot::init(&mut enemy, false, false, &rules, &mut world.rng.bots);
    world.ships.push(enemy);

    // The player holds still. Everything that happens is the bot's doing.
    let events = run(&mut world, &[idle(1)], 1_800);

    assert!(
        events.iter().any(|e| matches!(
            e,
            SimEvent::Fired {
                owner: 2,
                weapon: WeaponKind::Bullet,
                ..
            }
        )),
        "the bot never fired"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            SimEvent::Damaged {
                id: 1,
                source: Some(2),
                ..
            }
        )),
        "the bot fired but never connected — its rounds are not reaching the \
         shared bullet list, or they are resolving against the wrong pose"
    );
}

// ---------------------------------------------------------------------------
// A missile, end to end
// ---------------------------------------------------------------------------

#[test]
fn a_missile_locks_flies_and_detonates_end_to_end() {
    let rules = Rules::DEFAULT;
    let mut world = duel(320.0);
    let missiles_before = world.ship(1).expect("shooter").missiles_left;

    let launch = Input {
        id: 1,
        fire_missile: true,
        ..Input::default()
    };
    let mut events = tick(&mut world, &[launch], &[], TICK_DT).events;
    assert_eq!(world.missiles.len(), 1, "the E key must put one round up");
    assert_eq!(
        world.ship(1).expect("shooter").missiles_left,
        missiles_before - 1,
        "and spend exactly one"
    );
    assert!(events.iter().any(|e| matches!(
        e,
        SimEvent::Fired {
            owner: 1,
            weapon: WeaponKind::Missile,
            ..
        }
    )));

    // 320 units at 160 u/s is two seconds; the round lives for eight. Stop on
    // the step it goes off — health regeneration starts two seconds after the
    // last hit, so waiting any longer would measure the repair, not the damage.
    let mut detonated = false;
    for _ in 0..400 {
        let frame = tick(&mut world, &[idle(1)], &[], TICK_DT);
        detonated = frame.events.iter().any(|e| {
            matches!(
                e,
                SimEvent::Explosion {
                    kind: ExplosionKind::MissileHit,
                    ..
                }
            )
        });
        events.extend(frame.events);
        if detonated {
            break;
        }
    }

    assert!(detonated, "no detonation was reported");
    assert!(
        world.missiles.is_empty(),
        "the missile never left the world"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            SimEvent::Damaged {
                id: 2,
                source: Some(1),
                ..
            }
        )),
        "the damage must be attributed to whoever fired the missile"
    );
    assert_eq!(
        hp_of(&world, 2),
        rules.ship.max_hp - rules.weapons.missile_damage,
        "a missile that reaches its target must take {} hit points off it",
        rules.weapons.missile_damage
    );
}

#[test]
fn a_flare_burst_survives_the_loop_and_can_seduce_a_missile() {
    let mut world = duel(320.0);
    // The target releases countermeasures the moment the round is up.
    let launch = Input {
        id: 1,
        fire_missile: true,
        ..Input::default()
    };
    tick(&mut world, &[launch], &[], TICK_DT);

    let flares = Input {
        id: 2,
        deploy_flare: true,
        ..Input::default()
    };
    let events = tick(&mut world, &[idle(1), flares], &[], TICK_DT).events;
    assert_eq!(
        world.flares.len(),
        Rules::DEFAULT.weapons.flare_count as usize,
        "one Q press releases a whole burst"
    );
    assert!(events
        .iter()
        .any(|e| matches!(e, SimEvent::FlareBurst { owner: 2, .. })));

    run(&mut world, &[idle(1)], 400);
    assert!(
        world.flares.is_empty(),
        "flares must burn out through the loop, not accumulate"
    );
}

// ---------------------------------------------------------------------------
// Spawn protection, across every path
// ---------------------------------------------------------------------------

#[test]
fn spawn_invulnerability_holds_across_the_whole_loop() {
    let rules = Rules::DEFAULT;
    let invuln_ticks = (rules.combat.spawn_invuln / TICK_DT).round() as u32;

    let mut world = duel(40.0);
    world.ship_mut(2).expect("target").invuln_timer = rules.combat.spawn_invuln;

    // Five ticks short of the window closing.
    run(&mut world, &[hold_trigger(1)], invuln_ticks - 5);
    assert!(
        world.ship(2).expect("target").invuln_timer > 0.0,
        "the test needs the window still open here"
    );
    assert_eq!(
        hp_of(&world, 2),
        rules.ship.max_hp,
        "a ship inside its spawn window must take no damage from anything the \
         tick routes — bullets, missiles, or collisions"
    );

    // Now let it close, and confirm the shots that were bouncing off start
    // landing. Short enough that the target cannot die and respawn to full.
    run(&mut world, &[hold_trigger(1)], 15);
    assert_eq!(world.ship(2).expect("target").invuln_timer, 0.0);
    assert!(
        hp_of(&world, 2) < rules.ship.max_hp,
        "the same fire must connect once protection lapses"
    );
}

#[test]
fn a_missile_will_not_detonate_on_a_protected_ship() {
    let rules = Rules::DEFAULT;
    let mut world = duel(200.0);
    // Long enough to outlast the missile's whole flight.
    world.ship_mut(2).expect("target").invuln_timer = 30.0;

    let launch = Input {
        id: 1,
        fire_missile: true,
        ..Input::default()
    };
    tick(&mut world, &[launch], &[], TICK_DT);
    run(&mut world, &[idle(1)], 600);

    assert_eq!(
        hp_of(&world, 2),
        rules.ship.max_hp,
        "spawn protection has to hold on the missile path too, which is the \
         one `missiles.js` never checked"
    );
}

// ---------------------------------------------------------------------------
// The campaign death path
// ---------------------------------------------------------------------------

/// A campaign with no wave on the field, so the only thing under test is the
/// death path.
fn quiet_campaign() -> World {
    let mut world = bare_world(0xCA47, Mode::Campaign(1));
    let rules = world.rules;
    push_ship(&mut world, 1, Team::Zero, Vec3::ZERO);
    push_ship(&mut world, 2, Team::One, Vec3::new(0.0, 0.0, 30.0));
    world.local_id = Some(1);
    world.ship_mut(1).expect("player").hp = 15;
    // Face the shooter at the player.
    world.ship_mut(2).expect("shooter").quat = Quat::FLIP_Y;

    world.campaign = Some(CampaignState {
        mission: 1,
        phase: CampaignPhase::Wave,
        wave_index: 0,
        wave_bot_ids: Vec::new(),
        bots_alive: 0,
        between: false,
        between_timer: 0.0,
        lives: rules.campaign.lives,
        checkpoint_pos: Vec3::new(0.0, 0.0, -540.0),
        next_bot_id: rules.campaign.first_bot_id,
        warp_timer: 0.0,
        boss_hp: rules.campaign.boss_max_hp,
        boss_active: false,
        boss_pos: rules.campaign.boss_base_pos,
        boss_time: 0.0,
        turrets: [Turret::default(); 4],
    });
    world
}

#[test]
fn a_campaign_death_goes_through_exactly_one_path() {
    let rules = Rules::DEFAULT;
    let mut world = quiet_campaign();

    let mut died = false;
    for _ in 0..400 {
        tick(&mut world, &[hold_trigger(2)], &[], TICK_DT);
        if !world.ship(1).expect("player").alive {
            died = true;
            break;
        }
    }
    assert!(died, "the player should have been shot down");

    let camp = world.campaign.as_ref().expect("campaign state");
    assert_eq!(
        camp.lives,
        rules.campaign.lives - 1,
        "exactly one life per death — two would mean `bullets` and `campaign` \
         both settled it"
    );
    assert!(camp.warp_timer > 0.0, "the warp-in effect must be armed");
    assert_eq!(
        world.ship(1).expect("player").respawn_timer,
        rules.combat.campaign_respawn_delay,
        "the campaign timer must be the one that survives; `bullets` sets the \
         generic {} s delay first and the campaign path overwrites it",
        rules.combat.respawn_delay
    );

    // And the player actually comes back, at the checkpoint, hurt. Sampled on
    // the step it happens: health regeneration would otherwise top it up.
    let mut revived = false;
    for _ in 0..400 {
        tick(&mut world, &[idle(2)], &[], TICK_DT);
        if world.ship(1).expect("player").alive {
            revived = true;
            break;
        }
    }
    assert!(revived, "the respawn never fired");
    let checkpoint = world
        .campaign
        .as_ref()
        .expect("campaign state")
        .checkpoint_pos;
    let player = world.ship(1).expect("player");
    assert_eq!(
        player.pos, checkpoint,
        "the campaign respawn goes to the checkpoint, not the team anchor"
    );
    assert_eq!(
        player.hp,
        spaceships_sim::campaign::respawn_hp(&rules),
        "a campaign respawn comes back at 55 %, not full"
    );
}

// ---------------------------------------------------------------------------
// The capital ship
// ---------------------------------------------------------------------------

/// A boss fight with the player parked square in front of the hull.
fn boss_fight() -> World {
    let mut world = bare_world(0xB055F, Mode::Campaign(3));
    let nose = world.rules.campaign.boss_base_pos + Vec3::new(0.0, 0.0, -260.0);
    push_ship(&mut world, 1, Team::Zero, nose);
    world.local_id = Some(1);
    spaceships_sim::campaign::init(&mut world, false);
    // `init` puts wave 1 on the field; this test is about the boss, so clear it
    // and jump straight to the fight.
    world.ships.retain(|s| !s.bot.is_campaign_bot);
    spaceships_sim::campaign::activate_boss(&mut world, &mut Vec::new());
    if let Some(camp) = world.campaign.as_mut() {
        camp.wave_bot_ids.clear();
    }
    // Clear the hitboxes' own spawn protection so the fight can start at once.
    for s in &mut world.ships {
        s.invuln_timer = 0.0;
    }
    world
}

#[test]
fn a_bullet_damages_the_capital_ship_through_the_tick() {
    let mut world = boss_fight();
    let start = world.campaign.as_ref().expect("campaign").boss_hp;

    let events = run(&mut world, &[hold_trigger(1)], 120);

    let hp = world.campaign.as_ref().expect("campaign").boss_hp;
    assert!(
        hp < start,
        "the hull volumes are not reaching `bullets::step` — a bullet aimed \
         down the middle of a 268-unit capital ship hit nothing"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            SimEvent::Damaged {
                id: spaceships_sim::rules::BOSS_ID_BASE,
                ..
            }
        )),
        "boss damage must be reported against the shared pool, not per hitbox"
    );
    // One pool: every hitbox mirrors it, so nothing can read a stale number.
    for s in &world.ships {
        if spaceships_sim::world::is_boss_hitbox(s.id) {
            assert_eq!(s.hp, hp, "hitbox {} disagrees with the health bar", s.id);
        }
    }
}

#[test]
fn a_missile_damages_the_capital_ship_through_the_tick() {
    let mut world = boss_fight();
    let start = world.campaign.as_ref().expect("campaign").boss_hp;

    let launch = Input {
        id: 1,
        fire_missile: true,
        ..Input::default()
    };
    tick(&mut world, &[launch], &[], TICK_DT);
    assert_eq!(world.missiles.len(), 1, "the boss must be lockable");
    run(&mut world, &[idle(1)], 400);

    let hp = world.campaign.as_ref().expect("campaign").boss_hp;
    assert_eq!(
        start - hp,
        Rules::DEFAULT.weapons.missile_damage,
        "a missile must take exactly its own damage off the boss pool — once, \
         whether it resolved against a hull box or a hitbox ship"
    );
}

#[test]
fn the_boss_turret_rounds_are_ordinary_bullets_that_reach_the_player() {
    let rules = Rules::DEFAULT;
    let mut world = boss_fight();

    // Long enough for the staggered reload (up to 3.5 s) plus flight time.
    let events = run(&mut world, &[idle(1)], 900);

    assert!(
        events.iter().any(|e| matches!(
            e,
            SimEvent::Fired {
                owner: spaceships_sim::rules::BOSS_ID_BASE,
                ..
            }
        )),
        "the turrets never fired"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            SimEvent::Damaged {
                id: 1,
                amount,
                ..
            } if *amount == rules.weapons.boss_bullet_damage
        )),
        "turret rounds share `World::bullets`, so they must be stepped and \
         resolved by the same code every other bullet goes through"
    );
}

// ---------------------------------------------------------------------------
// The ordering the whole assembly rests on
// ---------------------------------------------------------------------------

/// One bullet, parked a hair from expiry, at `pos`, owned by ship 1.
///
/// Its remaining life clamps its travel to a fraction of a unit
/// (`bullets::step` moves a bullet by `vel * min(dt, life)`), so it samples a
/// *point* rather than a segment. That removes the bullet's own motion from the
/// experiment and leaves exactly one variable: which pose the target is at when
/// the sweep runs.
fn park_bullet(world: &mut World, pos: Vec3) {
    let key = world.take_projectile_key();
    let rules = world.rules;
    world.bullets.push(Bullet {
        key,
        pos,
        prev_pos: pos,
        vel: Vec3::Z * rules.weapons.bullet_speed,
        life: 1.0e-6,
        owner: 1,
        owner_team: Some(Team::Zero),
        owner_coarse_aim: false,
        damage: rules.weapons.gun_damage,
    });
}

#[test]
fn projectiles_resolve_before_ships_move() {
    // `bullets::resolve_impact` and `missiles::first_contact` both sweep a
    // target over `Ship::pos .. Ship::pos + vel * dt`. That is the step's true
    // segment only while `pos` is still the *start*-of-step pose, so phase 3
    // must precede phase 4. Both halves below flip their answer if the two are
    // ever swapped.
    let rules = Rules::DEFAULT;
    let fast = Vec3::new(0.0, 0.0, -3_000.0);

    // Ask the flight model itself how far that is in one step, so retuning the
    // velocity blend cannot silently invalidate the geometry.
    let travel = {
        let mut probe = Ship::spawn(9, ShipKind::Local, Vec3::ZERO, Quat::IDENTITY, &rules);
        probe.vel = fast;
        spaceships_sim::ship::integrate(
            &mut probe,
            &Input::default(),
            &rules,
            Mode::Skirmish,
            TICK_DT,
        );
        probe.pos.z
    };
    assert!(
        travel < -30.0,
        "the probe must clear a hit radius in one step; got {travel}"
    );

    // -- A. In reach at the start of the step, far away by the end. -----------
    let mut world = bare_world(1, Mode::Skirmish);
    push_ship(&mut world, 1, Team::Zero, Vec3::new(0.0, 0.0, 400.0));
    push_ship(&mut world, 2, Team::One, Vec3::ZERO);
    world.local_id = Some(1);
    world.ship_mut(2).expect("target").vel = fast;
    park_bullet(&mut world, Vec3::new(0.0, 0.0, -6.0));

    tick(&mut world, &[], &[], TICK_DT);
    assert!(
        hp_of(&world, 2) < rules.ship.max_hp,
        "the bullet was 6.0 from the target's start-of-step pose, inside the \
         6.5 reach — if this misses, ships are moving before projectiles \
         resolve and every sweep is testing the wrong interval"
    );

    // -- B. Out of reach at the start, on top of the bullet by the end. -------
    let mut world = bare_world(1, Mode::Skirmish);
    push_ship(&mut world, 1, Team::Zero, Vec3::new(0.0, 0.0, 400.0));
    push_ship(&mut world, 2, Team::One, Vec3::new(0.0, 0.0, -60.0));
    world.local_id = Some(1);
    world.ship_mut(2).expect("target").vel = -fast;
    // Where the target will *end* the step — 46 units from where it starts.
    park_bullet(&mut world, Vec3::new(0.0, 0.0, -60.0 - travel));

    tick(&mut world, &[], &[], TICK_DT);
    assert_eq!(
        hp_of(&world, 2),
        rules.ship.max_hp,
        "the bullet is parked on the target's end-of-step pose and 46 units \
         from its start-of-step one — a hit here means projectiles are being \
         resolved after the movers"
    );
}

#[test]
fn a_shot_fired_this_step_is_resolved_on_the_next_one() {
    // Weapons are phase 5 and projectiles are phase 3, so a round leaves the
    // muzzle at the end-of-step pose and is first swept on the following tick.
    // That is the invariant `bullets::spawn_bullet` documents when it seeds
    // `prev_pos == pos`; without it a bullet would sweep backwards through the
    // ship that fired it on its first step.
    let rules = Rules::DEFAULT;
    // Five units downrange: the muzzle sits 0.6 ahead of the nose, so the bolt
    // is born already inside the target's 6.5-unit reach.
    let mut world = duel(5.0);

    tick(&mut world, &[hold_trigger(1)], &[], TICK_DT);
    assert_eq!(world.bullets.len(), 1, "the trigger must produce a round");
    assert_eq!(
        hp_of(&world, 2),
        rules.ship.max_hp,
        "a round that has not been stepped yet cannot have hit anything"
    );

    tick(&mut world, &[idle(1)], &[], TICK_DT);
    assert!(
        hp_of(&world, 2) < rules.ship.max_hp,
        "and on the next step it must land"
    );
}

#[test]
fn bots_move_before_they_fire() {
    // `bot::plan_bot` computes its muzzle from the *post*-move pose, which is
    // only meaningful because `tick` runs the AI as a mover and the resulting
    // round is not swept until the next step. If firing were hoisted ahead of
    // movement the muzzle would trail the bot by a step.
    let rules = Rules::DEFAULT;
    let mut world = bare_world(0xB07F, Mode::Training);
    push_ship(&mut world, 1, Team::Zero, Vec3::ZERO);
    world.local_id = Some(1);

    let mut shooter = Ship::spawn(
        2,
        ShipKind::Bot,
        Vec3::new(0.0, 0.0, 200.0),
        Quat::FLIP_Y,
        &rules,
    );
    shooter.team = Some(Team::One);
    shooter.invuln_timer = 0.0;
    bot::init(&mut shooter, true, false, &rules, &mut world.rng.bots);
    world.ships.push(shooter);

    for _ in 0..600 {
        tick(&mut world, &[idle(1)], &[], TICK_DT);
        if let Some(bullet) = world.bullets.iter().find(|b| b.owner == 2) {
            let bot_pos = world.ship(2).expect("bot").pos;
            let offset = bullet.prev_pos.distance(bot_pos);
            assert!(
                offset < rules.bot.muzzle_offset + 1.0,
                "the round was born {offset} from the bot; it should sit at the \
                 muzzle, {} ahead of the post-move pose",
                rules.bot.muzzle_offset
            );
            return;
        }
    }
    panic!("the bot never fired");
}

// ---------------------------------------------------------------------------
// Frame assembly
// ---------------------------------------------------------------------------

#[test]
fn the_frame_describes_the_world_the_step_just_produced() {
    let mut world = duel(60.0);
    let frame = tick(&mut world, &[hold_trigger(1)], &[], TICK_DT);

    assert_eq!(frame.tick, world.tick);
    assert_eq!(frame.time, world.time);
    assert_eq!(frame.ships.len(), world.ships.len());
    assert_eq!(frame.bullets.len(), world.bullets.len());
    assert_eq!(frame.asteroids.len(), world.asteroids.len());
    assert!(
        frame.boss.is_none(),
        "there is no capital ship in a skirmish"
    );

    // Ships come out in `World::ships` order, which is what lets the renderer
    // keep an id -> mesh map without a second lookup.
    for (view, ship) in frame.ships.iter().zip(&world.ships) {
        assert_eq!(view.id, ship.id);
    }

    let local = &frame.ships[0];
    assert!(local
        .flags
        .contains(spaceships_sim::world::ShipFlags::LOCAL));
    assert!(local
        .flags
        .contains(spaceships_sim::world::ShipFlags::ALIVE));
    assert_eq!(frame.hud.hp, hp_of(&world, 1));
    assert!(
        frame.hud.ammo01 < 1.0,
        "the HUD must show the round that was just spent"
    );
    assert_eq!(
        frame.hud.missiles,
        world.ship(1).expect("player").missiles_left
    );
}

#[test]
fn a_server_authoritative_match_reports_its_shots_and_its_pose() {
    use spaceships_sim::world::{Authority, NetEvent, NetIntent};

    let mut world = bare_world(0x9E7, Mode::Multiplayer);
    assert_eq!(
        world.authority,
        Authority::Server,
        "multiplayer is not local"
    );
    push_ship(&mut world, 1, Team::Zero, Vec3::ZERO);
    world.local_id = Some(1);

    // A remote arrives the way it does over the wire: as a pose, for an id this
    // world has never seen.
    let arrival = NetEvent::RemoteState {
        id: 2,
        pos: Vec3::new(0.0, 0.0, 40.0),
        quat: Quat::FLIP_Y,
        boost: false,
    };
    tick(&mut world, &[idle(1)], &[arrival], TICK_DT);
    let remote = world.ship(2).expect("the pose must create the ship");
    assert_eq!(remote.kind, ShipKind::Remote);
    assert_eq!(
        remote.pos,
        Vec3::new(0.0, 0.0, 40.0),
        "the first pose snaps"
    );
    world.ship_mut(2).expect("remote").team = Some(Team::One);
    world.ship_mut(2).expect("remote").invuln_timer = 0.0;

    let mut intents = Vec::new();
    for _ in 0..60 {
        intents.extend(tick(&mut world, &[hold_trigger(1)], &[], TICK_DT).net_out);
    }

    assert!(
        intents.iter().any(|i| matches!(
            i,
            NetIntent::Fire {
                weapon: WeaponKind::Bullet,
                ..
            }
        )),
        "a shot must be announced so other clients can draw it"
    );
    assert!(
        intents.iter().any(|i| matches!(
            i,
            NetIntent::Hit {
                target: 2,
                weapon: WeaponKind::Bullet,
                from_bot: None,
            }
        )),
        "and the hit must be claimed, because the server does not check geometry"
    );
    // Pose updates go out at the state-send interval, not once per tick: 60
    // ticks is one second, and the interval is 20 Hz. The count can be one
    // short of the nominal rate because the window does not start on a
    // boundary, which is exactly the behaviour a stateless cadence has.
    let poses = intents
        .iter()
        .filter(|i| matches!(i, NetIntent::State { .. }))
        .count();
    let nominal =
        (60.0 * TICK_DT / Rules::DEFAULT.match_rules.state_send_interval).round() as usize;
    assert!(
        poses == nominal || poses + 1 == nominal,
        "expected about {nominal} pose updates in a second, got {poses}"
    );
}

#[test]
fn a_solo_match_emits_no_network_traffic() {
    // `Authority::Local` means nothing is reported: the JS learned this the hard
    // way, sending every solo asteroid hit to a socket that did not exist.
    let mut world = duel(40.0);
    for _ in 0..60 {
        let frame = tick(&mut world, &[hold_trigger(1)], &[], TICK_DT);
        assert!(frame.net_out.is_empty(), "solo must not talk to a server");
    }
}
