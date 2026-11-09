//! The EMP: a weapon that takes a pilot's information and leaves them their
//! hands.
//!
//! `BACKLOG.md` §2. Every other trigger in this crate ends in
//! [`crate::ship::apply_damage`]; this one never touches a hit point. What it
//! does instead is set [`Ship::emp_blind`] on everyone inside a sphere, and the
//! four seconds that follow are the whole weapon.
//!
//! # The shape
//!
//! ```text
//!   charge  ──────────────────────────────▶ 1.0     60 s, and not reset by death
//!   fire    ── spend the meter, detonate at your own position
//!   sphere  ── everyone alive within `radius`, minus the pilot who fired
//!   blind   ── 4 s with no instruments, no assist, no lock, no callouts
//! ```
//!
//! There is no projectile, no aim and no travel time. See
//! [`crate::rules::EmpRules`] for why each of those is absent and what the
//! numbers are balanced against.
//!
//! # What goes dark, and where each piece of it lives
//!
//! §2 lists four things and marks the fourth as optional. All four are in, and
//! the reason is that they are one system: the instrument panel, the head-up
//! display and the aiming aids are the *same* avionics, and switching off three
//! quarters of an aircraft's electronics is a harder thing to justify than
//! switching off all of it.
//!
//! | What | Enforced in | How |
//! |---|---|---|
//! | Aim assist — cone, pull, and lead marker | [`crate::aim_assist::update`] | a blind pilot is treated exactly as a dead one: strength to zero, no held target |
//! | Missile lock, and therefore missile launches | [`crate::missiles::acquire_lock`] | returns `None`, and `tick`'s launcher keeps the round on the rail |
//! | The lock *warning* | [`crate::tick`]'s `hud_state` | `missile_lock_warning` is forced false; the receiver is part of the panel |
//! | Cockpit lighting, instruments, radar, annunciators | `client/src/cockpit.rs` | `CockpitPower::emp`, off [`crate::world::HudState::emp_blind`] |
//! | The head-up display — tapes, meters, pips, brackets, boresight | `client/src/hud.rs` | one `powered` input to the model |
//! | The voice warnings | `client/src/audio.rs` | `stop_warnings`, then silence for the duration |
//!
//! And the two §2 is equally explicit must **not** go:
//!
//! - **Flight controls.** Nothing in here reads or writes velocity, throttle,
//!   orientation or any input but [`crate::world::Input::fire_emp`]. *"Taking
//!   away someone's ability to steer is frustrating, not tense."*
//! - **The guns.** A blinded pilot has full ammunition, full rate of fire and
//!   full damage. They have lost the crosshair, not the trigger.
//!
//! # Two open questions in §2, answered
//!
//! **"Aim assist is forced on for keyboard and mobile schemes, so an EMP hurts
//! those players much harder. Either soften the assist loss on those schemes, or
//! lean in — but decide deliberately."** Leaning in. Softening it would invert
//! the weapon: aim assist is the largest single thing an EMP takes, so exempting
//! the pilots who lean on it hardest would leave the pulse hurting mouse players
//! and sparing everyone else. The asymmetry is real and it is bounded by
//! [`crate::rules::EmpRules::blind_duration`] — four seconds of a coarse-aim
//! pilot shooting the way a mouse pilot always does.
//!
//! **"Give the victim something to do — a reboot input, mash a key to restore
//! systems faster. Otherwise the victim is just waiting, which is the least fun
//! state in any game."** Not built, because the premise does not hold here: the
//! victim is not waiting. They have a full flight model, a full weapon, and an
//! opponent in front of them; what they have lost is the help. A mash-to-reboot
//! prompt would replace *flying blind* — which is the experience the weapon
//! exists to create — with looking at a progress bar, and it would need a key
//! binding the JS client cannot mirror. The gradual part of the recovery is
//! there instead, in the cockpit's reboot ramp, so the last half-second is the
//! panel coming back rather than a switch.
//!
//! # Multiplayer
//!
//! [`detonate`] runs on every machine, against that machine's own copy of the
//! world, from a centre one machine chose. The firing client raises
//! [`crate::world::NetIntent::Emp`]; every other client receives
//! [`crate::world::NetEvent::EmpBurst`] and detonates the identical sphere.
//!
//! **The browser client and the Node server cannot carry it.** `server/index.js`
//! dispatches a fixed list of `msg.type` tags and drops anything else, so an
//! `emp` frame sent to it is silently discarded and no browser peer is ever told.
//! Against that server the pulse still blinds every ship the firing client
//! simulates — which is the bots the host drives — and no remote human. That is
//! stated where it can be acted on rather than hidden: `spaceships-protocol`
//! carries the message, the Rust server relays it, and the JS server is
//! deliberately not modified.
//!
//! # Determinism
//!
//! No randomness, no transcendentals, no iteration order that affects a result:
//! [`detonate`] sets the same field on every ship it reaches and the order it
//! visits them in cannot change the outcome. `distance` is a `sqrt`, which is
//! IEEE-754 exact.

