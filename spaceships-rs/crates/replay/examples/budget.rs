//! What a five-minute match actually costs to record, and to scrub.
//!
//! ```text
//! cargo run --release -p spaceships-replay --example budget
//! ```
//!
//! Two questions this answers, both of which decide whether the clip editor is
//! buildable:
//!
//! 1. **How big is a recording?** `BACKLOG.md` says tens of kilobytes. That
//!    holds for a pilot on the keyboard and not for one on the mouse, because a
//!    mouse aiming axis is a continuous `f64` that moves every tick and cannot
//!    be quantised without changing the match. This prints both.
//! 2. **How long is a seek?** Scrubbing has to feel instant, and the keyframe
//!    interval is the knob. This sweeps it.
//!
//! Run it in `--release`. In a debug build the simulation is roughly twenty
//! times slower and every number below is a measure of `rustc -O0`.

use std::time::Instant;

use spaceships_replay::{Playback, Recorder, Recording};
use spaceships_sim::math::{Quat, Vec3};
use spaceships_sim::rules::Rules;
use spaceships_sim::world::{Input, MapKind, Mode, Ship, ShipKind, Team, World, TICK_DT, TICK_HZ};
use spaceships_sim::{asteroids, bot, tick::tick};

/// Five minutes at 60 Hz — a full multiplayer match.
const STEPS: u64 = 300 * 60;

fn main() {
    let mouse = record(STEPS, mouse_pilot);
    let keyboard = record(STEPS, keyboard_pilot);

    println!("a {}-second match, {STEPS} ticks", STEPS as f64 / TICK_HZ);
    report("mouse pilot   ", &mouse);
    report("keyboard pilot", &keyboard);

    println!();
    for interval in [300, 600, 1200] {
        sweep(&mouse, interval);
    }
}

fn report(who: &str, recording: &Recording) {
    let bytes = recording.encode().len();
    let world = {
        let mut e = spaceships_replay::wire::Enc::new();
        spaceships_replay::wire::Wire::put(&recording.world, &mut e);
        e.len()
    };
    println!(
        "  {who}: {:>7} B total = {:>5} B world + {:>7} B log ({:.1} B/tick)",
        bytes,
        world,
        bytes - world,
        (bytes - world) as f64 / recording.len() as f64,
    );
}

/// Builds the keyframe index at `interval` and times a scrub across the match.
fn sweep(recording: &Recording, interval: usize) {
    let built = Instant::now();
    let mut play = Playback::open_with_keyframes(recording.clone(), interval);
    let build_ms = built.elapsed().as_secs_f64() * 1000.0;

    // A scrub: thirty seeks scattered across the timeline, the way a hand on a
    // scrubber lands rather than the way a loop would.
    let mut worst = 0.0f64;
    let mut total = 0.0f64;
    let mut worst_ticks = 0;
    for i in 0..30u64 {
        let target = ((i.wrapping_mul(7_919) % STEPS) as usize).min(play.len());
        let at = Instant::now();
        let cost = play.seek(target);
        let ms = at.elapsed().as_secs_f64() * 1000.0;
        total += ms;
        if ms > worst {
            worst = ms;
            worst_ticks = cost.ticks;
        }
    }

    println!(
        "  keyframes every {interval:>4} ticks ({:>4.1} s): {:>2} held, {build_ms:>6.1} ms to build, \
         seek {:.2} ms avg / {worst:.2} ms worst ({worst_ticks} ticks)",
        interval as f64 / TICK_HZ,
        play.keyframe_count(),
        total / 30.0,
    );
}

// ---------------------------------------------------------------------------
// Two pilots
// ---------------------------------------------------------------------------

/// A mouse pilot: both aiming axes move every single tick.
fn mouse_pilot(step: u64) -> Input {
    Input {
        id: 1,
        steer_x: (step % 37) as f64 / 37.0 - 0.5,
        steer_y: (step % 23) as f64 / 23.0 - 0.5,
        throttle_axis: 1.0,
        fire: step % 3 == 0,
        boost: step % 11 < 4,
        ..Default::default()
    }
}

/// A keyboard pilot: discrete axes that hold for a second at a time.
fn keyboard_pilot(step: u64) -> Input {
    Input {
        id: 1,
        arrow_x: f64::from(i32::from(step % 180 < 60) - i32::from(step % 180 >= 120)),
        arrow_y: f64::from(i32::from(step % 240 < 90)),
        throttle_axis: 1.0,
        fire: step % 120 < 40,
        boost: step % 600 < 200,
        ..Default::default()
    }
}

fn record(steps: u64, pilot: fn(u64) -> Input) -> Recording {
    let mut world = skirmish();
    let mut rec = Recorder::start(&world, "budget");
    let at = Instant::now();
    for step in 0..steps {
        let input = pilot(step);
        rec.push(&[input], &[]);
        tick(&mut world, &[input], &[], TICK_DT);
    }
    let ms = at.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "  (simulated {steps} ticks in {ms:.0} ms — {:.1} us a tick)",
        ms * 1000.0 / steps as f64
    );
    rec.finish()
}

fn skirmish() -> World {
    let mut world = World::new(0x5EED, Rules::DEFAULT, Mode::Skirmish, MapKind::Space);
    asteroids::populate(&mut world);
    let rules = world.rules;

    let mut me = Ship::spawn(
        1,
        ShipKind::Local,
        Vec3::new(0.0, 0.0, -420.0),
        Quat::IDENTITY,
        &rules,
    );
    me.team = Some(Team::Zero);
    world.ships.push(me);
    world.local_id = Some(1);

    // A 5v5, which is what `Mode::Skirmish` means in the lobby.
    for i in 0..9 {
        let team = if i < 4 { Team::Zero } else { Team::One };
        let mut b = Ship::spawn(
            10 + i,
            ShipKind::Bot,
            Vec3::new(f64::from(i) * 30.0 - 120.0, 0.0, f64::from(i) * 40.0),
            Quat::IDENTITY,
            &rules,
        );
        b.team = Some(team);
        bot::init(&mut b, false, false, &rules, &mut world.rng.bots);
        world.ships.push(b);
    }
    world
}
