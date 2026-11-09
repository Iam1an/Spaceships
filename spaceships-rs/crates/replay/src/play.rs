//! Playing a recording back, and seeking inside it without starting over.
//!
//! # The problem seeking has
//!
//! A five-minute match is 18,000 ticks. Re-simulating from tick zero to reach
//! tick 17,000 is not "a bit slow" — it is the difference between a scrubbable
//! timeline and an editor nobody can use, and it gets worse the further into
//! the match the interesting part is, which is exactly where it is.
//!
//! # Keyframes
//!
//! [`Timeline::build`] re-simulates the match once at load and keeps a whole
//! `World` every `interval` ticks. Seeking then means restoring the nearest
//! keyframe at or before the target and fast-forwarding the remainder, so the
//! worst seek costs one interval rather than one match — and a seek *forward*
//! by less than that does not even restore, it just steps.
//!
//! The trade is memory, and it is a good one. A deathmatch `World` is about ten
//! kilobytes of state; a keyframe every five seconds over five minutes is
//! sixty-one of them, built in about seventy milliseconds, and the worst seek
//! lands under two. Halving the interval halves the worst seek and doubles the
//! memory, and both numbers are small enough that the interval is a tuning knob
//! rather than a design decision. `cargo run --release -p spaceships-replay
//! --example budget` prints the sweep.
//!
//! # Two entry points, one mechanism
//!
//! [`Timeline`] drives a `World` the **caller** owns, which is what a renderer
//! wants: the client already keeps the world in a resource that half a dozen
//! systems read, and handing playback its own copy would mean cloning one every
//! tick to keep them in step. [`Playback`] owns a world and wraps a `Timeline`,
//! which is what a test or a headless tool wants.
//!
//! # What playback is *not*
//!
//! It is not a second simulation. Every step here is `sim::tick::tick`, called
//! with the recorded slices — the same function the live match ran, in the same
//! order, at the same `dt`. There is no replay-specific path in the simulation,
//! and there must never be one: the moment playback has its own branch, a
//! replay stops being evidence of what happened.

use spaceships_sim::tick::tick;
use spaceships_sim::world::{Frame, World};

use crate::Recording;

/// How many ticks apart keyframes sit by default. Five seconds at 60 Hz, which
/// the budget example measures at a worst-case seek of about 1.3 ms.
pub const DEFAULT_KEYFRAME_INTERVAL: usize = 300;

/// A recording, a seek index over it, and a cursor.
///
/// The world is the caller's. Every method that advances time takes it by
/// `&mut`, and every one of them assumes it is **the world this timeline's
/// cursor describes** — hand a different one over and the result is a plausible
/// match that is not the recorded one. [`Timeline::start_world`] is where a
/// caller gets a correct one from.
#[derive(Debug, Clone)]
pub struct Timeline {
    recording: Recording,
    /// How many steps have been applied. `0` is the recording's own start.
    cursor: usize,
    /// `(cursor, world)` pairs in ascending cursor order; index 0 always
    /// present, so there is always somewhere to rewind to.
    keyframes: Vec<(usize, World)>,
    interval: usize,
}

impl Timeline {
    /// A timeline with no seek index: cheap to open, expensive to scrub.
    ///
    /// The right choice for something that only ever plays forward — a killcam,
    /// a trials ghost, the intro cinematic — and the wrong one for an editor.
    #[must_use]
    pub fn open(recording: Recording) -> Timeline {
        let start = recording.world.clone();
        Timeline {
            recording,
            cursor: 0,
            keyframes: vec![(0, start)],
            interval: 0,
        }
    }

    /// A timeline with a seek index, built by re-simulating the whole recording
    /// once.
    ///
    /// An `interval` of `0` means [`DEFAULT_KEYFRAME_INTERVAL`].
    #[must_use]
    pub fn build(recording: Recording, interval: usize) -> Timeline {
        let mut timeline = Timeline::open(recording);
        timeline.interval = if interval == 0 {
            DEFAULT_KEYFRAME_INTERVAL
        } else {
            interval
        };
        timeline.build_keyframes();
        timeline
    }

