//! Recording and playback for Spaceships.
//!
//! # A replay is not a recording of positions
//!
//! `crates/sim` is deterministic by construction: no I/O, no wall clock, no
//! unseeded randomness, and every transcendental hand-rolled so the last bits
//! agree across glibc, musl, Apple and WASM. Everything that can change the
//! outcome of `tick(&mut World, &[Input], &[NetEvent], dt)` is in its
//! arguments. So a match is fully described by its starting state and the two
//! slices it was handed on every tick since, and re-running those *is* the
//! match — not an approximation of it, the same bits.
//!
//! That is why this crate stores a few hundred kilobytes where a state
//! recording would store hundreds of megabytes, and why a replay stays correct
//! when it is re-rendered at a higher graphics setting, from a different
//! camera, years later.
//!
//! ```no_run
//! use spaceships_replay::{Playback, Recorder, Recording};
//! use spaceships_sim::world::{Input, World, TICK_DT};
//!
//! # fn demo(mut world: World, inputs: Vec<Vec<Input>>) {
//! // Recording: snapshot the world, then hand over whatever the tick was fed.
//! let mut rec = Recorder::start(&world, "Skirmish on Space");
//! for step in &inputs {
//!     rec.push(step, &[]);
//!     spaceships_sim::tick::tick(&mut world, step, &[], TICK_DT);
//! }
//! let bytes = rec.finish().encode();
//!
//! // Playback: decode, re-simulate, and the same match happens again.
//! let mut play = Playback::open(Recording::decode(&bytes).unwrap());
//! while play.step().is_some() {}
//! assert!(play.world() == &world);
//! # }
//! ```
//!
//! # Layout
//!
//! - [`wire`] — the byte layer. Little-endian, fixed width, `f64` as bits.
//! - [`state`] — [`World`] on disk, and the rules fingerprint.
//! - [`log`] — the per-tick log, delta-coded.
//! - [`Recorder`] — the writing end.
//! - [`Timeline`] — the reading end, driving a `World` the caller owns.
//!   [`Playback`] is the same thing with a world of its own.
//!
//! # What is deliberately absent
//!
//! **A camera.** The camera was never simulation state, so every viewing
//! feature — free flight, riding a ship, slow motion, keyframed paths — is a
//! rendering concern over a re-simulated world and belongs in the client. This
//! crate hands over a `World` and a `Frame` and has no opinion about where
//! anybody is looking from.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub mod log;
pub mod play;
pub mod state;
pub mod wire;

use spaceships_sim::world::{Input, NetEvent, World, TICK_DT};

pub use log::Step;
pub use play::{Playback, SeekCost, Timeline};
pub use state::{default_rules_fingerprint, rules_fingerprint};
pub use wire::{Error, Result};

use wire::{Dec, Enc, Wire};

/// The first four bytes of every recording. "SpaceshiPs RePlay".
pub const MAGIC: [u8; 4] = *b"SPRP";

/// The format this build writes, and the only one it reads.
///
/// # Versioned from day one, on purpose
///
/// The alternative is a file with no version in it, and the first time the
/// format changes every recording anyone has made becomes a crash or, worse, a
/// plausible-looking world with the wrong numbers in it. A `u32` costs four
/// bytes once and is the difference between "this replay is from an older
/// build" and an unexplained desync.
pub const FORMAT: u32 = 1;

/// The default extension for a recording on disk.
pub const EXTENSION: &str = "spr";

/// How many steps a [`Recorder`] keeps before it stops.
///
/// An hour at 60 Hz. Recording is always on, so the bound is what stops a
/// client left running overnight from turning a match into a memory leak; at
/// roughly 26 bytes a tick that is about 5 MB, which is a size worth holding
/// and a size worth refusing to exceed.
pub const MAX_STEPS: usize = 60 * 60 * 60;

// ---------------------------------------------------------------------------
// The file
// ---------------------------------------------------------------------------