use crate::math::Vec3;
use crate::rules::Rules;
use crate::world::{EntityId, Ship, ShipKind, World};

/// Advances one ship's EMP clocks by `dt`: the charge meter up, the blindness
/// down.
///
/// Called from [`crate::tick`]'s clock phase for every ship, alongside
/// [`crate::ship::tick_timers`].
///
/// **The meter only fills while alive.** Charging through a respawn would make
/// dying a way to pass the time, and §2's whole objection to a respawn-cycled
/// EMP is that death should not be a route to one. Blindness, by contrast, runs
/// down whether or not the pilot is alive: a ship that was blinded and then shot
/// should not come back still blind, and it does not, because the clock never
/// stopped.
pub fn tick_clocks(ship: &mut Ship, rules: &Rules, dt: f64) {
    if ship.emp_blind > 0.0 {
        ship.emp_blind = (ship.emp_blind - dt).max(0.0);
    }
    if ship.alive {
        charge(ship, rules, dt);
    }
}

/// Fills the charge meter, clamped to `1.0`.
///
/// Split out because a zero [`crate::rules::EmpRules::charge_time`] is a legal
/// rule set meaning "always armed", and the obvious `dt / charge_time` divides
/// by zero there. Handled once, here, rather than at the call site.
pub fn charge(ship: &mut Ship, rules: &Rules, dt: f64) {
    let time = rules.emp.charge_time;
    if time <= 0.0 {
        ship.emp_charge = 1.0;
        return;
    }
    ship.emp_charge = (ship.emp_charge + dt / time).min(1.0);
}

/// Whether this ship can set off a pulse right now.
#[must_use]
pub fn is_armed(ship: &Ship) -> bool {
    ship.alive && ship.emp_charge >= 1.0
}

/// Whether this ship is currently flying blind.
///
/// One predicate rather than four `> 0.0` tests spread across `aim_assist`,
/// `missiles`, `bot` and `tick`, so "what does blind mean" has one answer.
#[must_use]
pub fn is_blind(ship: &Ship) -> bool {
    ship.emp_blind > 0.0
}

/// Sets off `owner`'s EMP: spends the meter and detonates at their position.
///
/// Returns the centre of the pulse, or `None` if the ship does not exist, is
/// dead, or is not fully charged — in which case nothing is spent, exactly as
/// [`crate::missiles::fire`] keeps the round on the rail.
///
/// Firing while blind is deliberately allowed. The meter is not an instrument
/// and the button is not aimed, so a pilot who has just been caught can answer
/// with their own pulse — which is the one piece of counterplay a blinded pilot
/// has that costs them nothing to find.
pub fn fire(world: &mut World, owner: EntityId) -> Option<Vec3> {
    let ship = world.ship_mut(owner)?;
    if !is_armed(ship) {
        return None;
    }
    ship.emp_charge = 0.0;
    let origin = ship.pos;
    detonate(world, owner, origin);
    Some(origin)
}