    /// A fresh copy of the world at the recording's start.
    #[must_use]
    pub fn start_world(&self) -> World {
        self.recording.world.clone()
    }

    /// The recording being played.
    #[must_use]
    pub fn recording(&self) -> &Recording {
        &self.recording
    }

    /// How many steps have been applied, `0..=`[`Timeline::len`].
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// How many steps the recording holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.recording.steps.len()
    }

    /// Whether the recording holds no steps at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.recording.steps.is_empty()
    }

    /// Whether the cursor has reached the end.
    #[must_use]
    pub fn at_end(&self) -> bool {
        self.cursor >= self.len()
    }

    /// Seconds from the start of the recording to the cursor.
    #[must_use]
    pub fn elapsed(&self) -> f64 {
        self.cursor as f64 * self.recording.dt
    }

    /// Seconds the whole recording runs for.
    #[must_use]
    pub fn duration(&self) -> f64 {
        self.recording.duration()
    }

    /// How far apart the keyframes sit, or `0` when there is only the start.
    #[must_use]
    pub fn keyframe_interval(&self) -> usize {
        self.interval
    }

    /// How many keyframes are held, including the one at the start.
    #[must_use]
    pub fn keyframe_count(&self) -> usize {
        self.keyframes.len()
    }

    /// Applies the next recorded step to `world`, or `None` at the end.
    ///
    /// The returned [`Frame`] is exactly what the live client drew on that
    /// tick, which is what makes the renderer's job identical either way.
    pub fn step(&mut self, world: &mut World) -> Option<Frame> {
        let step = self.recording.steps.get(self.cursor)?;
        let frame = tick(world, &step.inputs, &step.events, self.recording.dt);
        self.cursor += 1;
        Some(frame)
    }

    /// Moves `world` to `target`, clamped to the recording, and reports what it
    /// cost.
    ///
    /// The tick count is worth returning rather than hiding: it is the whole
    /// measure of whether the keyframe interval is doing its job.
    pub fn seek(&mut self, world: &mut World, target: usize) -> SeekCost {
        let target = target.min(self.len());
        if target == self.cursor {
            return SeekCost::default();
        }

        // Stepping forward costs no clone, so it wins unless a keyframe would
        // land nearer than the cursor already is.
        let best = self.best_keyframe(target);
        let from_keyframe = match (target.checked_sub(self.cursor), best) {
            (Some(ahead), Some(i)) => {
                let at = self.keyframes[i].0;
                at > self.cursor && target - at < ahead
            }
            (Some(_), None) => false,
            // Behind the cursor: there is nothing to step, so always restore.
            (None, _) => true,
        };

        if from_keyframe {
            match best {
                Some(i) => {
                    let (at, keyframe) = &self.keyframes[i];
                    self.cursor = *at;
                    world.clone_from(keyframe);
                }
                // Unreachable — index 0 always exists — but rewinding is the
                // safe answer if it ever becomes reachable.
                None => self.rewind(world),
            }
        }

        let mut ticks = 0;
        while self.cursor < target && self.step(world).is_some() {
            ticks += 1;
        }
        SeekCost {
            ticks,
            from_keyframe,
        }
    }

    /// Puts `world` back to the recording's first tick.
    pub fn rewind(&mut self, world: &mut World) {
        self.cursor = 0;
        world.clone_from(&self.recording.world);
    }

    /// Index of the latest keyframe at or before `target`.
    ///
    /// An index rather than a reference, so the caller can read out of
    /// `self.keyframes` while still holding `&mut self`.
    fn best_keyframe(&self, target: usize) -> Option<usize> {
        // Ascending by construction, so a reverse scan finds the last one that
        // fits. Sixty entries; a binary search would be the same number of
        // cache misses and one more place to get an off-by-one.
        self.keyframes.iter().rposition(|(at, _)| *at <= target)
    }

    /// Re-simulates the whole recording once, keeping a world every
    /// `interval` steps.
    fn build_keyframes(&mut self) {
        debug_assert!(self.cursor == 0, "keyframes are built from the start");
        let interval = self.interval.max(1);
        let mut world = self.recording.world.clone();
        let mut keyframes = vec![(0, world.clone())];

        for (i, step) in self.recording.steps.iter().enumerate() {
            tick(&mut world, &step.inputs, &step.events, self.recording.dt);
            let applied = i + 1;
            if applied % interval == 0 {
                keyframes.push((applied, world.clone()));
            }
        }
        self.keyframes = keyframes;
    }
}

