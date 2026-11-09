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
    // Armed at the start, so the script below actually sets a pulse off inside
    // fifteen seconds rather than spending the whole replay charging. This is
    // the only way the EMP — the charge meter, the blindness clock, and the
    // widened bot aim that follows it — gets into the determinism replay at all.
    player.emp_charge = 1.0;
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
        // Twice in fifteen seconds: the first press fires, the second finds a
        // meter that has had six seconds of a sixty-second charge and does
        // nothing. Both paths are therefore in the replay.
        fire_emp: step % 401 == 120,
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
        bits.push(s.emp_charge.to_bits());
        bits.push(s.emp_blind.to_bits());
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
// The EMP, end to end
// ---------------------------------------------------------------------------

/// The whole weapon in one press: the meter empties, the sphere goes off, the
/// pilot inside it loses their instruments and keeps everything else.
#[test]
fn an_emp_blinds_the_bandit_through_the_tick_and_costs_it_no_hit_points() {
    let rules = Rules::DEFAULT;
    let mut world = duel(100.0);
    world.ship_mut(1).expect("me").emp_charge = 1.0;

    let bandit_before = world.ship(2).expect("bandit").clone();
    let press = Input {
        id: 1,
        fire_emp: true,
        ..Input::default()
    };
    let events = tick(&mut world, &[press], &[], TICK_DT).events;

    assert!(
        events
            .iter()
            .any(|e| matches!(e, SimEvent::EmpBurst { owner: 1, .. })),
        "the burst has to reach the renderer, or nothing is drawn or heard"
    );
    assert!(
        !events.iter().any(|e| matches!(e, SimEvent::Fired { .. })),
        "an EMP is not a shot and must not raise one — that would give it a \
         muzzle flash and a gun report"
    );

    let bandit = world.ship(2).expect("bandit");
    assert_eq!(bandit.emp_blind, rules.emp.blind_duration);
    assert_eq!(bandit.hp, bandit_before.hp, "this weapon does no damage");
    assert_eq!(bandit.ammo, bandit_before.ammo, "and takes no ammunition");
    assert_eq!(
        bandit.missiles_left, bandit_before.missiles_left,
        "and empties no stores"
    );
    assert_eq!(
        world.ship(1).expect("me").emp_charge,
        0.0,
        "one press, one meter"
    );
    assert_eq!(
        world.ship(1).expect("me").emp_blind,
        0.0,
        "the pilot who fired can still see"
    );

    // And it wears off through the loop rather than needing anything to clear
    // it. One extra tick, because the burst landed after this step's clocks.
    let ticks = (rules.emp.blind_duration / TICK_DT).ceil() as u32 + 1;
    run(&mut world, &[idle(1)], ticks);
    assert_eq!(world.ship(2).expect("bandit").emp_blind, 0.0);
}

