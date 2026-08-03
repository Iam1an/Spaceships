//! The log: one record per tick, holding what the tick was fed.
//!
//! # Why the log is the recording
//!
//! `tick(&mut World, &[Input], &[NetEvent], dt)` takes everything that can
//! change the outcome as two explicit slices. Nothing else reaches the
//! simulation — no clock, no entropy, no file. So a world plus the two slices
//! it was handed on every tick since is a complete description of a match, and
//! the only thing recording costs is copying those slices.
//!
//! # Ticks are implicit
//!
//! A [`Step`] carries no tick number. The recorder is handed one step per tick
//! from the moment it starts, contiguously, so step `k` is tick
//! `first_tick + k` and a four-byte field per tick would say nothing eighteen
//! thousand times. [`crate::Recording::first_tick`] holds the one number that
//! is not implied.
//!
//! # Inputs are delta-coded, and that is where the size goes
//!
//! An `Input` is nineteen fields, most of them `f64`, so writing it whole costs
//! about 150 bytes a tick — 2.7 MB across a five-minute match. But a pilot
//! changes two of those fields per tick (the two mouse axes) and holds the rest
//! for seconds at a time, so each input goes over as a **field mask plus the
//! fields that moved**:
//!
//! | Situation | Bytes per tick |
//! |---|---|
//! | Nothing changed at all (a hand off the stick) | 1 |
//! | Keyboard flying, one axis moving | 13 |
//! | Mouse flying, both axes moving | 26 |
//! | Every field moving at once | 158 |
//!
//! Which puts a five-minute mouse dogfight around 400–500 kB, and a keyboard
//! one an order of magnitude below that. `BACKLOG.md`'s "tens of kilobytes"
//! assumed discrete inputs; a continuous `f64` aiming axis is what costs, and
//! it cannot be quantised away — the recorded value has to be the *exact*
//! value the live tick saw, or the replay diverges.
//!
//! # Changed means bit-changed
//!
//! Every comparison here is on the bit pattern, not on `==`. `0.0 == -0.0` is
//! true and their bits are not, and a simulation that is bit-deterministic by
//! contract is not a place to start rounding two values together on the grounds
//! that they usually behave alike.

use spaceships_sim::math::{Quat, Vec3};
use spaceships_sim::world::{EntityId, Input, NetEvent, Team, WeaponKind};

use crate::wire::{Dec, Enc, Error, Result, Wire};
use crate::wire_enum;

wire_enum!(WeaponKind, "weapon" { 0 => Bullet, 1 => Beam, 2 => Missile });

/// One tick's worth of what the simulation was fed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Step {
    /// The `inputs` slice, verbatim.
    pub inputs: Vec<Input>,
    /// The `events` slice, verbatim. Empty in every solo mode; in multiplayer
    /// it is the whole of the server's authority for that tick, and a replay
    /// without it is a different match. See [`crate::Recording`].
    pub events: Vec<NetEvent>,
}

// ---------------------------------------------------------------------------
// Bitwise comparison
// ---------------------------------------------------------------------------

/// "Is this the same value" for a field of an [`Input`], compared on bits.
trait Same {
    fn same(&self, other: &Self) -> bool;
}

impl Same for f64 {
    fn same(&self, other: &f64) -> bool {
        self.to_bits() == other.to_bits()
    }
}

impl Same for bool {
    fn same(&self, other: &bool) -> bool {
        self == other
    }
}

impl Same for i32 {
    fn same(&self, other: &i32) -> bool {
        self == other
    }
}

