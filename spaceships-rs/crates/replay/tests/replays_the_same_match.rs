//! The claim this crate makes, tested end to end.
//!
//! A recording is a starting state plus what the tick was fed. If that is true,
//! then replaying it must produce the *same bits* as the match that was
//! recorded — not a similar match, not one that diverges after a minute, the
//! same one. Every test here asserts on [`state_bits`], which is the comparison
//! `crates/sim/tests/tick_integration.rs` uses for exactly this property and
//! for the same reason: `PartialEq` on `World` compares floats by value, so
//! `0.0 == -0.0` and a divergence into a sign-flipped zero would pass.
//!
//! The multiplayer test is the load-bearing one. Solo replays would work
//! without the `NetEvent` log; a networked match's hit points, deaths, respawns
//! and match clock **all** arrive as `NetEvent`s, so a re-simulation without
//! them is a different match — and
//! `a_multiplayer_replay_is_wrong_without_its_net_events` is what stops anyone
//! deciding the log is dead weight.

use spaceships_replay::{Playback, Recorder, Recording};
use spaceships_sim::math::Vec3;
use spaceships_sim::rules::Rules;
use spaceships_sim::world::{
    Authority, Input, MapKind, Mode, NetEvent, Quat, Ship, ShipKind, Team, World, TICK_DT,
};
use spaceships_sim::{asteroids, bot, tick::tick};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A busy solo match: a player, three bots, and a full asteroid field.
///
/// The same shape as `tick_integration.rs`'s fixture, so every random stream is
/// exercised — the field at construction, spawn jitter on respawn, combat rolls
/// on rock collisions, bot decisions every tick, and flare directions.
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
///
/// Integer arithmetic throughout. `sin` would do as well here — this is a test
/// fixture, not a simulation path — but the crate it exercises bans
/// transcendentals for a reason, and a fixture that reaches for one anyway
/// invites the next person to.
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
/// Lifted from `crates/sim/tests/tick_integration.rs`. Duplicated rather than
/// shared because it is a *test* assertion — `sim` does not export it, and
/// exporting it would put a debug helper in the crate whose dependency list is
/// deliberately empty.
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

/// Plays a solo match for `steps` ticks, recording it, and hands back the world
/// that was actually flown along with the recording of it.
fn fly_and_record(seed: u64, steps: u64) -> (World, Recording) {
    let mut world = scripted_match(seed);
    let mut rec = Recorder::start(&world, "Skirmish on Space");
    for step in 0..steps {
        let input = scripted_input(step);
        // Before the tick, with the slices the tick is about to be given.
        rec.push(&[input], &[]);
        tick(&mut world, &[input], &[], TICK_DT);
    }
    (world, rec.finish())
}

/// Runs a recording to the end.
fn play_out(recording: Recording) -> World {
    let mut play = Playback::open(recording);
    while play.step().is_some() {}
    play.world().clone()
}

// ---------------------------------------------------------------------------
// The claim
// ---------------------------------------------------------------------------

/// Fifteen seconds of a busy skirmish, recorded and played back.
#[test]
fn a_recorded_match_replays_bit_identically() {
    const STEPS: u64 = 900;
    let (flown, recording) = fly_and_record(0x5EED, STEPS);
    assert_eq!(recording.len() as u64, STEPS);

    let replayed = play_out(recording);

    assert_eq!(
        state_bits(&flown),
        state_bits(&replayed),
        "a replay must reproduce the match bit for bit, not approximately",
    );
    assert!(
        flown == replayed,
        "and the whole world must compare equal, not just the floats",
    );
    assert_eq!(replayed.tick, STEPS);
}

/// The same claim, but through the bytes — so the file format is in the loop
/// rather than the in-memory structure.
#[test]
fn the_claim_survives_a_round_trip_through_the_file() {
    let (flown, recording) = fly_and_record(0xC0FFEE, 600);

    let bytes = recording.encode();
    let reloaded = Recording::decode(&bytes).expect("a file this build wrote, it can read");

    let replayed = play_out(reloaded);
    assert_eq!(state_bits(&flown), state_bits(&replayed));
    assert!(flown == replayed);
}

/// A different seed must produce a different match, or the test above would
/// pass on a replay system that ignores everything it is given.
#[test]
fn a_different_seed_is_a_different_match() {
    let (a, _) = fly_and_record(0x5EED, 400);
    let (b, _) = fly_and_record(0x5EEE, 400);
    assert_ne!(state_bits(&a), state_bits(&b));
}

/// Seeking is a shortcut, not a different simulation: landing on a tick by
/// keyframe must give the same bits as walking there.
#[test]
fn seeking_arrives_at_the_same_bits_as_walking() {
    let (flown, recording) = fly_and_record(0xB0A7, 1_500);

    let mut play = Playback::open_with_keyframes(recording, 250);
    let cost = play.seek(1_500);
    assert!(
        cost.ticks <= 250,
        "a seek to the end re-simulated {} ticks; the keyframes are not being used",
        cost.ticks,
    );
    assert_eq!(state_bits(&flown), state_bits(play.world()));

    // And scrubbing backwards is not a one-way trip.
    play.seek(300);
    play.seek(1_500);
    assert_eq!(state_bits(&flown), state_bits(play.world()));
}