/// The half of §2 that is a promise rather than an effect: a blinded pilot flies
/// and shoots exactly as before. Only the aids go.
#[test]
fn a_blinded_pilot_keeps_the_stick_and_the_gun_and_loses_the_aids() {
    let mut world = duel(300.0);
    // The blinded pilot is the *local* one, so aim assist and the missile
    // launcher are both being asked about the ship the EMP caught.
    world.ship_mut(1).expect("me").emp_blind = 4.0;

    // Short, because the gun is held down and the bandit is only 300 units
    // away: long enough for rounds to arrive and not long enough to kill it.
    let fighting = Input {
        id: 1,
        throttle_axis: 1.0,
        fire: true,
        fire_missile: true,
        ..Input::default()
    };
    let start = world.ship(1).expect("me").pos;
    let events = run(&mut world, &[fighting], 30);

    assert!(
        events.iter().any(|e| matches!(
            e,
            SimEvent::Fired {
                owner: 1,
                weapon: WeaponKind::Bullet,
                ..
            }
        )),
        "the gun still fires: they lost the crosshair, not the trigger"
    );
    assert!(!world.bullets.is_empty(), "and the rounds are really there");
    assert_eq!(
        world.ship(1).expect("me").missiles_left,
        Rules::DEFAULT.weapons.missile_max,
        "no lock, so the round stays on the rail and is not wasted"
    );
    assert!(world.missiles.is_empty());
    assert!(
        !world.aim_assist.has_target,
        "no cone, no pull, and therefore no lead marker"
    );
    assert_eq!(
        world.aim_assist.strength_smoothed, 0.0,
        "the assist releases rather than freezing at its last strength"
    );

    // Trigger off, throttle still forward: the airframe accelerates exactly as
    // it always did while its panel is dark.
    let cruising = Input {
        id: 1,
        throttle_axis: 1.0,
        ..Input::default()
    };
    run(&mut world, &[cruising], 60);
    let me = world.ship(1).expect("me");
    assert!(me.throttle > 0.0 && me.vel.length() > 0.0);
    assert!(
        me.pos.distance(start) > 1.0,
        "flight controls are not the EMP's business"
    );

    // The moment it clears, everything comes back — the assist without having
    // to reacquire from cold, and the missile off the rail.
    world.ship_mut(1).expect("me").emp_blind = 0.0;
    run(&mut world, &[cruising], 10);
    assert!(world.aim_assist.has_target, "systems restored");
    let relaunch = Input {
        id: 1,
        fire_missile: true,
        ..Input::default()
    };
    tick(&mut world, &[relaunch], &[], TICK_DT);
    assert_eq!(
        world.ship(1).expect("me").missiles_left,
        Rules::DEFAULT.weapons.missile_max - 1,
        "and the launcher works again"
    );
}

/// A blinded pilot's *lock warning* goes out too, because the receiver is part
/// of the panel — even though the missile chasing them is still very real.
#[test]
fn the_missile_lock_warning_dies_with_the_rest_of_the_avionics() {
    let mut world = duel(300.0);
    // Ship 2 shoots at ship 1, so the local pilot is the one being tracked.
    let launch = Input {
        id: 2,
        fire_missile: true,
        ..Input::default()
    };
    tick(&mut world, &[idle(1), launch], &[], TICK_DT);
    let frame = tick(&mut world, &[idle(1)], &[], TICK_DT);
    assert!(
        frame.hud.missile_lock_warning,
        "the test needs a live missile tracking the local ship"
    );

    world.ship_mut(1).expect("me").emp_blind = 4.0;
    let frame = tick(&mut world, &[idle(1)], &[], TICK_DT);
    assert!(!frame.hud.missile_lock_warning, "the receiver is dark");
    assert!(
        !world.missiles.is_empty(),
        "and the missile is still coming, which is the entire point"
    );
    assert!(frame.hud.emp_blind > 0.0, "the HUD says why instead");
}

/// A blinded bot loses the same two things a blinded human does — the seeker and
/// the threat receiver — and keeps its gun with a much worse aim.
#[test]
fn a_blinded_bot_stops_launching_and_starts_missing() {
    let rules = Rules::DEFAULT;
    let mut world = bare_world(0xB11D, Mode::Skirmish);
    push_ship(&mut world, 1, Team::Zero, Vec3::new(0.0, 0.0, 0.0));
    world.local_id = Some(1);

    let mut bandit = Ship::spawn(
        2,
        ShipKind::Bot,
        Vec3::new(0.0, 0.0, 400.0),
        Quat::FLIP_Y,
        &rules,
    );
    bandit.team = Some(Team::One);
    bandit.invuln_timer = 0.0;
    bot::init(&mut bandit, false, false, &rules, &mut world.rng.bots);
    bandit.bot.missile_timer = 0.0;
    bandit.emp_blind = 4.0;
    world.ships.push(bandit);

    run(&mut world, &[idle(1)], 120);
    assert_eq!(
        world.ship(2).expect("bandit").bot.missiles_left,
        rules.bot.missile_max_for(false),
        "a blinded bot has nothing to lock with, so it launches nothing"
    );
    assert!(world.missiles.is_empty());

    // And once it can see again the same bot does launch, which is what makes
    // the assertion above about the EMP rather than about the fixture.
    world.ship_mut(2).expect("bandit").emp_blind = 0.0;
    run(&mut world, &[idle(1)], 240);
    assert!(
        world.ship(2).expect("bandit").bot.missiles_left < rules.bot.missile_max_for(false),
        "systems restored"
    );
}