impl<T: Same> Same for Option<T> {
    fn same(&self, other: &Option<T>) -> bool {
        match (self, other) {
            (None, None) => true,
            (Some(a), Some(b)) => a.same(b),
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// The input delta
// ---------------------------------------------------------------------------

/// Generates the three halves of the delta codec from one list of fields, so
/// the mask bit a field is written under and the one it is read under cannot
/// drift apart.
///
/// The bit numbers are written down rather than derived from position for the
/// same reason [`crate::wire_enum`]'s tags are: adding a field must not change
/// what an existing recording decodes to.
macro_rules! input_delta {
    ($($bit:literal => $f:ident),* $(,)?) => {
        /// Which of `next`'s fields differ from `prev`'s.
        fn diff_mask(prev: &Input, next: &Input) -> u32 {
            let mut mask = 0u32;
            $( if !prev.$f.same(&next.$f) { mask |= 1 << $bit; } )*
            mask
        }

        /// Writes the fields `mask` names, in field order.
        fn put_changed(e: &mut Enc, mask: u32, v: &Input) {
            $( if mask & (1 << $bit) != 0 { Wire::put(&v.$f, e); } )*
        }

        /// Reads them back onto a baseline, in the same order.
        fn get_changed(d: &mut Dec<'_>, mask: u32, onto: &mut Input) -> Result<()> {
            $( if mask & (1 << $bit) != 0 { onto.$f = Wire::get(d)?; } )*
            Ok(())
        }

        /// The mask bits this version defines. A bit outside it is corruption,
        /// and reading past it would consume bytes belonging to the next step.
        const KNOWN_FIELDS: u32 = 0 $( | (1 << $bit) )*;
    };
}

// `id` is not in the mask: it is written on every input, because it is what
// selects which baseline the rest of the fields apply to.
input_delta! {
     0 => steer_x,
     1 => steer_y,
     2 => roll,
     3 => arrow_x,
     4 => arrow_y,
     5 => arrow_fine,
     6 => throttle_notches,
     7 => throttle_axis,
     8 => throttle_override,
     9 => fire,
    10 => braking,
    11 => boost,
    12 => hard_brake,
    13 => free_look,
    14 => fire_missile,
    15 => deploy_flare,
    16 => toggle_gun,
    17 => toggle_aim_assist,
}

/// The per-id baselines both sides of the codec keep, so a field that has not
/// moved costs nothing.
///
/// A `Vec` and a linear scan: a match has at most a dozen ships, and a `HashMap`
/// would be slower at that size as well as being the kind of thing that gets
/// iterated by accident.
#[derive(Debug, Default)]
struct Baselines {
    rows: Vec<Input>,
}

impl Baselines {
    /// The last input seen for `id`, or a neutral one carrying that id.
    fn get(&self, id: EntityId) -> Input {
        self.rows
            .iter()
            .find(|i| i.id == id)
            .copied()
            .unwrap_or(Input {
                id,
                ..Input::default()
            })
    }

    fn set(&mut self, v: Input) {
        match self.rows.iter_mut().find(|i| i.id == v.id) {
            Some(row) => *row = v,
            None => self.rows.push(v),
        }
    }
}

// ---------------------------------------------------------------------------
// Net events
// ---------------------------------------------------------------------------

impl Wire for NetEvent {
    fn put(&self, e: &mut Enc) {
        match *self {
            NetEvent::RemoteState {
                id,
                pos,
                quat,
                boost,
            } => {
                0u8.put(e);
                id.put(e);
                pos.put(e);
                quat.put(e);
                boost.put(e);
            }
            NetEvent::Hp { id, hp } => {
                1u8.put(e);
                id.put(e);
                hp.put(e);
            }
            NetEvent::Death { id, killer } => {
                2u8.put(e);
                id.put(e);
                killer.put(e);
            }
            NetEvent::Respawn { id, pos, quat } => {
                3u8.put(e);
                id.put(e);
                pos.put(e);
                quat.put(e);
            }
            NetEvent::Fired {
                id,
                weapon,
                origin,
                dir,
                target,
            } => {
                4u8.put(e);
                id.put(e);
                weapon.put(e);
                origin.put(e);
                dir.put(e);
                target.put(e);
            }
            NetEvent::FlareBurst { id, pos, quat } => {
                5u8.put(e);
                id.put(e);
                pos.put(e);
                quat.put(e);
            }
            NetEvent::PlayerRow {
                id,
                team,
                kills,
                deaths,
            } => {
                6u8.put(e);
                id.put(e);
                team.put(e);
                kills.put(e);
                deaths.put(e);
            }
            NetEvent::MatchState { timer, team_kills } => {
                7u8.put(e);
                timer.put(e);
                team_kills.put(e);
            }
            NetEvent::MatchEnd { winner, team_kills } => {
                8u8.put(e);
                winner.put(e);
                team_kills.put(e);
            }
            NetEvent::AsteroidHp { id, hp } => {
                9u8.put(e);
                id.put(e);
                hp.put(e);
            }
            NetEvent::AsteroidDestroyed { id } => {
                10u8.put(e);
                id.put(e);
            }
            NetEvent::Disconnect { id } => {
                11u8.put(e);
                id.put(e);
            }
        }
    }

    fn get(d: &mut Dec<'_>) -> Result<NetEvent> {
        Ok(match u8::get(d)? {
            0 => NetEvent::RemoteState {
                id: EntityId::get(d)?,
                pos: Vec3::get(d)?,
                quat: Quat::get(d)?,
                boost: bool::get(d)?,
            },
            1 => NetEvent::Hp {
                id: EntityId::get(d)?,
                hp: i32::get(d)?,
            },
            2 => NetEvent::Death {
                id: EntityId::get(d)?,
                killer: Option::<EntityId>::get(d)?,
            },
            3 => NetEvent::Respawn {
                id: EntityId::get(d)?,
                pos: Vec3::get(d)?,
                quat: Quat::get(d)?,
            },
            4 => NetEvent::Fired {
                id: EntityId::get(d)?,
                weapon: WeaponKind::get(d)?,
                origin: Vec3::get(d)?,
                dir: Vec3::get(d)?,
                target: Option::<EntityId>::get(d)?,
            },
            5 => NetEvent::FlareBurst {
                id: EntityId::get(d)?,
                pos: Vec3::get(d)?,
                quat: Quat::get(d)?,
            },
            6 => NetEvent::PlayerRow {
                id: EntityId::get(d)?,
                team: Option::<Team>::get(d)?,
                kills: u32::get(d)?,
                deaths: u32::get(d)?,
            },
            7 => NetEvent::MatchState {
                timer: f64::get(d)?,
                team_kills: <[u32; 2]>::get(d)?,
            },
            8 => NetEvent::MatchEnd {
                winner: Option::<Team>::get(d)?,
                team_kills: <[u32; 2]>::get(d)?,
            },
            9 => NetEvent::AsteroidHp {
                id: u32::get(d)?,
                hp: i32::get(d)?,
            },
            10 => NetEvent::AsteroidDestroyed { id: u32::get(d)? },
            11 => NetEvent::Disconnect {
                id: EntityId::get(d)?,
            },
            tag => {
                return Err(Error::BadTag {
                    tag,
                    what: "net event",
                })
            }
        })
    }
}

// ---------------------------------------------------------------------------
// The step stream
// ---------------------------------------------------------------------------

/// Inputs are byte-identical to the previous step; no input block follows.
const HEAD_SAME_INPUTS: u8 = 0x01;
/// A net-event block follows.
const HEAD_HAS_EVENTS: u8 = 0x02;
/// The input count lives in the top six bits, so a step carries at most 63
/// inputs — four times the largest lobby the game has ever had.
const HEAD_COUNT_SHIFT: u32 = 2;
/// The largest input count a step can express.
pub const MAX_INPUTS_PER_STEP: usize = 63;

/// Whether two input slices are the same, field by field, on bits.
fn steps_match(a: &[Input], b: &[Input]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.id == y.id && diff_mask(x, y) == 0)
}

/// Writes every step, delta-coded against the one before.
pub(crate) fn put_steps(e: &mut Enc, steps: &[Step]) {
    let n = u32::try_from(steps.len()).expect("a match longer than u32::MAX ticks");
    n.put(e);

    let mut baselines = Baselines::default();
    let mut previous: &[Input] = &[];

    for step in steps {
        let same = steps_match(previous, &step.inputs);
        let count = step.inputs.len().min(MAX_INPUTS_PER_STEP);
        let mut head = 0u8;
        if same {
            head |= HEAD_SAME_INPUTS;
        } else {
            head |= (count as u8) << HEAD_COUNT_SHIFT;
        }
        if !step.events.is_empty() {
            head |= HEAD_HAS_EVENTS;
        }
        head.put(e);

        if !same {
            for input in step.inputs.iter().take(count) {
                let base = baselines.get(input.id);
                let mask = diff_mask(&base, input);
                input.id.put(e);
                mask.put(e);
                put_changed(e, mask, input);
                baselines.set(*input);
            }
        }
        if !step.events.is_empty() {
            step.events.put(e);
        }
        previous = &step.inputs;
    }
}

/// Reads them back.
pub(crate) fn get_steps(d: &mut Dec<'_>) -> Result<Vec<Step>> {
    // Every step is at least its one head byte, which is what bounds the claim.
    let n = d.count(1)?;
    let mut steps: Vec<Step> = Vec::with_capacity(n);
    let mut baselines = Baselines::default();
    let mut previous: Vec<Input> = Vec::new();

    for _ in 0..n {
        let head = u8::get(d)?;
        let inputs = if head & HEAD_SAME_INPUTS != 0 {
            previous.clone()
        } else {
            let count = usize::from(head >> HEAD_COUNT_SHIFT);
            let mut inputs = Vec::with_capacity(count);
            for _ in 0..count {
                let id = EntityId::get(d)?;
                let mask = u32::get(d)?;
                if mask & !KNOWN_FIELDS != 0 {
                    // Reading past a bit this version does not know would take
                    // bytes belonging to the next step and turn one corrupt
                    // field into a corrupt file.
                    return Err(Error::BadTag {
                        tag: 0,
                        what: "input field mask",
                    });
                }
                let mut input = baselines.get(id);
                get_changed(d, mask, &mut input)?;
                baselines.set(input);
                inputs.push(input);
            }
            inputs
        };
        let events = if head & HEAD_HAS_EVENTS != 0 {
            Vec::<NetEvent>::get(d)?
        } else {
            Vec::new()
        };
        previous.clone_from(&inputs);
        steps.push(Step { inputs, events });
    }
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(steps: &[Step]) -> Vec<Step> {
        let mut e = Enc::new();
        put_steps(&mut e, steps);
        let bytes = e.finish();
        let mut d = Dec::new(&bytes);
        let back = get_steps(&mut d).expect("decodes");
        assert!(d.is_empty(), "the log must consume the whole buffer");
        back
    }

    fn flying(t: f64) -> Input {
        Input {
            id: 1,
            steer_x: (t * 0.37).rem_euclid(2.0) - 1.0,
            steer_y: (t * 0.11).rem_euclid(2.0) - 1.0,
            throttle_axis: 1.0,
            fire: t as u64 % 7 < 3,
            ..Default::default()
        }
    }

    #[test]
    fn a_flown_log_round_trips_exactly() {
        let steps: Vec<Step> = (0..600)
            .map(|i| Step {
                inputs: vec![flying(f64::from(i))],
                events: Vec::new(),
            })
            .collect();
        assert_eq!(round_trip(&steps), steps);
    }

    /// The delta is the whole reason the format is affordable, so the saving is
    /// asserted rather than assumed.
    #[test]
    fn holding_still_costs_a_byte_a_tick() {
        let held = Input {
            id: 1,
            throttle_axis: 1.0,
            ..Default::default()
        };
        let steps: Vec<Step> = (0..1000)
            .map(|_| Step {
                inputs: vec![held],
                events: Vec::new(),
            })
            .collect();
        let mut e = Enc::new();
        put_steps(&mut e, &steps);
        // Four bytes of count, then the first step in full and 999 repeats.
        assert!(
            e.len() < 1050,
            "a held stick should cost about a byte a tick, not {}",
            e.len()
        );
        assert_eq!(round_trip(&steps), steps);
    }

    /// A pilot on the mouse changes two `f64` per tick and nothing else.
    #[test]
    fn mouse_flying_costs_about_twenty_six_bytes_a_tick() {
        let steps: Vec<Step> = (0..1000)
            .map(|i| Step {
                inputs: vec![Input {
                    id: 1,
                    steer_x: f64::from(i) * 0.001,
                    steer_y: f64::from(i) * 0.002,
                    ..Default::default()
                }],
                events: Vec::new(),
            })
            .collect();
        let mut e = Enc::new();
        put_steps(&mut e, &steps);
        let per_tick = e.len() / 1000;
        assert!(
            (24..=28).contains(&per_tick),
            "expected ~26 bytes a tick, got {per_tick}",
        );
    }

    /// Two ships in one step, each with its own baseline.
    #[test]
    fn several_players_keep_separate_baselines() {
        let steps: Vec<Step> = (0..50)
            .map(|i| Step {
                inputs: vec![
                    Input {
                        id: 1,
                        steer_x: f64::from(i),
                        ..Default::default()
                    },
                    Input {
                        id: 7,
                        steer_y: f64::from(-i),
                        boost: i % 2 == 0,
                        ..Default::default()
                    },
                ],
                events: Vec::new(),
            })
            .collect();
        assert_eq!(round_trip(&steps), steps);
    }

    #[test]
    fn every_net_event_round_trips() {
        let events = vec![
            NetEvent::RemoteState {
                id: 4,
                pos: Vec3::new(1.0, 2.0, 3.0),
                quat: Quat::IDENTITY,
                boost: true,
            },
            NetEvent::Hp { id: 4, hp: 70 },
            NetEvent::Death {
                id: 4,
                killer: Some(1),
            },
            NetEvent::Respawn {
                id: 4,
                pos: Vec3::ZERO,
                quat: Quat::FLIP_Y,
            },
            NetEvent::Fired {
                id: 1,
                weapon: WeaponKind::Missile,
                origin: Vec3::new(0.0, 1.0, 2.0),
                dir: Vec3::new(0.0, 0.0, 1.0),
                target: Some(4),
            },
            NetEvent::FlareBurst {
                id: 4,
                pos: Vec3::ZERO,
                quat: Quat::IDENTITY,
            },
            NetEvent::PlayerRow {
                id: 4,
                team: Some(Team::One),
                kills: 3,
                deaths: 1,
            },
            NetEvent::MatchState {
                timer: 244.5,
                team_kills: [2, 5],
            },
            NetEvent::MatchEnd {
                winner: None,
                team_kills: [5, 5],
            },
            NetEvent::AsteroidHp { id: 12, hp: 4 },
            NetEvent::AsteroidDestroyed { id: 12 },
            NetEvent::Disconnect { id: 4 },
        ];
        let steps = vec![Step {
            inputs: vec![flying(0.0)],
            events,
        }];
        assert_eq!(round_trip(&steps), steps);
    }

    /// `0.0` and `-0.0` are equal and their bits are not, and the recorded
    /// value has to be the one the live tick saw.
    #[test]
    fn negative_zero_is_a_change() {
        let a = Input {
            id: 1,
            steer_x: 0.0,
            ..Default::default()
        };
        let b = Input {
            id: 1,
            steer_x: -0.0,
            ..Default::default()
        };
        assert_ne!(diff_mask(&a, &b), 0, "-0.0 must not be folded into 0.0");
        let steps = vec![
            Step {
                inputs: vec![a],
                events: Vec::new(),
            },
            Step {
                inputs: vec![b],
                events: Vec::new(),
            },
        ];
        let back = round_trip(&steps);
        assert_eq!(back[1].inputs[0].steer_x.to_bits(), (-0.0f64).to_bits());
    }

    #[test]
    fn a_mask_bit_this_version_does_not_know_is_rejected() {
        let mut e = Enc::new();
        1u32.put(&mut e); // one step
        (1u8 << HEAD_COUNT_SHIFT).put(&mut e); // one input, no events
        1i32.put(&mut e); // id
        (1u32 << 31).put(&mut e); // a field that does not exist
        let bytes = e.finish();
        assert!(get_steps(&mut Dec::new(&bytes)).is_err());
    }

    #[test]
    fn an_empty_log_round_trips() {
        assert_eq!(round_trip(&[]), Vec::<Step>::new());
    }
}