/// One recorded match: where it started, and everything it was fed since.
#[derive(Debug, Clone, PartialEq)]
pub struct Recording {
    /// Fingerprint of the [`spaceships_sim::rules::Rules`] the match ran under.
    ///
    /// # Why this is checked and not merely stored
    ///
    /// A replay is a re-simulation, and a re-simulation under different rules
    /// is a different match. Change `missile_speed` by one unit and a recording
    /// made yesterday still *plays* — the ship flies, the guns fire — but the
    /// missile that killed you misses, and from there nothing that follows is
    /// what happened. There is no symptom: no error, no glitch, just a match
    /// that is quietly not the one you recorded.
    ///
    /// So [`Recording::decode`] refuses a mismatch by default, and
    /// [`Recording::decode_ignoring_rules`] is the escape hatch for looking at
    /// an old file on the understanding that it will drift.
    ///
    /// The eventual fix is to store the rules themselves, at which point an old
    /// recording plays correctly under the rules it was made with. That is
    /// cheap to *add* and expensive to retrofit onto files that never carried
    /// the field, which is why the field is here now.
    pub rules_hash: u64,
    /// Seconds since the Unix epoch when recording began, or `0` if the
    /// recorder had no clock. Display only — nothing in the simulation reads
    /// it, and it is not used for ordering.
    pub recorded_at: u64,
    /// A human label: the mode and map, for a file browser to show.
    pub label: String,
    /// The fixed timestep the match ran at, in seconds. Recorded rather than
    /// assumed, so a replay cannot be re-simulated at a rate the original never
    /// used.
    pub dt: f64,
    /// [`World::tick`] at the moment recording began.
    pub first_tick: u64,
    /// The complete simulation state at [`Recording::first_tick`].
    pub world: World,
    /// One entry per tick, from `first_tick` onward, contiguously.
    pub steps: Vec<Step>,
}

impl Recording {
    /// How many ticks this recording covers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Whether nothing was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// How long the recording runs, in seconds.
    #[must_use]
    pub fn duration(&self) -> f64 {
        self.steps.len() as f64 * self.dt
    }

    /// Serialises the whole recording.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc::new();
        e.raw(&MAGIC);
        FORMAT.put(&mut e);
        self.rules_hash.put(&mut e);
        self.recorded_at.put(&mut e);
        self.label.put(&mut e);
        self.dt.put(&mut e);
        self.first_tick.put(&mut e);
        self.world.put(&mut e);
        log::put_steps(&mut e, &self.steps);
        e.finish()
    }

    /// Reads a recording, refusing one whose rules do not match this build's.
    ///
    /// See [`Recording::rules_hash`].
    pub fn decode(bytes: &[u8]) -> Result<Recording> {
        let rec = Recording::decode_ignoring_rules(bytes)?;
        let expected = default_rules_fingerprint();
        if rec.rules_hash != expected {
            return Err(Error::RulesChanged {
                found: rec.rules_hash,
                expected,
            });
        }
        Ok(rec)
    }

    /// Reads a recording without checking the rules fingerprint.
    ///
    /// For inspecting a file from an older build, on the understanding that
    /// what plays back is not necessarily what happened.
    pub fn decode_ignoring_rules(bytes: &[u8]) -> Result<Recording> {
        let mut d = Dec::new(bytes);
        if d.take(MAGIC.len())? != MAGIC {
            return Err(Error::NotARecording);
        }
        let format = u32::get(&mut d)?;
        if format != FORMAT {
            return Err(Error::UnknownVersion {
                found: format,
                expected: FORMAT,
            });
        }
        Ok(Recording {
            rules_hash: u64::get(&mut d)?,
            recorded_at: u64::get(&mut d)?,
            label: String::get(&mut d)?,
            dt: f64::get(&mut d)?,
            first_tick: u64::get(&mut d)?,
            world: World::get(&mut d)?,
            steps: log::get_steps(&mut d)?,
        })
    }
}

// ---------------------------------------------------------------------------
// Recording
// ---------------------------------------------------------------------------

/// Captures a match as it is played.
///
/// The contract is one call per tick, with **the same slices the tick is given**
/// and *before* it is given them — see [`Recorder::push`].
#[derive(Debug, Clone)]
pub struct Recorder {
    recording: Recording,
    full: bool,
}

impl Recorder {
    /// Begins recording from `world`'s current state.
    ///
    /// The clone is the expensive part and it happens once: a deathmatch world
    /// is about ten kilobytes, dominated by sixty asteroids.
    #[must_use]
    pub fn start(world: &World, label: impl Into<String>) -> Recorder {
        Recorder {
            recording: Recording {
                rules_hash: rules_fingerprint(&world.rules),
                recorded_at: 0,
                label: label.into(),
                dt: TICK_DT,
                first_tick: world.tick,
                world: world.clone(),
                steps: Vec::new(),
            },
            full: false,
        }
    }

