//! [`World`] on disk: every field of the simulation state, written once.
//!
//! # Why the whole world and not just a seed
//!
//! `BACKLOG.md` describes a replay as `seed + rules snapshot + input log`, and
//! for a solo match that is enough — [`spaceships_sim::world::World::new`] plus
//! `asteroids::populate` plus the lobby's spawn pass reconstructs tick zero
//! exactly. It is *not* enough for anything else:
//!
//! - **A networked match's opening state comes off the wire**, not out of a
//!   seed. The server picks the spawns, the teams, and who drives which balance
//!   bot, and hands them over in the `start` message. There is no function to
//!   call that reproduces them.
//! - **Reconstructing means duplicating the lobby.** `sim_bridge::new_match`
//!   decides how many bots a skirmish has and `net.rs::start_networked_match`
//!   decides what a networked one has. A replay that re-ran either would break
//!   the day either changed, and it would break *quietly* — as a match that
//!   plays out slightly differently, not as an error.
//! - **Four of the five RNG streams have already been drawn from** by the time
//!   the first tick runs: the field generator has laid out a belt and every bot
//!   has rolled its opening missile delay. `WorldRng::from_seed` rebuilds the
//!   streams at their *start*, which is not where they are.
//!
//! Writing the state out costs about ten kilobytes for a deathmatch — an
//! asteroid is 88 bytes and there are sixty of them — against a log that runs
//! to hundreds. It is the small half of the file, and it makes recording
//! mode-agnostic: solo, campaign, trials and multiplayer all record and replay
//! through one path, because the only thing any of them contributes is a
//! `World`.
//!
//! # What is *not* written
//!
//! [`World::rules`]. See [`crate::Recording::rules_hash`] — the rules are
//! pinned by fingerprint and restored from [`Rules::DEFAULT`], because today
//! there is exactly one `Rules` value in existence and storing 265 fields to
//! say so would be storage for a distinction that cannot yet be drawn.

use spaceships_sim::rng::Rng;
use spaceships_sim::rules::Rules;
use spaceships_sim::world::{
    AimAssistState, Asteroid, AsteroidTier, Authority, BotFsm, BotState, BoxVolume, Bullet,
    CampaignPhase, CampaignState, EntityId, Flare, GunMode, MapKind, MatchState, Missile,
    MissileTarget, Mode, Obstacle, RemoteInterp, Score, Ship, ShipKind, Team, TrialsState, Turret,
    World, WorldRng,
};

use crate::wire::{Dec, Enc, Error, Result, Wire};
use crate::{wire_enum, wire_struct};

// ---------------------------------------------------------------------------
// Primitives sim uses that the byte layer does not define
// ---------------------------------------------------------------------------

/// `usize` is 64-bit on the desktop and 32-bit in the browser, so it travels as
/// a `u64` and a value that will not fit on the reading side is an error rather
/// than a truncation.
impl Wire for usize {
    fn put(&self, e: &mut Enc) {
        (*self as u64).put(e);
    }
    fn get(d: &mut Dec<'_>) -> Result<usize> {
        usize::try_from(u64::get(d)?).map_err(|_| Error::Truncated)
    }
}

/// A generator's position, as the two words [`Rng::to_raw`] hands over.
///
/// Not the seed: a stream that has been drawn from is not where a seed puts it.
/// See that method's docs.
impl Wire for Rng {
    fn put(&self, e: &mut Enc) {
        let (state, inc) = self.to_raw();
        state.put(e);
        inc.put(e);
    }
    fn get(d: &mut Dec<'_>) -> Result<Rng> {
        let state = u64::get(d)?;
        let inc = u64::get(d)?;
        Ok(Rng::from_raw(state, inc))
    }
}

// ---------------------------------------------------------------------------
// The fieldless enums
// ---------------------------------------------------------------------------
//
// Every tag below is a number chosen here rather than a discriminant read off
// the type. Reordering a variant in `sim` must not change what an old recording
// decodes to, and this is the list that stops it.

wire_enum!(Team, "team" { 0 => Zero, 1 => One });
wire_enum!(MapKind, "map" { 0 => Space, 1 => Terrain });
wire_enum!(Authority, "authority" { 0 => Local, 1 => Server });
wire_enum!(GunMode, "gun mode" { 0 => Bullet, 1 => Beam });
wire_enum!(ShipKind, "ship kind" { 0 => Local, 1 => Remote, 2 => Bot, 3 => BossHitbox });
wire_enum!(BotFsm, "bot state" { 0 => Seek, 1 => Attack, 2 => Evade });
wire_enum!(AsteroidTier, "asteroid tier" { 0 => Small, 1 => Medium, 2 => Big, 3 => Huge });
wire_enum!(CampaignPhase, "campaign phase" { 0 => Wave, 1 => Boss, 2 => Victory, 3 => Failed });