/// Applies a pulse centred on `origin` to every ship in this world.
///
/// The four exclusions, in the order they are tested:
///
/// 1. **Boss hitboxes.** [`ShipKind::BossHitbox`] entries exist to be shot at;
///    there is no pilot behind one and nothing reads their blindness. Skipped so
///    a pulse near the capital ship does not quietly set a field on twenty
///    ships that will never look at it.
/// 2. **Ships that cannot currently be damaged.** That is
///    [`Ship::is_damageable`] — dead, or still inside the spawn-protection
///    window. Blindness is not damage, so this one is a choice rather than a
///    consequence, and the choice is that spawn protection means *protected*:
///    the whole reason the window exists is that a pilot who has just
///    materialised cannot yet defend themselves, and blinding them on arrival is
///    precisely the spawn-camp the window was added to prevent.
/// 3. **The pilot who fired**, unless
///    [`crate::rules::EmpRules::blinds_owner`].
/// 4. **Their team**, unless [`crate::rules::EmpRules::friendly_blind`] — which
///    defaults on, so by default this exclusion never fires. Both are rules
///    because both are §2 questions with a defensible other answer.
///
/// Blindness accumulates by `max`, never by sum: two overlapping pulses leave a
/// pilot dark for the longer of the two rather than for eight seconds. A weapon
/// whose effect stacked would be one that two attackers could hold a third
/// player under indefinitely, which is a different and much worse weapon.
pub fn detonate(world: &mut World, owner: EntityId, origin: Vec3) {
    let emp = world.rules.emp;
    let owner_team = world.ship(owner).and_then(|s| s.team);
    let r2 = emp.radius * emp.radius;

    for ship in &mut world.ships {
        if ship.kind == ShipKind::BossHitbox || !ship.is_damageable() {
            continue;
        }
        if ship.id == owner && !emp.blinds_owner {
            continue;
        }
        if !emp.friendly_blind && ship.id != owner {
            if let (Some(a), Some(b)) = (owner_team, ship.team) {
                if a == b {
                    continue;
                }
            }
        }
        // Squared, so the sphere test costs no `sqrt` per ship. The radius
        // itself is exact either way; this is the same comparison.
        let d = ship.pos - origin;
        if d.length_squared() > r2 {
            continue;
        }
        ship.emp_blind = ship.emp_blind.max(emp.blind_duration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::Quat;
    use crate::world::{MapKind, Mode, Team};

    fn world() -> World {
        World::new(9, Rules::DEFAULT, Mode::Skirmish, MapKind::Space)
    }

    /// Adds a ship at `pos` on `team`, past its spawn window.
    fn add(world: &mut World, id: EntityId, team: Team, pos: Vec3) {
        let rules = world.rules;
        let mut s = Ship::spawn(id, ShipKind::Bot, pos, Quat::IDENTITY, &rules);
        s.team = Some(team);
        s.invuln_timer = 0.0;
        world.ships.push(s);
    }

    #[test]
    fn a_fresh_ship_is_unarmed_and_charges_in_exactly_charge_time() {
        let rules = Rules::DEFAULT;
        let mut s = Ship::spawn(1, ShipKind::Local, Vec3::ZERO, Quat::IDENTITY, &rules);
        assert_eq!(s.emp_charge, 0.0, "a fresh spawn does not arrive armed");
        assert!(!is_armed(&s));

        // One second short.
        charge(&mut s, &rules, rules.emp.charge_time - 1.0);
        assert!(!is_armed(&s), "not armed a second early");
        charge(&mut s, &rules, 1.0);
        assert!(is_armed(&s));
        // And it does not run past full.
        charge(&mut s, &rules, 100.0);
        assert_eq!(s.emp_charge, 1.0);
    }

    #[test]
    fn a_zero_charge_time_arms_instantly_instead_of_dividing_by_it() {
        let mut rules = Rules::DEFAULT;
        rules.emp.charge_time = 0.0;
        assert!(rules.validate().is_ok());
        let mut s = Ship::spawn(1, ShipKind::Local, Vec3::ZERO, Quat::IDENTITY, &rules);
        charge(&mut s, &rules, 1.0 / 60.0);
        assert_eq!(s.emp_charge, 1.0);
        assert!(s.emp_charge.is_finite(), "must not be an infinity");
    }

    #[test]
    fn the_meter_survives_death_and_the_respawn_that_follows() {
        // The §2 requirement in one test: dying neither refunds a spent meter
        // nor fills an empty one.
        let rules = Rules::DEFAULT;
        let mut s = Ship::spawn(1, ShipKind::Local, Vec3::ZERO, Quat::IDENTITY, &rules);
        charge(&mut s, &rules, rules.emp.charge_time * 0.5);
        let held = s.emp_charge;

        crate::ship::kill(&mut s, &rules, Mode::Skirmish);
        assert_eq!(s.emp_charge, held, "death must not refund the meter");
        crate::ship::respawn(&mut s, Vec3::ZERO, Quat::IDENTITY, &rules);
        assert_eq!(s.emp_charge, held, "a fresh spawn must not arrive armed");
    }

    #[test]
    fn the_meter_stops_while_dead_but_the_blindness_does_not() {
        let rules = Rules::DEFAULT;
        let mut s = Ship::spawn(1, ShipKind::Local, Vec3::ZERO, Quat::IDENTITY, &rules);
        s.emp_blind = 1.0;
        crate::ship::kill(&mut s, &rules, Mode::Skirmish);

        tick_clocks(&mut s, &rules, 0.5);
        assert_eq!(s.emp_charge, 0.0, "a corpse does not charge");
        assert!(
            (s.emp_blind - 0.5).abs() < 1e-12,
            "but the blackout still runs down, so a respawn is not still blind"
        );
        tick_clocks(&mut s, &rules, 1.0);
        assert_eq!(s.emp_blind, 0.0, "and it floors at zero");
    }

    #[test]
    fn firing_spends_the_whole_meter_and_blinds_the_sphere() {
        let mut w = world();
        add(&mut w, 1, Team::Zero, Vec3::ZERO);
        let r = w.rules.emp.radius;
        add(&mut w, 2, Team::One, Vec3::new(r - 1.0, 0.0, 0.0));
        add(&mut w, 3, Team::One, Vec3::new(r + 1.0, 0.0, 0.0));
        w.ships[0].emp_charge = 1.0;

        assert_eq!(fire(&mut w, 1), Some(Vec3::ZERO));
        assert_eq!(w.ship(1).unwrap().emp_charge, 0.0, "one pulse, whole meter");
        assert_eq!(w.ship(1).unwrap().emp_blind, 0.0, "the emitter is hardened");
        assert_eq!(w.ship(2).unwrap().emp_blind, w.rules.emp.blind_duration);
        assert_eq!(w.ship(3).unwrap().emp_blind, 0.0, "outside the sphere");
    }

    #[test]
    fn an_unarmed_or_dead_pilot_fires_nothing_and_spends_nothing() {
        let mut w = world();
        add(&mut w, 1, Team::Zero, Vec3::ZERO);
        add(&mut w, 2, Team::One, Vec3::new(10.0, 0.0, 0.0));

        w.ships[0].emp_charge = 0.99;
        assert_eq!(fire(&mut w, 1), None);
        assert_eq!(w.ship(1).unwrap().emp_charge, 0.99, "nothing was spent");
        assert_eq!(w.ship(2).unwrap().emp_blind, 0.0);

        w.ships[0].emp_charge = 1.0;
        w.ships[0].alive = false;
        assert_eq!(fire(&mut w, 1), None);
        assert_eq!(w.ship(1).unwrap().emp_charge, 1.0);

        assert_eq!(
            fire(&mut w, 404),
            None,
            "and an id nobody has is not a panic"
        );
    }

    #[test]
    fn allies_are_caught_by_default_which_is_the_point_of_the_weapon() {
        let mut w = world();
        add(&mut w, 1, Team::Zero, Vec3::ZERO);
        add(&mut w, 2, Team::Zero, Vec3::new(50.0, 0.0, 0.0));
        w.ships[0].emp_charge = 1.0;
        fire(&mut w, 1);
        assert_eq!(
            w.ship(2).unwrap().emp_blind,
            w.rules.emp.blind_duration,
            "friendly blinding is on: firing inside a furball costs your wing"
        );

        // And the rule turns it off, which is the alternative §2 offers.
        let mut w = world();
        w.rules.emp.friendly_blind = false;
        add(&mut w, 1, Team::Zero, Vec3::ZERO);
        add(&mut w, 2, Team::Zero, Vec3::new(50.0, 0.0, 0.0));
        add(&mut w, 3, Team::One, Vec3::new(50.0, 0.0, 0.0));
        w.ships[0].emp_charge = 1.0;
        fire(&mut w, 1);
        assert_eq!(w.ship(2).unwrap().emp_blind, 0.0);
        assert_eq!(w.ship(3).unwrap().emp_blind, w.rules.emp.blind_duration);
    }

    #[test]
    fn the_owner_is_caught_only_when_the_rule_says_so() {
        let mut w = world();
        w.rules.emp.blinds_owner = true;
        add(&mut w, 1, Team::Zero, Vec3::ZERO);
        w.ships[0].emp_charge = 1.0;
        fire(&mut w, 1);
        assert_eq!(
            w.ship(1).unwrap().emp_blind,
            w.rules.emp.blind_duration,
            "the screenshot hook's whole trick"
        );
    }

    #[test]
    fn spawn_protection_and_death_both_keep_a_ship_out_of_the_pulse() {
        let mut w = world();
        add(&mut w, 1, Team::Zero, Vec3::ZERO);
        add(&mut w, 2, Team::One, Vec3::new(10.0, 0.0, 0.0));
        add(&mut w, 3, Team::One, Vec3::new(20.0, 0.0, 0.0));
        w.ships[1].invuln_timer = w.rules.combat.spawn_invuln;
        w.ships[2].alive = false;

        detonate(&mut w, 1, Vec3::ZERO);
        assert_eq!(
            w.ship(2).unwrap().emp_blind,
            0.0,
            "a pilot inside their spawn window is protected from this too"
        );
        assert_eq!(
            w.ship(3).unwrap().emp_blind,
            0.0,
            "and a corpse is not blind"
        );
    }

    #[test]
    fn boss_hitboxes_are_not_pilots() {
        let mut w = world();
        add(&mut w, 1, Team::Zero, Vec3::ZERO);
        let rules = w.rules;
        let mut hitbox = Ship::spawn(
            crate::rules::BOSS_ID_BASE,
            ShipKind::BossHitbox,
            Vec3::new(10.0, 0.0, 0.0),
            Quat::IDENTITY,
            &rules,
        );
        hitbox.invuln_timer = 0.0;
        w.ships.push(hitbox);

        detonate(&mut w, 1, Vec3::ZERO);
        assert_eq!(w.ships[1].emp_blind, 0.0);
    }

    #[test]
    fn overlapping_pulses_take_the_longer_one_rather_than_adding_up() {
        let mut w = world();
        add(&mut w, 1, Team::Zero, Vec3::ZERO);
        add(&mut w, 2, Team::One, Vec3::new(10.0, 0.0, 0.0));
        let full = w.rules.emp.blind_duration;

        detonate(&mut w, 1, Vec3::ZERO);
        w.ships[1].emp_blind = full - 1.0; // a second has run off
        detonate(&mut w, 1, Vec3::ZERO);
        assert_eq!(
            w.ship(2).unwrap().emp_blind,
            full,
            "a second pulse tops the blackout up, it does not double it"
        );

        // And a shorter pending blackout cannot cut a longer one short.
        w.ships[1].emp_blind = full + 10.0;
        detonate(&mut w, 1, Vec3::ZERO);
        assert_eq!(w.ship(2).unwrap().emp_blind, full + 10.0);
    }

    #[test]
    fn the_pulse_is_a_sphere_and_the_edge_is_inclusive() {
        let mut w = world();
        let r = w.rules.emp.radius;
        add(&mut w, 1, Team::Zero, Vec3::ZERO);
        // Exactly on the surface, on an axis so the arithmetic is exact.
        add(&mut w, 2, Team::One, Vec3::new(0.0, r, 0.0));
        // A hair outside, and a diagonal well inside.
        add(&mut w, 3, Team::One, Vec3::new(0.0, 0.0, r * 1.000_001));
        add(&mut w, 4, Team::One, Vec3::new(r * 0.5, r * 0.5, r * 0.5));

        detonate(&mut w, 1, Vec3::ZERO);
        assert!(w.ship(2).unwrap().emp_blind > 0.0, "the surface is caught");
        assert_eq!(w.ship(3).unwrap().emp_blind, 0.0);
        assert!(w.ship(4).unwrap().emp_blind > 0.0);
    }

    #[test]
    fn detonating_somewhere_else_catches_the_ships_there_not_the_firer() {
        // The multiplayer path: a client is told where a pulse went off and
        // resolves it against its own poses, so the centre is an argument.
        let mut w = world();
        add(&mut w, 1, Team::Zero, Vec3::ZERO);
        add(&mut w, 7, Team::One, Vec3::new(1000.0, 0.0, 0.0));
        detonate(&mut w, 7, Vec3::new(1000.0, 0.0, 0.0));
        assert_eq!(w.ship(1).unwrap().emp_blind, 0.0, "a thousand units away");

        detonate(&mut w, 7, Vec3::new(100.0, 0.0, 0.0));
        assert!(w.ship(1).unwrap().emp_blind > 0.0);
        assert_eq!(w.ship(7).unwrap().emp_blind, 0.0, "still not the firer");
    }
}