/// What a [`Timeline::seek`] cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SeekCost {
    /// Ticks re-simulated.
    pub ticks: usize,
    /// Whether a keyframe was restored first, or the seek simply stepped
    /// forward from where it already was.
    pub from_keyframe: bool,
}

// ---------------------------------------------------------------------------
// The owning form
// ---------------------------------------------------------------------------

/// A [`Timeline`] and the world it drives, for a caller that does not already
/// have one.
#[derive(Debug, Clone)]
pub struct Playback {
    timeline: Timeline,
    world: World,
}

impl Playback {
    /// Opens a recording with no seek index. See [`Timeline::open`].
    #[must_use]
    pub fn open(recording: Recording) -> Playback {
        let timeline = Timeline::open(recording);
        let world = timeline.start_world();
        Playback { timeline, world }
    }

    /// Opens a recording and builds the seek index. See [`Timeline::build`].
    #[must_use]
    pub fn open_with_keyframes(recording: Recording, interval: usize) -> Playback {
        let timeline = Timeline::build(recording, interval);
        let world = timeline.start_world();
        Playback { timeline, world }
    }

    /// The state as of the current cursor.
    #[must_use]
    pub fn world(&self) -> &World {
        &self.world
    }

    /// The timeline underneath, for everything it reports.
    #[must_use]
    pub fn timeline(&self) -> &Timeline {
        &self.timeline
    }