/// Two of [`Mode`]'s six variants carry a number, so this is written out rather
/// than generated.
impl Wire for Mode {
    fn put(&self, e: &mut Enc) {
        match self {
            Mode::Multiplayer => 0u8.put(e),
            Mode::Training => 1u8.put(e),
            Mode::Skirmish => 2u8.put(e),
            Mode::Tutorial => 3u8.put(e),
            Mode::Trials(n) => {
                4u8.put(e);
                n.put(e);
            }
            Mode::Campaign(n) => {
                5u8.put(e);
                n.put(e);
            }
        }
    }
    fn get(d: &mut Dec<'_>) -> Result<Mode> {
        match u8::get(d)? {
            0 => Ok(Mode::Multiplayer),
            1 => Ok(Mode::Training),
            2 => Ok(Mode::Skirmish),
            3 => Ok(Mode::Tutorial),
            4 => Ok(Mode::Trials(u8::get(d)?)),
            5 => Ok(Mode::Campaign(u8::get(d)?)),
            tag => Err(Error::BadTag { tag, what: "mode" }),
        }
    }
}

/// What a missile is chasing: a ship id, or a flare key.
impl Wire for MissileTarget {
    fn put(&self, e: &mut Enc) {
        match self {
            MissileTarget::Ship(id) => {
                0u8.put(e);
                id.put(e);
            }
            MissileTarget::Flare(key) => {
                1u8.put(e);
                key.put(e);
            }
        }
    }
    fn get(d: &mut Dec<'_>) -> Result<MissileTarget> {
        match u8::get(d)? {
            0 => Ok(MissileTarget::Ship(EntityId::get(d)?)),
            1 => Ok(MissileTarget::Flare(u64::get(d)?)),
            tag => Err(Error::BadTag {
                tag,
                what: "missile target",
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// The records
// ---------------------------------------------------------------------------

wire_struct!(RemoteInterp {
    target_pos,
    target_quat,
    has_target,
    last_state_time,
    last_state_pos,
    vel_seeded,
    boost,
});

wire_struct!(BotState {
    fsm,
    state_timer,
    fire_timer,
    missiles_left,
    missile_timer,
    stuck_time,
    evade_axis,
    aim_offset,
    tracked_lead,
    tracked_lead_seeded,
    hard_mode,
    is_campaign_bot,
});

wire_struct!(Ship {
    id,
    team,
    kind,
    pos,
    quat,
    vel,
    throttle,
    target_throttle,
    arrow_kx,
    arrow_ky,
    hp,
    alive,
    respawn_timer,
    invuln_timer,
    gun_mode,
    fire_timer,
    ammo,
    ammo_idle,
    missiles_left,
    flares_left,
    boost_meter,
    boost_idle,
    brake_charge,
    brake_boost_timer,
    brake_boost_charge,
    brake_overcharge_time,
    self_damage_accum,
    prev_braking,
    health_idle_damage,
    health_idle_shot,
    health_regen_tick,
    coarse_aim,
    hit_radius_override,
    touching_asteroids,
    asteroid_damage_cooldown,
    touching_moon,
    touching_ground,
    hit_flash,
    interp,
    bot,
});

wire_struct!(Bullet {
    key,
    pos,
    prev_pos,
    vel,
    life,
    owner,
    owner_team,
    owner_coarse_aim,
    owner_is_bot,
    damage,
});

wire_struct!(Missile {
    key,
    pos,
    dir,
    target,
    life,
    age,
    owner,
    owner_team,
});

wire_struct!(Flare {
    key,
    pos,
    vel,
    life,
    age,
    owner,
});

wire_struct!(Asteroid {
    id,
    pos,
    size,
    radius,
    hp,
    tier,
    variant,
    rot,
    spin,
    hit_flash,
});

wire_struct!(Obstacle { pos, radius });
wire_struct!(BoxVolume { pos, half });

wire_struct!(AimAssistState {
    enabled,
    strength_smoothed,
    target,
    has_target,
    target_dir,
});

wire_struct!(TrialsState {
    trial,
    checkpoints,
    next_cp,
    timer,
    lap,
    running,
    best_lap,
    last_lap,
    cp_cooldown,
    countdown,
    countdown_active,
});

wire_struct!(Turret {
    local_pos,
    yaw,
    pitch,
    fire_timer,
});

wire_struct!(CampaignState {
    mission,
    phase,
    wave_index,
    wave_bot_ids,
    bots_alive,
    between,
    between_timer,
    lives,
    checkpoint_pos,
    next_bot_id,
    warp_timer,
    boss_hp,
    boss_active,
    boss_pos,
    boss_time,
    turrets,
});

wire_struct!(Score {
    id,
    team,
    kills,
    deaths,
});

wire_struct!(MatchState {
    timer,
    team_kills,
    over,
    active,
    solo_bots_killed,
    scores,
});

wire_struct!(WorldRng {
    seed,
    field,
    spawn,
    combat,
    bots,
    effects,
});

// ---------------------------------------------------------------------------
// The world
// ---------------------------------------------------------------------------

/// Written by hand for one reason: [`World::rules`] is not on the wire.
///
/// Everything else is the field list in [`World`]'s own order.
impl Wire for World {
    fn put(&self, e: &mut Enc) {
        // `rules` — pinned by fingerprint, see the module docs.
        self.rng.put(e);
        self.time.put(e);
        self.tick.put(e);
        self.mode.put(e);
        self.map.put(e);
        self.authority.put(e);
        self.local_id.put(e);
        self.ships.put(e);
        self.bullets.put(e);
        self.missiles.put(e);
        self.flares.put(e);
        self.asteroids.put(e);
        self.obstacles.put(e);
        self.boxes.put(e);
        self.match_state.put(e);
        self.aim_assist.put(e);
        self.trials.put(e);
        self.campaign.put(e);
        self.next_projectile_key.put(e);
        self.next_asteroid_id.put(e);
    }

    fn get(d: &mut Dec<'_>) -> Result<World> {
        let rng = WorldRng::get(d)?;
        let time = f64::get(d)?;
        let tick = u64::get(d)?;
        let mode = Mode::get(d)?;
        let map = MapKind::get(d)?;

        // `World::new` seeds the static geometry and the match clock; every one
        // of those fields is then overwritten below. Going through the
        // constructor rather than building the struct literally is what keeps
        // this compiling — and failing loudly — the day `World` grows a field
        // this codec has not been told about.
        let mut w = World::new(rng.seed, Rules::DEFAULT, mode, map);
        w.rng = rng;
        w.time = time;
        w.tick = tick;
        w.authority = Authority::get(d)?;
        w.local_id = Option::<EntityId>::get(d)?;
        w.ships = Vec::<Ship>::get(d)?;
        w.bullets = Vec::<Bullet>::get(d)?;
        w.missiles = Vec::<Missile>::get(d)?;
        w.flares = Vec::<Flare>::get(d)?;
        w.asteroids = Vec::<Asteroid>::get(d)?;
        w.obstacles = Vec::<Obstacle>::get(d)?;
        w.boxes = Vec::<BoxVolume>::get(d)?;
        w.match_state = MatchState::get(d)?;
        w.aim_assist = AimAssistState::get(d)?;
        w.trials = Option::<TrialsState>::get(d)?;
        w.campaign = Option::<CampaignState>::get(d)?;
        w.next_projectile_key = u64::get(d)?;
        w.next_asteroid_id = u32::get(d)?;
        Ok(w)
    }
}

// ---------------------------------------------------------------------------
// The rules fingerprint
// ---------------------------------------------------------------------------

/// A 64-bit fingerprint of every value in [`Rules`].
///
/// # Why `Debug` and not 265 hand-written fields
///
/// `Rules` derives `Debug`, and a derived `Debug` renders **every** field. So
/// the rendering is a complete description of the value, and hashing it gives a
/// fingerprint that changes if any rule changes — including a rule added
/// tomorrow, which is the case a hand-written list would silently miss. `f64`'s
/// `Debug` prints the shortest string that round-trips, so two distinct values
/// never render alike.
///
/// The hash is FNV-1a, which is chosen for being written down rather than for
/// being good: nothing here guards a secret, and the requirement is only that
/// the same rules give the same number on every platform. It is `const`-free
/// and dependency-free, which is the whole reason it is not `DefaultHasher` —
/// `std`'s hasher is explicitly documented as not stable across releases, so a
/// recording made yesterday would fail to load tomorrow for no reason at all.
#[must_use]
pub fn rules_fingerprint(rules: &Rules) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let text = format!("{rules:?}");
    let mut hash = OFFSET;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Convenience: the fingerprint of the rules this build ships.
#[must_use]
pub fn default_rules_fingerprint() -> u64 {
    rules_fingerprint(&Rules::DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use spaceships_sim::math::{Quat, Vec3};
    use spaceships_sim::world::TICK_DT;

    fn encode(w: &World) -> Vec<u8> {
        let mut e = Enc::new();
        w.put(&mut e);
        e.finish()
    }

    fn decode(bytes: &[u8]) -> World {
        let mut d = Dec::new(bytes);
        let w = World::get(&mut d).expect("decodes");
        assert!(d.is_empty(), "the world must consume the whole buffer");
        w
    }

    /// A busy world: bots that have been shot at, projectiles in flight, a
    /// scoreboard, and four RNG streams well away from their seeds.
    fn busy_world() -> World {
        let mut w = World::new(0xB01D, Rules::DEFAULT, Mode::Skirmish, MapKind::Space);
        spaceships_sim::asteroids::populate(&mut w);

        let mut me = Ship::spawn(1, ShipKind::Local, Vec3::ZERO, Quat::IDENTITY, &w.rules);
        me.team = Some(Team::Zero);
        w.ships.push(me);
        w.local_id = Some(1);

        let rules = w.rules;
        for i in 0..4 {
            let at = Vec3::new(f64::from(i) * 30.0, 0.0, 120.0);
            let mut bot = Ship::spawn(10 + i, ShipKind::Bot, at, Quat::FLIP_Y, &rules);
            bot.team = Some(Team::One);
            spaceships_sim::bot::init(&mut bot, false, false, &rules, &mut w.rng.bots);
            w.ships.push(bot);
            w.match_state.scores.push(Score {
                id: 10 + i,
                team: Some(Team::One),
                kills: 0,
                deaths: 0,
            });
        }

        // Run it long enough that bullets, hits and flashes exist.
        let hold = spaceships_sim::world::Input {
            id: 1,
            fire: true,
            throttle_axis: 1.0,
            steer_x: 0.2,
            ..Default::default()
        };
        for _ in 0..240 {
            spaceships_sim::tick::tick(&mut w, &[hold], &[], TICK_DT);
        }
        w
    }

    #[test]
    fn a_busy_world_round_trips_exactly() {
        let w = busy_world();
        assert!(!w.bullets.is_empty(), "the fixture must have projectiles");
        assert!(!w.asteroids.is_empty(), "and a field");
        let back = decode(&encode(&w));
        assert!(back == w, "a decoded world must equal the one encoded");
    }

    /// The real requirement is not equality on the spot — it is that the
    /// restored world *carries on identically*, which is what a seek does.
    #[test]
    fn a_restored_world_continues_the_same_match() {
        let mut a = busy_world();
        let mut b = decode(&encode(&a));

        let input = spaceships_sim::world::Input {
            id: 1,
            fire: true,
            steer_y: -0.4,
            ..Default::default()
        };
        for _ in 0..300 {
            spaceships_sim::tick::tick(&mut a, &[input], &[], TICK_DT);
            spaceships_sim::tick::tick(&mut b, &[input], &[], TICK_DT);
        }
        assert!(a == b, "a restored world must diverge from nothing");
    }

    #[test]
    fn the_campaign_and_its_boss_round_trip() {
        let mut w = World::new(0xB055, Rules::DEFAULT, Mode::Campaign(3), MapKind::Space);
        spaceships_sim::asteroids::populate(&mut w);
        let me = Ship::spawn(
            1,
            ShipKind::Local,
            Vec3::new(0.0, 0.0, 380.0),
            Quat::IDENTITY,
            &w.rules,
        );
        w.ships.push(me);
        w.local_id = Some(1);
        spaceships_sim::campaign::init(&mut w, true);
        spaceships_sim::campaign::activate_boss(&mut w, &mut Vec::new());
        for _ in 0..120 {
            spaceships_sim::tick::tick(&mut w, &[], &[], TICK_DT);
        }
        assert!(w.campaign.is_some());
        assert!(decode(&encode(&w)) == w);
    }

    #[test]
    fn a_trials_world_round_trips() {
        let mut w = World::new(3, Rules::DEFAULT, Mode::Trials(2), MapKind::Space);
        w.trials = Some(TrialsState {
            trial: 2,
            checkpoints: spaceships_sim::rules::trial_checkpoints(2).to_vec(),
            next_cp: 3,
            timer: 12.5,
            lap: 1,
            running: true,
            best_lap: Some(41.25),
            last_lap: None,
            cp_cooldown: 0.4,
            countdown: 0.0,
            countdown_active: false,
        });
        assert!(decode(&encode(&w)) == w);
    }

    /// The fingerprint must actually depend on the rules, or it pins nothing.
    #[test]
    fn the_fingerprint_moves_when_a_rule_does() {
        let base = default_rules_fingerprint();
        let mut tweaked = Rules::DEFAULT;
        tweaked.ship.max_throttle += 1.0;
        assert_ne!(base, rules_fingerprint(&tweaked));

        // And it is stable for an unchanged value, on every run.
        assert_eq!(base, default_rules_fingerprint());
    }

    #[test]
    fn a_truncated_world_is_an_error_not_a_panic() {
        let bytes = encode(&busy_world());
        assert!(World::get(&mut Dec::new(&bytes[..bytes.len() / 2])).is_err());
    }
}