// ---------------------------------------------------------------------------
// Multiplayer
// ---------------------------------------------------------------------------

/// A networked match: `Authority::Server`, so this client resolves nothing on
/// its own and every authoritative fact arrives as a [`NetEvent`].
fn networked_match() -> World {
    let mut world = World::new(0x0F17, Rules::DEFAULT, Mode::Multiplayer, MapKind::Space);
    asteroids::populate(&mut world);
    assert_eq!(world.authority, Authority::Server);
    let rules = world.rules;

    let mut me = Ship::spawn(
        1,
        ShipKind::Local,
        Vec3::new(0.0, 0.0, -540.0),
        Quat::IDENTITY,
        &rules,
    );
    me.team = Some(Team::Zero);
    world.ships.push(me);
    world.local_id = Some(1);

    let mut them = Ship::spawn(
        2,
        ShipKind::Remote,
        Vec3::new(0.0, 0.0, 540.0),
        Quat::FLIP_Y,
        &rules,
    );
    them.team = Some(Team::One);
    world.ships.push(them);
    world
}

/// What the server sends on tick `step`, if anything.
///
/// Poses at 20 Hz, the match clock at 1 Hz, and a hit, a death and a respawn on
/// the way through — the whole of what a client is told in a real match.
fn server_traffic(step: u64) -> Vec<NetEvent> {
    let mut out = Vec::new();
    if step % 3 == 0 {
        // A remote pilot flying a slow arc toward the middle.
        let t = step as f64 * TICK_DT;
        out.push(NetEvent::RemoteState {
            id: 2,
            pos: Vec3::new(t * 6.0, 0.0, 540.0 - t * 40.0),
            quat: Quat::FLIP_Y,
            boost: step % 120 < 40,
        });
    }
    if step % 60 == 0 {
        out.push(NetEvent::MatchState {
            timer: 300.0 - step as f64 * TICK_DT,
            team_kills: [u32::try_from(step / 600).unwrap_or(0), 0],
        });
    }
    match step {
        200 => out.push(NetEvent::Hp { id: 1, hp: 60 }),
        260 => out.push(NetEvent::Hp { id: 1, hp: 10 }),
        300 => out.push(NetEvent::Death {
            id: 1,
            killer: Some(2),
        }),
        420 => out.push(NetEvent::Respawn {
            id: 1,
            pos: Vec3::new(20.0, 0.0, -540.0),
            quat: Quat::IDENTITY,
        }),
        500 => out.push(NetEvent::AsteroidHp { id: 3, hp: 2 }),
        520 => out.push(NetEvent::AsteroidDestroyed { id: 3 }),
        _ => {}
    }
    out
}

fn fly_and_record_networked(steps: u64) -> (World, Recording) {
    let mut world = networked_match();
    let mut rec = Recorder::start(&world, "Multiplayer on Space");
    for step in 0..steps {
        let input = scripted_input(step);
        let events = server_traffic(step);
        rec.push(&[input], &events);
        tick(&mut world, &[input], &events, TICK_DT);
    }
    (world, rec.finish())
}

/// A networked match replays exactly, because the authority it ran under was
/// recorded along with the stick.
#[test]
fn a_networked_match_replays_bit_identically() {
    let (flown, recording) = fly_and_record_networked(900);

    // The traffic really is in the file, and it really is what a server sends.
    let events: usize = recording.steps.iter().map(|s| s.events.len()).sum();
    assert!(events > 300, "only {events} events recorded");

    let bytes = recording.encode();
    let replayed = play_out(Recording::decode(&bytes).expect("decodes"));

    assert_eq!(state_bits(&flown), state_bits(&replayed));
    assert!(flown == replayed);
    // The client that recorded this was dead at tick 300 and respawned at 420,
    // neither of which it decided for itself.
    assert!(replayed.ship(1).expect("the pilot").alive);
}

/// **The reason the `NetEvent` log exists.**
///
/// Drop the server's traffic and re-simulate the same inputs: the match still
/// runs, the ship still flies, and none of it is what happened. Under
/// `Authority::Server` this client never resolves its own hit points, never
/// respawns itself, and never counts the match clock down, so without the log
/// there is no death, no respawn, and nobody else on the map.
#[test]
fn a_multiplayer_replay_is_wrong_without_its_net_events() {
    let (flown, recording) = fly_and_record_networked(900);

    let mut stripped = recording.clone();
    for step in &mut stripped.steps {
        step.events.clear();
    }

    let with = play_out(recording);
    let without = play_out(stripped);

    assert_eq!(state_bits(&flown), state_bits(&with));
    assert_ne!(
        state_bits(&flown),
        state_bits(&without),
        "a multiplayer replay without its net events must not accidentally agree",
    );
    assert_eq!(
        without.ship(1).expect("the pilot").hp,
        Rules::DEFAULT.ship.max_hp,
        "with no server telling it otherwise, the ship is never hurt at all",
    );
}