    /// How many steps have been applied.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.timeline.cursor()
    }

    /// How many steps the recording holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.timeline.len()
    }

    /// Whether the recording holds no steps at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.timeline.is_empty()
    }

    /// Whether the cursor has reached the end.
    #[must_use]
    pub fn at_end(&self) -> bool {
        self.timeline.at_end()
    }

    /// How many keyframes are held.
    #[must_use]
    pub fn keyframe_count(&self) -> usize {
        self.timeline.keyframe_count()
    }

    /// Applies the next recorded step.
    pub fn step(&mut self) -> Option<Frame> {
        self.timeline.step(&mut self.world)
    }

    /// Moves to `target`.
    pub fn seek(&mut self, target: usize) -> SeekCost {
        self.timeline.seek(&mut self.world, target)
    }

    /// Back to the beginning.
    pub fn rewind(&mut self) {
        self.timeline.rewind(&mut self.world);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Recorder;
    use spaceships_sim::math::{Quat, Vec3};
    use spaceships_sim::rules::Rules;
    use spaceships_sim::world::{Input, MapKind, Mode, Ship, ShipKind, Team, TICK_DT};

    fn scripted_input(step: usize) -> Input {
        Input {
            id: 1,
            steer_x: (step % 37) as f64 / 37.0 - 0.5,
            steer_y: (step % 23) as f64 / 23.0 - 0.5,
            roll: if step % 90 < 30 { 1.0 } else { 0.0 },
            throttle_axis: 1.0,
            boost: step % 240 < 60,
            fire: step % 20 < 8,
            fire_missile: step % 300 == 137,
            deploy_flare: step % 400 == 211,
            ..Default::default()
        }
    }

    fn duel() -> World {
        let mut w = World::new(0x5EED, Rules::DEFAULT, Mode::Skirmish, MapKind::Space);
        spaceships_sim::asteroids::populate(&mut w);
        let mut me = Ship::spawn(1, ShipKind::Local, Vec3::ZERO, Quat::IDENTITY, &w.rules);
        me.team = Some(Team::Zero);
        w.ships.push(me);
        w.local_id = Some(1);

        let rules = w.rules;
        for i in 0..3 {
            let at = Vec3::new(f64::from(i) * 40.0 - 40.0, 0.0, 200.0);
            let mut bot = Ship::spawn(10 + i, ShipKind::Bot, at, Quat::FLIP_Y, &rules);
            bot.team = Some(Team::One);
            spaceships_sim::bot::init(&mut bot, false, false, &rules, &mut w.rng.bots);
            w.ships.push(bot);
        }
        w
    }

    /// Records a match and returns `(final live world, the recording)`.
    fn record(steps: usize) -> (World, crate::Recording) {
        let mut w = duel();
        let mut rec = Recorder::start(&w, "test");
        for i in 0..steps {
            let input = scripted_input(i);
            rec.push(&[input], &[]);
            spaceships_sim::tick::tick(&mut w, &[input], &[], TICK_DT);
        }
        (w, rec.finish())
    }

    #[test]
    fn playing_a_recording_reproduces_the_match() {
        let (live, recording) = record(900);
        let mut play = Playback::open(recording);
        while play.step().is_some() {}
        assert_eq!(play.cursor(), 900);
        assert!(play.world() == &live);
    }

    #[test]
    fn seeking_lands_on_the_same_world_as_playing_there() {
        let (_, recording) = record(1_500);

        let mut straight = Playback::open(recording.clone());
        for _ in 0..1_100 {
            straight.step();
        }

        let mut seeking = Playback::open_with_keyframes(recording, 200);
        let cost = seeking.seek(1_100);
        assert!(cost.from_keyframe, "a keyframe should have been used");
        assert!(
            cost.ticks < 200,
            "a seek must not re-simulate more than one interval, took {}",
            cost.ticks
        );
        assert!(seeking.world() == straight.world());
    }

    /// Seeking backwards and forwards again must not accumulate anything.
    #[test]
    fn scrubbing_back_and_forth_is_stable() {
        let (_, recording) = record(1_200);
        let mut play = Playback::open_with_keyframes(recording, 300);

        play.seek(1_000);
        let forward = play.world().clone();
        play.seek(100);
        play.seek(1_000);
        assert!(play.world() == &forward, "scrubbing must be idempotent");
    }

    /// A short seek forward should step rather than restore, because stepping
    /// is the cheaper of the two.
    #[test]
    fn a_short_seek_forward_does_not_rewind_to_a_keyframe() {
        let (_, recording) = record(900);
        let mut play = Playback::open_with_keyframes(recording, 300);
        play.seek(610);
        let cost = play.seek(620);
        assert!(!cost.from_keyframe);
        assert_eq!(cost.ticks, 10);
    }

    #[test]
    fn seeking_past_the_end_clamps() {
        let (live, recording) = record(300);
        let mut play = Playback::open_with_keyframes(recording, 100);
        play.seek(usize::MAX);
        assert_eq!(play.cursor(), 300);
        assert!(play.at_end());
        assert!(play.world() == &live);
    }

    #[test]
    fn keyframes_cover_the_recording() {
        let (_, recording) = record(1_000);
        let timeline = Timeline::build(recording, 250);
        // Start, 250, 500, 750, 1000.
        assert_eq!(timeline.keyframe_count(), 5);
    }

    /// The form the renderer uses: the world is the caller's, and the timeline
    /// only ever advances it.
    #[test]
    fn a_timeline_drives_a_world_it_does_not_own() {
        let (live, recording) = record(600);
        let mut timeline = Timeline::build(recording, 150);
        let mut world = timeline.start_world();

        timeline.seek(&mut world, 600);
        assert!(world == live);

        timeline.rewind(&mut world);
        assert_eq!(timeline.cursor(), 0);
        assert_eq!(world.tick, 0);
    }
}