    /// Stamps the recording with a wall-clock time, for a file browser to sort
    /// by.
    ///
    /// Separate from [`Recorder::start`] because this crate has no clock and
    /// must not grow one: `sim`'s ban on wall-clock time is what makes a replay
    /// reproducible, and the honest place for a timestamp is the caller that
    /// already knows what platform it is on.
    pub fn stamp(&mut self, unix_seconds: u64) {
        self.recording.recorded_at = unix_seconds;
    }

    /// Records one tick's arguments.
    ///
    /// **Call this before the tick, not after.** The slices are copied as
    /// given; a caller that ticks first and records afterwards would still
    /// record the right values, but a caller that ticks first and *drains* its
    /// event queue in between would record an empty one, which is the mistake
    /// this note exists to prevent.
    ///
    /// Silently stops at [`MAX_STEPS`]; [`Recorder::is_full`] reports it.
    pub fn push(&mut self, inputs: &[Input], events: &[NetEvent]) {
        if self.recording.steps.len() >= MAX_STEPS {
            self.full = true;
            return;
        }
        self.recording.steps.push(Step {
            inputs: inputs.to_vec(),
            events: events.to_vec(),
        });
    }

    /// How many ticks have been recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.recording.steps.len()
    }

    /// Whether nothing has been recorded yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.recording.steps.is_empty()
    }

    /// Whether recording stopped because it reached [`MAX_STEPS`].
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.full
    }

    /// The recording so far, without ending it.
    #[must_use]
    pub fn recording(&self) -> &Recording {
        &self.recording
    }

    /// Ends recording and hands over the result.
    #[must_use]
    pub fn finish(self) -> Recording {
        self.recording
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spaceships_sim::math::{Quat, Vec3};
    use spaceships_sim::rules::Rules;
    use spaceships_sim::world::{MapKind, Mode, Ship, ShipKind, Team};

    fn world() -> World {
        let mut w = World::new(0x5EED, Rules::DEFAULT, Mode::Skirmish, MapKind::Space);
        spaceships_sim::asteroids::populate(&mut w);
        let mut me = Ship::spawn(1, ShipKind::Local, Vec3::ZERO, Quat::IDENTITY, &w.rules);
        me.team = Some(Team::Zero);
        w.ships.push(me);
        w.local_id = Some(1);
        w
    }

    #[test]
    fn a_recording_round_trips_through_bytes() {
        let mut w = world();
        let mut rec = Recorder::start(&w, "Skirmish on Space");
        rec.stamp(1_700_000_000);
        for i in 0..120 {
            let input = Input {
                id: 1,
                steer_x: f64::from(i) * 0.01,
                fire: i % 3 == 0,
                ..Default::default()
            };
            rec.push(&[input], &[]);
            spaceships_sim::tick::tick(&mut w, &[input], &[], TICK_DT);
        }
        let original = rec.finish();
        let bytes = original.encode();
        let back = Recording::decode(&bytes).expect("decodes");
        assert!(back == original);
        assert_eq!(back.len(), 120);
        assert_eq!(back.label, "Skirmish on Space");
        assert_eq!(back.recorded_at, 1_700_000_000);
    }

    #[test]
    fn a_file_that_is_not_a_recording_is_rejected() {
        assert_eq!(
            Recording::decode(b"not a replay at all"),
            Err(Error::NotARecording)
        );
    }

    #[test]
    fn a_future_format_is_rejected_by_version_not_by_crashing() {
        let mut bytes = Recorder::start(&world(), "x").finish().encode();
        bytes[4] = 99;
        assert!(matches!(
            Recording::decode(&bytes),
            Err(Error::UnknownVersion { found: 99, .. })
        ));
    }

    /// The check that stops an old replay silently becoming a different match.
    #[test]
    fn a_recording_made_under_other_rules_is_refused() {
        let mut rec = Recorder::start(&world(), "x").finish();
        rec.rules_hash ^= 1;
        let bytes = rec.encode();
        assert!(matches!(
            Recording::decode(&bytes),
            Err(Error::RulesChanged { .. })
        ));
        // And the escape hatch opens it anyway.
        assert!(Recording::decode_ignoring_rules(&bytes).is_ok());
    }

    #[test]
    fn recording_stops_rather_than_growing_without_bound() {
        let mut rec = Recorder::start(&world(), "x");
        // Reach into the cap rather than running an hour of ticks.
        rec.recording.steps = vec![Step::default(); MAX_STEPS];
        rec.push(&[], &[]);
        assert!(rec.is_full());
        assert_eq!(rec.len(), MAX_STEPS);
    }
}