/// The multiplayer contract, both directions: firing raises a message, and
/// receiving one detonates locally without spending anything.
#[test]
fn an_emp_crosses_the_wire_as_a_centre_and_is_re_detonated_on_arrival() {
    use spaceships_sim::world::{NetEvent, NetIntent};

    let mut world = bare_world(0xE111, Mode::Multiplayer);
    push_ship(&mut world, 1, Team::Zero, Vec3::ZERO);
    push_ship(&mut world, 2, Team::One, Vec3::new(0.0, 0.0, 60.0));
    world.local_id = Some(1);
    world.ships[1].kind = ShipKind::Remote;
    world.ship_mut(1).expect("me").emp_charge = 1.0;

    let press = Input {
        id: 1,
        fire_emp: true,
        ..Input::default()
    };
    let out = tick(&mut world, &[press], &[], TICK_DT).net_out;
    assert!(
        out.iter()
            .any(|i| matches!(i, NetIntent::Emp { pos } if *pos == Vec3::ZERO)),
        "the centre is what goes out; who it caught is each peer's own answer"
    );

    // Now the other direction. A pulse the *remote* pilot set off, reported
    // back as an event: it must blind this client's ship and must not touch
    // this client's meter, which belongs to a different aircraft.
    world.ship_mut(1).expect("me").emp_charge = 0.4;
    world.ship_mut(1).expect("me").emp_blind = 0.0;
    let inbound = NetEvent::EmpBurst {
        id: 2,
        pos: Vec3::new(0.0, 0.0, 60.0),
    };
    let frame = tick(&mut world, &[idle(1)], &[inbound], TICK_DT);
    assert!(
        world.ship(1).expect("me").emp_blind > 0.0,
        "a pulse from somebody else has to reach this cockpit"
    );
    assert_eq!(
        world.ship(1).expect("me").emp_charge,
        0.4 + TICK_DT / Rules::DEFAULT.emp.charge_time,
        "and must cost this ship nothing but the tick's ordinary charging"
    );
    assert!(frame
        .events
        .iter()
        .any(|e| matches!(e, SimEvent::EmpBurst { owner: 2, .. })));
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
        owner_is_bot: false,
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
    // Fourteen units downrange. The muzzle sits 12.2 ahead of the ship's origin
    // and 0.75 under it (`WeaponRules::muzzle_offset` — the gun is under the
    // nose of a 13.65-unit airframe), so the bolt is born 1.95 from the target
    // and already inside its 6.5-unit reach. The old 5.0 was chosen against a
    // 0.6 muzzle and now puts the target *behind* the gun.
    let mut world = duel(14.0);

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
            let bot = world.ship(2).expect("bot");
            // The bot fires through the player's launcher, so its round is born
            // at the player's gun port — `bot.js:304`'s own 2.5-ahead is gone.
            // A step of movement would move it by at most `speed * TICK_DT`.
            let want = spaceships_sim::bullets::muzzle_origin(
                bot.pos,
                spaceships_sim::bullets::ShipBasis::of(bot.quat),
                &rules,
            );
            let offset = bullet.prev_pos.distance(want);
            assert!(
                offset < rules.bot.speed * TICK_DT,
                "the round was born {offset} from the muzzle of the post-move \
                 pose; a whole step of drift is only {}",
                rules.bot.speed * TICK_DT
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
fn every_projectile_in_the_frame_says_whose_it_is() {
    use spaceships_sim::world::Allegiance;

    // Three shooters, one of each relationship to the player, all firing into
    // the same frame. Without this the renderer paints the whole sky in the
    // local palette and incoming fire is indistinguishable from outgoing.
    let mut world = bare_world(0xA11E, Mode::Skirmish);
    push_ship(&mut world, 1, Team::Zero, Vec3::ZERO);
    push_ship(&mut world, 2, Team::Zero, Vec3::new(40.0, 0.0, 0.0));
    push_ship(&mut world, 3, Team::One, Vec3::new(0.0, 0.0, 300.0));
    world.local_id = Some(1);

    let frame = tick(
        &mut world,
        &[hold_trigger(1), hold_trigger(2), hold_trigger(3)],
        &[],
        TICK_DT,
    );
    assert_eq!(frame.bullets.len(), 3, "one bolt from each shooter");

    let of = |owner: i32| {
        let key = world
            .bullets
            .iter()
            .find(|b| b.owner == owner)
            .expect("no bolt from that shooter")
            .key;
        frame
            .bullets
            .iter()
            .find(|b| b.key == key)
            .expect("the frame dropped a bullet")
            .allegiance
    };
    assert_eq!(of(1), Allegiance::Own);
    assert_eq!(of(2), Allegiance::Ally);
    assert_eq!(of(3), Allegiance::Hostile);

    // And the events say it too, so a beam and a muzzle flash are painted the
    // same way the bolt is.
    for (owner, want) in [(1, Allegiance::Own), (3, Allegiance::Hostile)] {
        assert!(
            frame.events.iter().any(|e| matches!(
                e,
                SimEvent::Fired { owner: o, allegiance, .. } if *o == owner && *allegiance == want
            )),
            "no {want:?} shot reported for ship {owner}"
        );
    }

    // Missiles carry the same signal, off the launch-time team snapshot.
    let launch = |id: i32| Input {
        id,
        fire_missile: true,
        ..Input::default()
    };
    let frame = tick(&mut world, &[launch(1), launch(3)], &[], TICK_DT);
    let mut seen: Vec<Allegiance> = frame.missiles.iter().map(|m| m.allegiance).collect();
    seen.sort_unstable_by_key(|a| *a as u32);
    assert_eq!(seen, vec![Allegiance::Own, Allegiance::Hostile]);
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
fn a_hosted_bot_announces_its_own_gunfire() {
    use spaceships_sim::world::{Authority, NetIntent};

    // The defect this pins: in the JS a balance bot the host drives damaged
    // remote players with rounds those players never saw, because `sim` only
    // reported `fire` for the local ship and nothing sent `bot-fire`.
    let rules = Rules::DEFAULT;
    let mut world = bare_world(0xB07F, Mode::Multiplayer);
    assert_eq!(world.authority, Authority::Server);
    push_ship(&mut world, 1, Team::Zero, Vec3::ZERO);
    world.local_id = Some(1);

    // A bot on the far team, close enough to open fire, driven here.
    let mut hosted = Ship::spawn(
        -3,
        ShipKind::Bot,
        Vec3::new(0.0, 0.0, 200.0),
        Quat::FLIP_Y,
        &rules,
    );
    hosted.team = Some(Team::One);
    hosted.invuln_timer = 0.0;
    bot::init(&mut hosted, false, false, &rules, &mut world.rng.bots);
    world.ships.push(hosted);

    let mut intents = Vec::new();
    let mut shots = 0;
    for _ in 0..900 {
        let frame = tick(&mut world, &[idle(1)], &[], TICK_DT);
        shots += frame
            .events
            .iter()
            .filter(|e| matches!(e, SimEvent::Fired { owner: -3, .. }))
            .count();
        intents.extend(frame.net_out);
    }

    let announced = intents
        .iter()
        .filter(|i| {
            matches!(
                i,
                NetIntent::Fire {
                    from_bot: Some(-3),
                    ..
                }
            )
        })
        .count();
    assert!(shots > 0, "the bot never fired in fifteen seconds");
    assert_eq!(
        shots, announced,
        "{shots} bot shots but {announced} of them reached the wire"
    );
    // And the player's own id is never borrowed for them: `fire` and `bot-fire`
    // are different messages and the server credits them differently.
    assert!(
        !intents
            .iter()
            .any(|i| matches!(i, NetIntent::Fire { from_bot: None, .. })),
        "the player fired nothing, so no plain `fire` may go out"
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

// ---------------------------------------------------------------------------
// Aim assist
// ---------------------------------------------------------------------------

/// A duel with the target 300 units downrange and 22 degrees off the nose —
/// inside the 53-degree cone, well outside the dead angle.
fn assisted_duel() -> World {
    let mut world = duel(300.0);
    world.ship_mut(2).expect("target").pos = Vec3::new(120.0, 0.0, 300.0);
    world
}

#[test]
fn aim_assist_turns_the_nose_and_lights_the_hud_through_the_tick() {
    // `aim_assist::update` has its own unit tests. This one only asserts that
    // the tick calls it at all, with the local ship, and that the result
    // reaches both the ship's pose and `HudState`.
    let want = Vec3::new(120.0, 0.0, 300.0).normalize();

    // Switched off by hand: assist now *defaults* on
    // (`AimAssistState::default`), so this half of the test has to ask for the
    // disabled case rather than assume it.
    let mut off = assisted_duel();
    off.aim_assist.enabled = false;
    let frame = tick(&mut off, &[idle(1)], &[], TICK_DT);
    assert_eq!(
        frame.hud.assist_target, -1,
        "a disabled assist draws no lock"
    );
    assert_eq!(off.ship(1).expect("pilot").quat, Quat::IDENTITY);

    let mut on = assisted_duel();
    on.aim_assist.enabled = true;
    let frame = tick(&mut on, &[idle(1)], &[], TICK_DT);
    assert_eq!(
        frame.hud.assist_target, 2,
        "the HUD lock comes from the sim"
    );

    let nose = spaceships_sim::math::forward(on.ship(1).expect("pilot").quat);
    assert!(
        nose.dot(want) > Vec3::Z.dot(want),
        "the nose should have moved toward the target, not away from it"
    );
    assert!(
        nose.dot(want) < 1.0,
        "and only part of the way: this is a nudge, not a snap"
    );
}

#[test]
fn the_assist_defaults_on_for_a_mouse_pilot() {
    // The JS reads a persisted `localStorage` flag (`main.js:996`) that a
    // returning pilot has almost always set. With no settings store here, a
    // faithful `false` means assist is off at every launch and `C` is the only
    // way to it — which is how "there is no aim assist" gets reported. It
    // engages without anyone having to know the binding.
    let mut world = assisted_duel();
    assert!(world.aim_assist.enabled, "a fresh world has assist on");
    assert!(
        !world.ship(1).expect("pilot").coarse_aim,
        "and this pilot is on the precise profile, not force-enabled",
    );

    let frame = tick(&mut world, &[idle(1)], &[], TICK_DT);
    assert_eq!(frame.hud.assist_target, 2, "it locks with no key pressed");
}

#[test]
fn the_c_key_toggles_aim_assist_through_the_tick() {
    // Phase 5 owns the toggle and phase 4 owns the pull, so `C` lands on the
    // step *after* the one it is pressed on — the same order `main.js` has, where
    // `applyAimAssist` runs at `:1256` and the key edge is read at `:1385`.
    let mut world = assisted_duel();
    // From off, so the sequence below reads on-then-off. The default is on; see
    // `the_assist_defaults_on_for_a_mouse_pilot`.
    world.aim_assist.enabled = false;
    let press = Input {
        id: 1,
        toggle_aim_assist: true,
        ..Input::default()
    };

    let frame = tick(&mut world, &[press], &[], TICK_DT);
    assert_eq!(
        frame.hud.assist_target, -1,
        "the press has not taken effect yet"
    );
    assert!(world.aim_assist.enabled);

    let frame = tick(&mut world, &[idle(1)], &[], TICK_DT);
    assert_eq!(frame.hud.assist_target, 2);

    let frame = tick(&mut world, &[press], &[], TICK_DT);
    assert_eq!(frame.hud.assist_target, 2, "still on for this step");
    assert!(!world.aim_assist.enabled);

    let frame = tick(&mut world, &[idle(1)], &[], TICK_DT);
    assert_eq!(frame.hud.assist_target, -1, "and off from the next one");
}

#[test]
fn aim_assist_solves_against_the_start_of_step_poses() {
    // Phase placement, pinned. `main.js:1256` runs the assist from inside the
    // flight block, long before the remote interpolation at `:1721` and the bot
    // update, so it always solves the intercept against where everything *was*
    // at the top of the step. The remote below has a network pose waiting for
    // it, so `interpolate_remotes` will move it during this same step: run the
    // assist after that phase instead of before it and the direction below
    // changes, and this fails.
    let mut world = bare_world(0xA551, Mode::Skirmish);
    push_ship(&mut world, 1, Team::Zero, Vec3::ZERO);
    world.local_id = Some(1);
    world.aim_assist.enabled = true;

    let target_pos = Vec3::new(120.0, 0.0, 300.0);
    let target_vel = Vec3::new(200.0, 0.0, 0.0);
    let rules = world.rules;
    let mut remote = Ship::spawn(2, ShipKind::Remote, target_pos, Quat::IDENTITY, &rules);
    remote.team = Some(Team::One);
    remote.invuln_timer = 0.0;
    remote.vel = target_vel;
    remote.interp.has_target = true;
    remote.interp.target_pos = target_pos + target_vel * 0.05;
    remote.interp.target_quat = Quat::IDENTITY;
    world.ships.push(remote);

    tick(&mut world, &[idle(1)], &[], TICK_DT);
    assert_ne!(
        world.ship(2).expect("target").pos,
        target_pos,
        "the remote must actually move this step, or the test proves nothing"
    );

    let t = spaceships_sim::math::solve_intercept(
        target_pos,
        target_vel,
        Vec3::ZERO,
        Vec3::ZERO,
        Rules::DEFAULT.weapons.bullet_speed,
    )
    .expect("the target is slower than a bullet");
    let want = (target_pos + target_vel * t).normalize();
    assert!(
        world.aim_assist.target_dir.abs_diff_eq(want, 1e-12),
        "aimed at {:?}, wanted {want:?}",
        world.aim_assist.target_dir
    );
}

// ---------------------------------------------------------------------------
// Asteroid collisions
// ---------------------------------------------------------------------------

/// Flying into a rock costs 15..=29 hit points, once — not your life.
///
/// `main.js:2215` charges on the *rising edge* of contact, so resting against a
/// rock is free and re-entering charges again. The report is that a rock kills
/// outright, which would mean either the edge is not being detected or the
/// charge is landing every tick.
#[test]
fn a_rock_costs_one_hit_not_the_ship() {
    let mut world = bare_world(0x4D0C, Mode::Skirmish);
    let idx = push_ship(&mut world, 1, Team::Zero, Vec3::new(0.0, 0.0, -40.0));
    world.local_id = Some(1);
    world.ships[idx].throttle = 80.0;
    world.ships[idx].vel = Vec3::new(0.0, 0.0, 80.0);
    world.asteroids.push(spaceships_sim::world::Asteroid {
        id: 0,
        pos: Vec3::ZERO,
        size: 10.0 / world.rules.world.asteroid_field.collision_radius_scale,
        radius: 10.0,
        hp: 5,
        tier: spaceships_sim::world::AsteroidTier::Medium,
        variant: 0,
        rot: Vec3::ZERO,
        spin: Vec3::ZERO,
        hit_flash: 0.0,
    });

    let start = hp_of(&world, 1);
    for _ in 0..240 {
        tick(&mut world, &[hold_forward(1)], &[], TICK_DT);
    }
    let lost = start - hp_of(&world, 1);
    assert!(
        lost <= world.rules.combat.asteroid_collision_damage_max,
        "one rock took {lost} hp over four seconds of contact",
    );
    assert!(
        world.ship(1).expect("ship").alive,
        "the rock killed the pilot"
    );
}

/// A cluster bills once, not once per rock.
///
/// `asteroids::populate` runs no separation test, so overlapping rocks are
/// normal rather than exotic, and `main.js:2215` bills inside the per-asteroid
/// loop: four stacked rocks cost 49 of 100 hit points in a single pass and a
/// denser knot killed outright. One impact, one charge.
#[test]
fn a_cluster_of_rocks_bills_once() {
    let mut world = bare_world(0x4D0D, Mode::Skirmish);
    push_ship(&mut world, 1, Team::Zero, Vec3::new(0.0, 0.0, -40.0));
    world.local_id = Some(1);
    world.ships[0].vel = Vec3::new(0.0, 0.0, 80.0);
    // Four rocks on top of each other, which the generator can and does make.
    for id in 0..4u32 {
        world.asteroids.push(spaceships_sim::world::Asteroid {
            id,
            pos: Vec3::new(f64::from(id) * 2.0, 0.0, 0.0),
            size: 10.0 / world.rules.world.asteroid_field.collision_radius_scale,
            radius: 10.0,
            hp: 5,
            tier: spaceships_sim::world::AsteroidTier::Medium,
            variant: 0,
            rot: Vec3::ZERO,
            spin: Vec3::ZERO,
            hit_flash: 0.0,
        });
    }

    let start = hp_of(&world, 1);
    for _ in 0..240 {
        tick(&mut world, &[hold_forward(1)], &[], TICK_DT);
    }
    let lost = start - hp_of(&world, 1);
    assert!(
        lost <= world.rules.combat.asteroid_collision_damage_max,
        "four stacked rocks billed {lost} hp; one impact is one charge",
    );
    assert!(lost > 0, "and it still costs something");
    assert!(world.ship(1).expect("ship").alive);
}

fn hold_forward(id: i32) -> Input {
    Input {
        id,
        throttle_axis: 1.0,
        ..Input::default()
    }
}

/// **Bots can fly the Sierras.**
///
/// A map is not finished when it looks right. The terrain was rebuilt with
/// 590-unit ranges, ravines with near-vertical walls, and a lake whose surface
/// is solid — and `bot.rs` navigates all of it with one lookahead sample and a
/// pull-up, tuned against a heightfield that no longer exists. If the new relief
/// were beyond it, half of every skirmish would be bots flying into hillsides,
/// and nothing else in the suite would notice: every other terrain test asks
/// where the ground *is*, not whether anything can fly over it.
///
/// The assertion is on clearance rather than on a death count, because a death
/// count cannot say what killed anyone — these bots are on two teams and shoot
/// each other, which is the point of putting them on two teams. Clearance is the
/// stronger claim anyway: no bot ever entered the kill volume at all.
///
/// Measured at the time of writing: minimum clearance 35 units across three
/// seeds and 60 seconds of flying, against a hard floor of 5. The margin is the
/// avoidance working, not the clamp catching them.
#[test]
fn bots_can_fly_the_terrain_map() {
    let rules = Rules::DEFAULT;
    for seed in [1u64, 7, 99] {
        let mut w = World::new(seed, rules, Mode::Skirmish, MapKind::Terrain);
        let mut rng = spaceships_sim::rng::Rng::new(seed);
        // Ten bots scattered over the whole map — ranges, ravines, lake and
        // coast alike — rather than the one clear corridor a fixture would pick.
        for i in 0..10i32 {
            let x = rng.range_f64(-1500.0, 1500.0);
            let z = rng.range_f64(-1500.0, 1500.0);
            let ground = spaceships_sim::ship::terrain_height(x, z, &rules);
            let mut s = Ship::spawn(
                100 + i,
                ShipKind::Bot,
                Vec3::new(x, ground + 120.0, z),
                Quat::IDENTITY,
                &rules,
            );
            s.team = Some(if i % 2 == 0 { Team::Zero } else { Team::One });
            w.ships.push(s);
        }

        let mut lowest = f64::INFINITY;
        let mut moved = 0.0f64;
        let start: Vec<Vec3> = w.ships.iter().map(|s| s.pos).collect();
        for _ in 0..(60 * 60) {
            tick(&mut w, &[], &[], TICK_DT);
            for s in &w.ships {
                if s.alive {
                    let g = spaceships_sim::ship::terrain_height(s.pos.x, s.pos.z, &rules);
                    lowest = lowest.min(s.pos.y - g);
                }
            }
        }
        for (i, s) in w.ships.iter().enumerate() {
            moved = moved.max(s.pos.distance(start[i]));
        }

        // Against the bot's own floor, not against the kill plane. Clearing
        // the kill plane by a hair is what the old numbers did — the floor was
        // set to the same 5 as the clearance, so a bot on the clamp sat exactly
        // on the altitude that kills it and passed only because the test is a
        // strict `<`. The floor has to be a real gap for this to mean anything.
        //
        // The tolerance is arithmetic, not slack. `bot::plan_bot` clamps with
        // `pos.y = height_at(x, z) + clearance` and this recovers the clearance
        // by subtracting the same height straight back off; over an airfield
        // mesa at 210 units that round trip loses a few ulps, and a bot sitting
        // exactly on the clamp reads 17.999999999999968. Asserting on the exact
        // 18.0 makes the test a lottery on which bot happens to be on the floor.
        const CLAMP_EPS: f64 = 1e-9;
        assert!(
            lowest >= rules.bot.terrain_min_clearance - CLAMP_EPS,
            "seed {seed}: a bot closed to {lowest}, under its own {:.1} floor",
            rules.bot.terrain_min_clearance,
        );
        assert!(
            rules.bot.terrain_min_clearance > rules.world.terrain_kill_clearance,
            "the bot's floor must sit above the plane that kills it",
        );
        // And they were actually flying, not parked in a corner: a fixture that
        // never moves would pass the clearance check trivially.
        assert!(
            moved > 500.0,
            "seed {seed}: the bots barely moved ({moved:.0})"
        );
    }
}

/// **A bot leaves its runway on a climb-out, not on a launch.**
///
/// `bot.js:205` adds `pull * 6` to an already-normalised heading's `y`, which is
/// not a bounded operation. Sitting on its own pad a bot has 40 units of
/// clearance against a margin of 180, and that arithmetic commands 79° nose-up
/// — held for two and a half seconds, by every bot in the match at once,
/// climbing to 400 units and staying there. It reads as a rocket launch, which
/// is what it is.
///
/// One bot and one distant enemy at the same altitude, so the aim vector is
/// horizontal and the only thing steering vertically is the terrain rule. A
/// dogfight adds its own climbing — bots chase each other upward, which is
/// pursuit working correctly — so it is deliberately not in this fixture.
#[test]
fn a_bot_climbs_off_its_pad_rather_than_launching() {
    let rules = Rules::DEFAULT;
    let mut w = World::new(3, rules, Mode::Skirmish, MapKind::Terrain);
    let pad = Vec3::new(0.0, rules.spawn.terrain_y, -rules.spawn.terrain_z);
    let mut b = Ship::spawn(200, ShipKind::Bot, pad, Quat::IDENTITY, &rules);
    b.team = Some(Team::Zero);
    w.ships.push(b);
    let mut e = Ship::spawn(
        201,
        ShipKind::Bot,
        pad + Vec3::new(0.0, 0.0, 1_800.0),
        Quat::IDENTITY,
        &rules,
    );
    e.team = Some(Team::One);
    w.ships.push(e);

    // It starts below the margin, or the fixture proves nothing.
    let start = pad.y - spaceships_sim::ship::terrain_height(pad.x, pad.z, &rules);
    assert!(
        start < rules.bot.terrain_margin,
        "fixture starts already clear"
    );

    let mut steepest = 0.0f64;
    for _ in 0..(5 * 60) {
        tick(&mut w, &[], &[], TICK_DT);
        let s = &w.ships[0];
        steepest = steepest.max(s.vel.y / s.vel.length().max(1e-9));
    }
    // sin(climb angle). 0.6 is 37°: a steep departure is fine, vertical is not.
    assert!(
        steepest < 0.6,
        "the bot left the pad at {steepest:+.2} of vertical",
    );
}
