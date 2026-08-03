//! Every tunable game rule, written down exactly once.
//!
//! # Why this module exists
//!
//! The JS game defines its rules twice: once on the client (`public/src/`) and
//! once on the server (`server/index.js`). Nothing keeps the two copies in step,
//! and they have drifted — respawn takes 2.5 s in solo and 2.0 s in multiplayer,
//! a ship is 4.0 units wide to a bot's bullet and 7.0 to a player's, the
//! campaign boss has three different hit radii depending on which trigger pulled,
//! and server-generated asteroids spawn inside the moon because only the client
//! knows the moon is an obstacle.
//!
//! Every one of those is the same bug: a rule with two homes. This module gives
//! each rule one home. Where the two originals disagreed, one value was picked
//! and the decision is recorded in the field's doc comment together with both
//! original values and their `file.js:line`.
//!
//! # How to read a citation
//!
//! Citations are `file.js:line` against the working tree this module was written
//! from. Line numbers rot; the identifier does not. Grep for the JS identifier
//! named in the comment rather than jumping to the line.
//!
//! # Why a struct and not a wall of `const`
//!
//! [`Rules`] is a plain value with a [`Rules::DEFAULT`] constant. Tests can vary
//! a single field without touching global state, and future game modes (ranked,
//! sandbox, a rebalance behind a flag) become a different `Rules` value rather
//! than a fork of the simulation. Only values that are *identity* rather than
//! *balance* — id ranges, table sizes, fixed geometry — are free-standing
//! `const`s here, because varying them would not be a rebalance, it would be a
//! different game.
//!
//! # Units
//!
//! Distances are world units, times are seconds, angles are radians, rates are
//! per-second. HP is an integer. All floats are `f64`; see the `world` module
//! for why.

use crate::math::Vec3;

// ---------------------------------------------------------------------------
// Genuinely universal constants — identity, not balance.
// ---------------------------------------------------------------------------

/// First entity id of the campaign capital ship's hitbox cluster.
///
/// The boss is not a single entity: it is [`BOSS_HITBOX_COUNT`] pseudo-players
/// inserted into the same entity table as everything else so that the ordinary
/// weapon code can hit it. `main.js:2272` (`BOSS_ID_BASE`), inserted at
/// `main.js:2933`.
pub const BOSS_ID_BASE: i32 = 9000;

/// Number of pseudo-player hitboxes making up the campaign boss.
///
/// `main.js:2273` (`BOSS_HITBOX_COUNT`).
pub const BOSS_HITBOX_COUNT: usize = 20;

/// Offsets of the boss hitboxes from the capital ship's origin.
///
/// Despite the JS name `BOSS_HB_OFFSETS_WORLD` (`main.js:2298`) these are added
/// to the capital ship position *without* applying its rotation
/// (`main.js:2664`), so they are world-axis offsets from a hull that never
/// yaws. Reproduce that, or the hitboxes will detach from the model.
///
/// Note the spacing: 57 units apart in x, 75 in z, against a hitbox radius of
/// [`WeaponRules::boss_hitbox_radius`] (28). The x rows very nearly touch; the z
/// rows leave a 19-unit gap. See that field's docs for why this matters.
pub const BOSS_HITBOX_OFFSETS: [Vec3; BOSS_HITBOX_COUNT] = [
    Vec3::new(-85.0, 0.0, -150.0),
    Vec3::new(-28.0, 0.0, -150.0),
    Vec3::new(28.0, 0.0, -150.0),
    Vec3::new(85.0, 0.0, -150.0),
    Vec3::new(-85.0, 0.0, -75.0),
    Vec3::new(-28.0, 0.0, -75.0),
    Vec3::new(28.0, 0.0, -75.0),
    Vec3::new(85.0, 0.0, -75.0),
    Vec3::new(-85.0, 0.0, 0.0),
    Vec3::new(0.0, 0.0, 0.0),
    Vec3::new(85.0, 0.0, 0.0),
    Vec3::new(-85.0, 0.0, 75.0),
    Vec3::new(-28.0, 0.0, 75.0),
    Vec3::new(28.0, 0.0, 75.0),
    Vec3::new(85.0, 0.0, 75.0),
    Vec3::new(-85.0, 0.0, 150.0),
    Vec3::new(-28.0, 0.0, 150.0),
    Vec3::new(28.0, 0.0, 150.0),
    Vec3::new(85.0, 0.0, 150.0),
    Vec3::new(0.0, 30.0, 50.0),
];

/// Number of turrets on the campaign capital ship.
///
/// `main.js:2626` (`turretLocalPositions`).
pub const BOSS_TURRET_COUNT: usize = 4;

/// Turret pivot positions in capital-ship local space.
///
/// `main.js:2626` lists the *base* positions; the firing pivot sits 4 units
/// above each (`main.js:2638`), which is what is stored here.
pub const BOSS_TURRET_PIVOTS: [Vec3; BOSS_TURRET_COUNT] = [
    Vec3::new(-80.0, 22.0, 110.0),
    Vec3::new(80.0, 22.0, 110.0),
    Vec3::new(-80.0, 22.0, -110.0),
    Vec3::new(80.0, 22.0, -110.0),
];

/// Muzzle offset from a turret pivot, in the pivot's local space.
///
/// `main.js:2670`.
pub const BOSS_TURRET_MUZZLE: Vec3 = Vec3::new(0.0, 1.8, 22.0);

/// Number of asteroid size tiers. Fixed by the shape of the tier table, which
/// the client, the server, and the campaign generator all agree on.
pub const ASTEROID_TIER_COUNT: usize = 4;

/// Largest frame delta the JS render loop will ever hand the simulation.
///
/// `main.js:3485` clamps with `Math.min(0.05, clock.getDelta())`. This is a
/// *rule*, not a performance guard: a tab that was backgrounded for ten seconds
/// must not teleport every ship ten seconds forward. A caller that runs a fixed
/// timestep should accumulate at most this much real time per frame before
/// dropping the remainder.
pub const MAX_FRAME_DT: f64 = 0.05;

// ---------------------------------------------------------------------------
// Asteroid tiers
// ---------------------------------------------------------------------------

/// One row of the asteroid size/HP/frequency table.
///
/// The identical table exists in three places today — `asteroids.js:14`
/// (`TIERS`), `main.js:245` (`TIERS_LOCAL`, campaign generator, spelling the
/// weight `w` instead of `weight`) and `server/index.js:520`
/// (`ASTEROID_TIERS`). All three agree on every number, which is luck rather
/// than design. This is the one copy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AsteroidTierSpec {
    /// Smallest mesh scale for this tier.
    pub min_size: f64,
    /// Largest mesh scale for this tier.
    pub max_size: f64,
    /// Hit points. One point is removed per weapon impact regardless of weapon
    /// (see [`CombatRules::asteroid_damage_per_hit`]).
    pub hp: i32,
    /// Relative frequency when rolling a tier. The four weights sum to 1.
    pub weight: f64,
    /// Multiplier applied to the raw `[-0.5, 0.5)` per-axis spin draw.
    ///
    /// `asteroids.js:54` (`spinScaleFor`) and `asteroids.js:172`. Purely
    /// cosmetic — collision uses a constant radius — but it is written three
    /// times in JS with three different results, so it lives here.
    ///
    /// **Apply this exactly once.** `createAsteroidFieldFromData`
    /// (`asteroids.js:90`) multiplies incoming spin by the scale, so the server
    /// (`server/index.js:581`) correctly sends the *unscaled* `±0.5` draw, but
    /// the campaign generator (`main.js:265`) sends an already-narrowed
    /// `±0.2`/`±0.1` that then gets scaled a second time, leaving campaign rocks
    /// spinning at 20–40 % of everyone else's rate.
    pub spin_scale: f64,
}

/// The unified asteroid tier table, smallest first.
///
/// Ordering is load-bearing: the weighted pick walks cumulative weights in this
/// order, so reordering the rows changes which tier a given random draw selects
/// and therefore changes every field generated from a given seed.
// Formatted as a table on purpose: it is diffed column-by-column against the
// three JS copies it replaces, which rustfmt's one-field-per-line expansion
// makes needlessly hard.
#[rustfmt::skip]
pub const ASTEROID_TIERS: [AsteroidTierSpec; ASTEROID_TIER_COUNT] = [
    AsteroidTierSpec { min_size: 5.0, max_size: 7.0, hp: 5, weight: 0.45, spin_scale: 0.5 },
    AsteroidTierSpec { min_size: 9.0, max_size: 15.0, hp: 10, weight: 0.30, spin_scale: 0.32 },
    AsteroidTierSpec { min_size: 18.0, max_size: 30.0, hp: 30, weight: 0.18, spin_scale: 0.18 },
    AsteroidTierSpec { min_size: 38.0, max_size: 55.0, hp: 50, weight: 0.07, spin_scale: 0.10 },
];

/// The tier weights alone, in table order.
///
/// Convenience for `Rng::weighted_index`, which is the deterministic
/// replacement for `pickTier` (`asteroids.js:20`) and `pickAsteroidTier`
/// (`server/index.js:531`).
#[must_use]
pub fn asteroid_tier_weights() -> [f64; ASTEROID_TIER_COUNT] {
    let mut out = [0.0; ASTEROID_TIER_COUNT];
    let mut i = 0;
    while i < ASTEROID_TIER_COUNT {
        out[i] = ASTEROID_TIERS[i].weight;
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Aim assist
// ---------------------------------------------------------------------------

/// Which aim-assist tuning a player gets.
///
/// `main.js:405` derives this from the control scheme: mouse pilots get
/// [`AimProfile::Precise`], keyboard/mobile/no-mouse pilots get
/// [`AimProfile::Coarse`]. It is an accessibility setting, not a difficulty
/// setting, and it is the *one* legitimate reason for two players to see
/// different hit geometry — see [`ShipRules::hit_radius_coarse_aim_bonus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AimProfile {
    /// Mouse or right-stick aiming.
    #[default]
    Precise,
    /// Keyboard, touch, or gamepad-only aiming.
    Coarse,
}

/// Aim-assist tuning for one [`AimProfile`].
///
/// All fields from `main.js:1000`–`:1008`, which declares each of them as a
/// `coarseAim ? a : b` ternary. Splitting the ternary into two named profiles
/// removes the branch from the simulation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AimAssistTuning {
    /// Minimum `dot(forward, to_target)` for a target to be considered.
    /// `main.js:1000` (`ASSIST_CONE_DOT`), 0.60 precise / 0.5 coarse.
    pub cone_dot: f64,
    /// Extra cone tolerance granted to the currently-held target, so assist
    /// does not flicker between two candidates. `main.js:1007`
    /// (`ASSIST_STICKY_DOT_BONUS`).
    pub sticky_dot_bonus: f64,
    /// Rotation rate toward the intercept point. `main.js:1004`
    /// (`ASSIST_STRENGTH`), 2.6 precise / 2.2 coarse.
    pub strength: f64,
    /// Fraction of [`AimAssistRules::range`] at which strength starts falling
    /// off. `main.js:1005` (`ASSIST_FALLOFF_START`).
    pub falloff_start: f64,
    /// Angular error below which assist does nothing, so a perfectly-aimed shot
    /// is never nudged. `main.js:1006` (`ASSIST_DEAD_ANGLE`).
    pub dead_angle: f64,
    /// Steering magnitude at which the player is judged to be aiming
    /// deliberately and assist releases. `main.js:1008`
    /// (`ASSIST_INTENT_BREAK`), 1.8 precise / 0.25 coarse — coarse breaks far
    /// sooner because arrow keys saturate at 1.0.
    pub intent_break: f64,
}

/// Aim assist, both profiles plus the ranges they share.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AimAssistRules {
    /// Nearest range at which assist engages. `main.js:1001`
    /// (`ASSIST_MIN_RANGE`).
    pub min_range: f64,
    /// Furthest range at which assist engages. `main.js:1002`
    /// (`ASSIST_RANGE`).
    pub range: f64,
    /// Damping rate for [`crate::world::AimAssistState::strength_smoothed`], so
    /// acquiring and losing a target ramps instead of stepping. `main.js:2063`
    /// and `:2114` — the bare `6` in both `MathUtils.damp` calls.
    pub strength_damp_rate: f64,
    /// Damping rate for [`crate::world::AimAssistState::target_dir`] while the
    /// same target is held, which is what stops the pull chattering as the
    /// intercept point jitters. `main.js:2120` (the `12` in
    /// `1 - Math.exp(-12 * dt)`).
    pub dir_track_rate: f64,
    /// Below this smoothed strength the pull is skipped entirely.
    /// `main.js:2129`.
    pub engage_epsilon: f64,
    /// Tuning for mouse pilots.
    pub precise: AimAssistTuning,
    /// Tuning for keyboard/touch pilots.
    pub coarse: AimAssistTuning,
}

impl AimAssistRules {
    /// The frozen default.
    pub const DEFAULT: Self = Self {
        min_range: 0.0,
        range: 1000.0,
        strength_damp_rate: 6.0,
        dir_track_rate: 12.0,
        engage_epsilon: 0.01,
        precise: AimAssistTuning {
            cone_dot: 0.60,
            sticky_dot_bonus: 0.05,
            strength: 2.6,
            falloff_start: 0.28,
            dead_angle: 0.005,
            intent_break: 1.8,
        },
        coarse: AimAssistTuning {
            cone_dot: 0.5,
            sticky_dot_bonus: 0.05,
            strength: 2.2,
            falloff_start: 0.30,
            dead_angle: 0.0,
            intent_break: 0.25,
        },
    };

    /// The tuning for `profile`.
    #[must_use]
    pub fn tuning(&self, profile: AimProfile) -> &AimAssistTuning {
        match profile {
            AimProfile::Precise => &self.precise,
            AimProfile::Coarse => &self.coarse,
        }
    }
}

// ---------------------------------------------------------------------------
// Ship
// ---------------------------------------------------------------------------

/// Hull, flight model, and boost. Everything that describes a ship as a moving
/// object rather than as a shooter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShipRules {
    /// Starting and maximum hit points. `main.js:570` and
    /// `server/index.js:424` — the two copies agree.
    pub max_hp: i32,

    /// Radius of the ship's collision sphere against asteroids, the moon, and
    /// mothership boxes.
    ///
    /// **Divergence resolved.** `main.js:980` derives `2.2 * SHIP_SCALE` = 3.3
    /// for the local player; `bot.js:31` hard-codes `SHIP_RADIUS = 3.5` for
    /// bots. Unified on **3.3**, the player's value: it is the one derived from
    /// the actual model scale, and it is what the visible ship has always used.
    /// Bots become 6 % less likely to clip a rock.
    pub collide_radius: f64,

    /// Radius of the sphere a *weapon* must reach to count as a hit on this
    /// ship.
    ///
    /// **Divergence resolved — five values became one.** The JS has:
    ///
    /// | Weapon | Radius | Site |
    /// |---|---|---|
    /// | Player bullet | 6.0 (7.0 coarse-aim) | `main.js:406` into `bullets.js:74` |
    /// | Player beam | 5.5 | `main.js:1040` (`BEAM_SHIP_RADIUS`) |
    /// | Missile | 6.0 | `missiles.js:5` (`HIT_RADIUS`) |
    /// | Bot bullet | 4.0 | `bot.js:31`+`:52` (`SHIP_RADIUS + 0.5`) |
    /// | Boss bullet | 7.0 | `main.js:2769` (`PLAYER_HIT_R`) |
    ///
    /// Unified on **6.0**: it is what the two highest-traffic weapons (player
    /// bullets and missiles) already used, so the common case does not change.
    /// The consequences of the change are deliberate:
    ///
    /// - Bots go from 4.0 to 6.0. Today a bot must close ~35 % nearer than a
    ///   player to land the same shot with the same geometry, which reads as
    ///   the bots being bad at aiming when they are actually being cheated.
    /// - Beams go from 5.5 to 6.0. Marginally more forgiving on the weapon that
    ///   is already hitscan, but it pays 3 ammo per shot against a bullet's 1.
    /// - Boss bullets go from 7.0 to 6.0. Compensate with
    ///   [`WeaponRules::boss_bullet_damage`] if the fight gets easy.
    ///
    /// This is the *target* radius. A projectile's own radius
    /// ([`WeaponRules::bullet_radius`]) is added on top, which is what the JS
    /// does at `bullets.js:144`.
    pub hit_radius: f64,

    /// Extra hit radius granted when the *shooter* is on
    /// [`AimProfile::Coarse`].
    ///
    /// `main.js:406` passes `coarseAim ? 7.0 : 6.0`. This is the one hit-radius
    /// difference worth keeping: a keyboard or touch pilot cannot make the fine
    /// corrections a mouse pilot can, and widening the target is the standard
    /// accessibility answer. Keeping it as a named bonus rather than a second
    /// radius makes it obvious that it is a deliberate exception, and makes it
    /// one edit to remove if it ever becomes a competitive complaint.
    ///
    /// It keys off the shooter, never the target, so it cannot be farmed by
    /// switching your own control scheme.
    pub hit_radius_coarse_aim_bonus: f64,

    /// Hit radius *removed* when the shooter is a bot.
    ///
    /// **This reverses a decision made above.** [`Self::hit_radius`] unified
    /// five JS radii on 6.0, and its comment argued that the bot's 4.0
    /// (`bot.js:31`+`:52`, `SHIP_RADIUS + 0.5`) made bots "read as bad at
    /// aiming when they are actually being cheated". That reading was wrong in
    /// the way that matters: the 4.0 *was* the difficulty setting. A 6.0 sphere
    /// against a 4.0 one is 2.25x the cross-section, so the port's bots landed
    /// shots the JS's would have missed from identical geometry, and they
    /// played as markedly harder than the game they are a port of.
    ///
    /// Restoring it as a shooter-keyed penalty rather than by lowering
    /// [`Self::hit_radius`] keeps every player weapon at 6.0 and keeps the
    /// "one function decides a hit radius" property — it is the same shape as
    /// [`Self::hit_radius_coarse_aim_bonus`], applied at the same place, and
    /// varying it is how a difficulty tier would be expressed.
    ///
    /// 2.0 puts a bot back on the JS's 4.0. Zero makes bots shoot exactly as
    /// well as players.
    pub hit_radius_bot_penalty: f64,

    /// Top speed at full throttle, before boost. `main.js:982`
    /// (`MAX_THROTTLE`).
    pub max_throttle: f64,
    /// Speed multiplier while boosting. `main.js:983` (`BOOST_FACTOR`).
    pub boost_factor: f64,
    /// Throttle change per mouse-wheel notch. `main.js:984` (`THROTTLE_STEP`).
    pub throttle_step: f64,
    /// Throttle change per second while W/S is held. `main.js:985`
    /// (`KEY_THROTTLE_RATE`).
    pub key_throttle_rate: f64,
    /// Exponential rate at which actual throttle chases target throttle.
    /// `main.js:1217`, where it is an unnamed literal `3` inside
    /// `THREE.MathUtils.damp`.
    pub throttle_damp_rate: f64,

    /// Pitch rate. `main.js:986` (`PITCH_RATE`).
    pub pitch_rate: f64,
    /// Extra pitch authority when pulling *up* only. `main.js:987`
    /// (`PITCH_UP_BOOST`) — nose-up is the evasive input, so it is faster.
    pub pitch_up_boost: f64,
    /// Yaw rate. `main.js:988` (`YAW_RATE`).
    pub yaw_rate: f64,
    /// Roll rate. `main.js:989` (`ROLL_RATE`).
    pub roll_rate: f64,
    /// Pitch multiplier while braking. `main.js:1009` (`BRAKE_PITCH_MULT`).
    pub brake_pitch_mult: f64,
    /// Yaw multiplier while braking. `main.js:1010` (`BRAKE_YAW_MULT`).
    pub brake_yaw_mult: f64,

    /// Stick/mouse input below this magnitude is treated as zero.
    /// `main.js:991` (`STEER_DEADZONE`).
    pub steer_deadzone: f64,
    /// Response curve exponent applied to steering input. `main.js:1227`,
    /// where it is an unnamed literal `1.6` inside `Math.pow`.
    pub steer_curve_exponent: f64,
    /// Rate at which held arrow keys ramp toward full deflection.
    /// `main.js:993` (`ARROW_RAMP_UP_RATE`).
    pub arrow_ramp_up_rate: f64,
    /// Ramp rate with the fine-aim modifier (Q) held. `main.js:994`
    /// (`ARROW_RAMP_UP_RATE_FINE`).
    pub arrow_ramp_up_rate_fine: f64,
    /// Rate at which released arrow keys ramp back to centre. `main.js:995`
    /// (`ARROW_RAMP_DOWN_RATE`).
    pub arrow_ramp_down_rate: f64,

    /// Velocity-toward-facing blend rate under thrust. `main.js:990`
    /// (`VELOCITY_BLEND`), used as `1 - 0.001^(dt * k / 6)` at `main.js:1294`.
    pub velocity_blend: f64,
    /// The same blend during a brake-release boost, deliberately slacker so the
    /// boost feels like a slingshot. `main.js:1018` (`VELOCITY_BLEND_RELEASE`).
    pub velocity_blend_release: f64,
    /// Per-second velocity retention while drifting. `main.js:1015`
    /// (`DRIFT_DRAG`), applied as `vel *= drag^dt`.
    pub drift_drag: f64,
    /// How hard a drift pulls velocity back onto the nose. `main.js:1016`
    /// (`DRIFT_GRIP`).
    pub drift_grip: f64,
    /// Velocity retention while drifting *and* holding S — the hard stop.
    /// `main.js:1017` (`DRIFT_BRAKE`).
    pub drift_brake: f64,

    /// Time on the brake to reach a full charge. `main.js:1011`
    /// (`BRAKE_FULL_TIME`).
    pub brake_full_time: f64,
    /// Minimum charge that still yields a release boost. `main.js:1012`
    /// (`BRAKE_BOOST_MIN`).
    pub brake_boost_min: f64,
    /// Release-boost duration at full charge. `main.js:1013`
    /// (`BRAKE_BOOST_DURATION_MAX`).
    pub brake_boost_duration_max: f64,
    /// Extra speed added along the nose at full charge. `main.js:1014`
    /// (`BRAKE_BOOST_BONUS_MAX`).
    pub brake_boost_bonus_max: f64,
    /// Time held at full charge before the HUD warns. `main.js:1019`
    /// (`BRAKE_OVERCHARGE_WARN`). Purely a HUD threshold; kept here so the
    /// simulation can report it rather than the HUD re-deriving it.
    pub brake_overcharge_warn: f64,
    /// Time held at full charge before self-damage begins. `main.js:1020`
    /// (`BRAKE_OVERCHARGE_DAMAGE`).
    pub brake_overcharge_damage_delay: f64,
    /// Self-damage per second once overcharged. `main.js:1021`
    /// (`BRAKE_OVERCHARGE_DPS`). Accumulated fractionally and spent in whole
    /// points (`main.js:1316`), so the damage stream is integral.
    pub brake_overcharge_dps: f64,

    /// Boost meter capacity, in seconds of boost. `main.js:1108`
    /// (`MAX_BOOST`).
    pub max_boost: f64,
    /// Boost drain per second while boosting. `main.js:1109` (`BOOST_DRAIN`).
    pub boost_drain: f64,
    /// Boost recharge per second. `main.js:1110` (`BOOST_RECHARGE`).
    pub boost_recharge: f64,
    /// Idle time before boost starts recharging. `main.js:1111`
    /// (`BOOST_REGEN_DELAY`).
    pub boost_regen_delay: f64,
}

impl ShipRules {
    /// The frozen default.
    pub const DEFAULT: Self = Self {
        max_hp: 100,
        collide_radius: 3.3,
        hit_radius: 6.0,
        hit_radius_coarse_aim_bonus: 1.0,
        hit_radius_bot_penalty: 2.0,

        max_throttle: 80.0,
        boost_factor: 1.7,
        throttle_step: 6.0,
        key_throttle_rate: 30.0,
        throttle_damp_rate: 3.0,

        pitch_rate: 1.75,
        pitch_up_boost: 1.25,
        yaw_rate: 1.3,
        roll_rate: 1.4,
        brake_pitch_mult: 1.3,
        brake_yaw_mult: 1.7,

        steer_deadzone: 0.05,
        steer_curve_exponent: 1.6,
        arrow_ramp_up_rate: 3.0,
        arrow_ramp_up_rate_fine: 1.5,
        arrow_ramp_down_rate: 12.0,

        velocity_blend: 4.0,
        velocity_blend_release: 1.5,
        drift_drag: 0.9,
        drift_grip: 0.3,
        drift_brake: 0.1,

        brake_full_time: 1.4,
        brake_boost_min: 0.18,
        brake_boost_duration_max: 1.0,
        brake_boost_bonus_max: 50.0,
        brake_overcharge_warn: 1.0,
        brake_overcharge_damage_delay: 2.0,
        brake_overcharge_dps: 10.0,

        max_boost: 10.0,
        boost_drain: 2.0,
        boost_recharge: 4.0,
        boost_regen_delay: 1.0,
    };

    /// Hit radius a shooter on `profile` sees on any target ship.
    ///
    /// The bonus is a property of the shooter, never of the target.
    #[must_use]
    pub fn hit_radius_for(&self, profile: AimProfile) -> f64 {
        match profile {
            AimProfile::Precise => self.hit_radius,
            AimProfile::Coarse => self.hit_radius + self.hit_radius_coarse_aim_bonus,
        }
    }
}

// ---------------------------------------------------------------------------
// Weapons
// ---------------------------------------------------------------------------

/// Guns, missiles, flares, and the boss's turrets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeaponRules {
    /// Seconds between bullet shots. `main.js:1030` (`BULLET_COOLDOWN`).
    pub bullet_cooldown: f64,
    /// Seconds between beam shots. `main.js:1031` (`BEAM_COOLDOWN`).
    pub beam_cooldown: f64,
    /// Ammo spent per bullet. `main.js:1461` (`ammoCost`, the `: 1` branch).
    pub bullet_ammo_cost: f64,
    /// Ammo spent per beam. `main.js:1461` (`ammoCost`, the `? 3` branch).
    pub beam_ammo_cost: f64,
    /// Ammo capacity. `main.js:1087` (`MAX_AMMO`).
    pub max_ammo: f64,
    /// Ammo regenerated per second. `main.js:1088` (`AMMO_REGEN`).
    pub ammo_regen: f64,
    /// Idle time before ammo starts regenerating. `main.js:1086`
    /// (`REGEN_DELAY`).
    pub ammo_regen_delay: f64,

    /// Damage from one bullet or beam hit.
    ///
    /// `main.js:1497`/`:1632` and `server/index.js:945` — both 10, and the
    /// server comment records that bullets were deliberately buffed to match
    /// the beam so arrow-key pilots are not punished. Bot bullets also do 10
    /// (`bot.js:20`).
    pub gun_damage: i32,
    /// Damage from one missile hit. `main.js:1655` and
    /// `server/index.js:945` — both 50.
    pub missile_damage: i32,
    /// Damage from one boss turret round. `main.js:2770`
    /// (`BOSS_BULLET_DMG`). No server counterpart — the campaign is solo-only.
    pub boss_bullet_damage: i32,

    /// Bullet muzzle velocity.
    ///
    /// Declared twice today with the same value: `bullets.js:2`
    /// (`BULLET_SPEED`, exported) and `bot.js:26` (re-declared, not imported).
    /// Nothing enforced the match.
    pub bullet_speed: f64,
    /// Bullet lifetime. `bullets.js:7` (`LIFE`) and `bot.js:27`
    /// (`BULLET_LIFE`) — again the same value declared twice.
    pub bullet_life: f64,
    /// Bullet collision radius, added to whatever it is testing against.
    /// `bullets.js:8` (`RADIUS`).
    pub bullet_radius: f64,
    /// Muzzle offset from the ship origin, in ship local space.
    /// `main.js:1032` (`MUZZLE_OFFSETS`, a single-element array).
    pub muzzle_offset: Vec3,
    /// Maximum beam length. `main.js:1039` (`BEAM_RANGE`).
    pub beam_range: f64,
    /// How far ahead of the muzzle the beam *visual* starts, so it does not
    /// clip the cockpit. `main.js:1041` (`BEAM_FORWARD_OFFSET`). Visual only;
    /// the hit test starts at the muzzle.
    pub beam_forward_offset: f64,

    /// Missile cruise speed. `missiles.js:2` (`MISSILE_SPEED`).
    pub missile_speed: f64,
    /// Maximum missile turn rate. `missiles.js:3` (`TURN_RATE`).
    pub missile_turn_rate: f64,
    /// Missile lifetime before self-detonation. `missiles.js:4` (`LIFE`).
    pub missile_life: f64,
    /// Missile collision radius, added to a target's [`ShipRules::hit_radius`].
    ///
    /// `missiles.js` adds no projectile radius at all (`:402` compares against
    /// `HIT_RADIUS` alone), unlike `bullets.js:144` which adds its `RADIUS`.
    /// Kept at 0.0 to preserve current behaviour.
    ///
    // TODO(verify): a missile is a 3.5-long body (`missiles.js:17 BODY_LEN`)
    // with 0.28 radius, so 0.0 is almost certainly an oversight rather than a
    // decision. Raising it to ~0.5 would make missiles marginally easier to
    // land; that is a balance call, not a port call.
    pub missile_radius: f64,
    /// Missiles carried. `main.js:1091` (`MISSILE_MAX`).
    pub missile_max: u8,
    /// How far ahead of the ship a missile spawns. `main.js:1427` and
    /// `main.js:2504`, both `addScaledVector(fwd, 6)`.
    pub missile_spawn_offset: f64,
    /// Minimum lookahead distance for missile obstacle avoidance.
    /// `missiles.js:6` (`AVOID_BASE_LOOKAHEAD`).
    pub missile_avoid_base_lookahead: f64,
    /// Lookahead is at least `obstacle_radius * this`. `missiles.js:7`
    /// (`AVOID_RADIUS_SCALE`).
    pub missile_avoid_radius_scale: f64,
    /// Clearance a missile tries to keep from an obstacle surface.
    /// `missiles.js:8` (`AVOID_MARGIN`).
    pub missile_avoid_margin: f64,
    /// The missile's own radius for avoidance purposes. `missiles.js:9`
    /// (`MISSILE_AVOID_R`). Distinct from [`Self::missile_radius`], which is
    /// the damage test.
    pub missile_avoid_self_radius: f64,
    /// Weight of the avoidance vector against the homing vector.
    /// `missiles.js:10` (`AVOID_WEIGHT`).
    pub missile_avoid_weight: f64,
    /// Extra margin at which a missile detonates on an obstacle rather than
    /// entering it. `missiles.js:11` (`DETONATE_MARGIN`).
    pub missile_detonate_margin: f64,

    /// Flare charges carried. `main.js:1100` (`FLARE_MAX`).
    pub flare_max: u8,
    /// Flares released per charge. `missiles.js:14` (`FLARE_COUNT`).
    pub flare_count: u32,
    /// Base flare ejection speed. `missiles.js:12` (`FLARE_SPEED`).
    pub flare_speed: f64,
    /// Flare burn time. `missiles.js:13` (`FLARE_LIFE`).
    pub flare_life: f64,
    /// Low end of the per-flare speed multiplier. `missiles.js:247`
    /// (`0.65 + random() * 0.70`).
    pub flare_speed_jitter_min: f64,
    /// Width of the per-flare speed multiplier range. `missiles.js:247`.
    pub flare_speed_jitter_range: f64,
    /// Per-second velocity retention for a coasting flare, applied as
    /// `vel *= drag^dt`. `missiles.js:467`.
    pub flare_drag: f64,
    /// Range within which a missile abandons its target for a flare.
    /// `missiles.js:15` (`FLARE_SEDUCTION_DIST`). A missile ignores flares from
    /// its own owner (`missiles.js:321`).
    pub flare_seduction_dist: f64,

    /// Boss turret round speed. `main.js:2688`.
    pub boss_bullet_speed: f64,
    /// Boss turret round lifetime. `main.js:2688`.
    pub boss_bullet_life: f64,

    /// Radius of one boss hitbox, for **every** weapon.
    ///
    /// **Divergence resolved — three values became one.** The JS boss answers
    /// to three different radii depending on what is shooting it:
    ///
    /// - Bullets honour the record's `hitRadius: 28` (`main.js:2945`, read at
    ///   `bullets.js:144`).
    /// - Missiles ignore `hitRadius` entirely and use their own `HIT_RADIUS` of
    ///   6.0 (`missiles.js:402`), so the highest-damage weapon in the game must
    ///   pass within 6 units of a hitbox point instead of 28 — and mostly
    ///   misses.
    /// - The beam takes a separate path altogether: a single sphere of radius
    ///   **95** at the capital ship centre (`main.js:1476`–`:1479`), so beams
    ///   hit trivially from any angle.
    ///
    /// Unified on **28** for all three. Bullets are unchanged, missiles become
    /// usable against the boss (the point of the fix), and the beam loses its
    /// free hull-wide sphere.
    ///
    // TODO(verify): with the 95-radius proxy gone, weapons must hit one of the
    // 20 spheres in `BOSS_HITBOX_OFFSETS`. Those are 75 units apart in z
    // against a diameter of 56, so a shot can pass between rows and miss a
    // capital ship it is visually inside. That is a hitbox-layout problem, not
    // a rules problem, but it needs a playtest before this ships — the fix is
    // more offsets, not a second radius.
    pub boss_hitbox_radius: f64,
}

impl WeaponRules {
    /// The frozen default.
    pub const DEFAULT: Self = Self {
        bullet_cooldown: 0.05,
        beam_cooldown: 0.25,
        bullet_ammo_cost: 1.0,
        beam_ammo_cost: 3.0,
        max_ammo: 90.0,
        ammo_regen: 36.0,
        ammo_regen_delay: 1.0,

        gun_damage: 10,
        missile_damage: 50,
        boss_bullet_damage: 14,

        bullet_speed: 780.0,
        bullet_life: 2.0,
        bullet_radius: 0.5,
        muzzle_offset: Vec3::new(0.0, 0.0, 0.6),
        beam_range: 1000.0,
        beam_forward_offset: 4.0,

        missile_speed: 160.0,
        missile_turn_rate: 1.4,
        missile_life: 8.0,
        missile_radius: 0.0,
        missile_max: 4,
        missile_spawn_offset: 6.0,
        missile_avoid_base_lookahead: 130.0,
        missile_avoid_radius_scale: 3.2,
        missile_avoid_margin: 4.0,
        missile_avoid_self_radius: 2.5,
        missile_avoid_weight: 4.0,
        missile_detonate_margin: 2.0,

        flare_max: 3,
        flare_count: 20,
        flare_speed: 140.0,
        flare_life: 1.8,
        flare_speed_jitter_min: 0.65,
        flare_speed_jitter_range: 0.70,
        flare_drag: 0.22,
        flare_seduction_dist: 180.0,

        boss_bullet_speed: 430.0,
        boss_bullet_life: 4.2,
        boss_hitbox_radius: 28.0,
    };
}

// ---------------------------------------------------------------------------
// Damage, death, respawn
// ---------------------------------------------------------------------------

/// Damage application, death, respawn, and health regeneration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CombatRules {
    /// Seconds between death and respawn.
    ///
    /// **Divergence resolved.** `main.js:571` says `RESPAWN_DELAY = 2.5`
    /// (used for solo bots at `main.js:3207` and the solo player at
    /// `main.js:3258`); `server/index.js:425` says `RESPAWN_DELAY_MS = 2000`
    /// (used at `:896` and `:957`). Solo and campaign respawn 25 % slower than
    /// multiplayer, and nothing in the HUD reconciles them — the death banner
    /// shows no timer.
    ///
    /// Unified on **2.0 s**, the server's value, for two reasons. First, the
    /// server is authoritative in multiplayer: choosing 2.5 would mean changing
    /// behaviour that live clients already observe and time against, whereas
    /// choosing 2.0 only speeds up solo, which no player can compare against
    /// anything. Second, 2.0 is the value that is already load-bearing
    /// elsewhere — it equals [`Self::spawn_invuln`], so invulnerability expires
    /// exactly as the next death could occur.
    pub respawn_delay: f64,

    /// Respawn delay in campaign, which uses a warp-in effect instead of a
    /// plain wait. `main.js:3253`, paired with the 1.5 s warp flash at
    /// `main.js:3252`.
    pub campaign_respawn_delay: f64,

    /// Spawn protection window, applied on match start and every respawn.
    ///
    /// **Divergence resolved — not in the value, in who gets it.** The number
    /// agrees: `main.js:572` (`SPAWN_INVULN_DURATION = 2.0`) and
    /// `server/index.js:741`/`:751`/`:893`/`:954` (`Date.now() + 2000`). What
    /// disagrees is coverage. The server gates every hit on
    /// `target.invulnUntil` (`server/index.js:942`). In solo,
    /// `applyPlayerDamageLocal` checks `myInvulnTimer` (`main.js:3232`) but
    /// `applyHitToBot` checks only `!r.alive` (`main.js:3201`) — bot records
    /// never get an invuln timer at all (`spawnBot`, `main.js:2471`, sets
    /// `hp`, `alive` and `respawnTimer` and nothing else). A freshly respawned
    /// solo bot can be killed on the spot at its spawn anchor; the player
    /// cannot.
    ///
    /// **This rule is universal.** Every ship — local, remote, bot, and boss
    /// hitbox — carries an invulnerability timer, and every damage path checks
    /// it. Asymmetric spawn protection is not a difficulty setting.
    pub spawn_invuln: f64,

    /// Time since the last damage *and* the last shot fired before health
    /// regenerates. `main.js:1114` (`HEALTH_REGEN_DELAY`); both clocks must
    /// clear (`main.js:1535`).
    pub health_regen_delay: f64,
    /// Interval between regenerated hit points. `main.js:1115`
    /// (`HEALTH_REGEN_INTERVAL`).
    pub health_regen_interval: f64,
    /// Hit points restored per interval. `main.js:1539` (`myHp + 1`).
    pub health_regen_amount: i32,

    /// Hit points an asteroid loses per weapon impact, regardless of weapon.
    /// `main.js:2183` (`damageAsteroidLocal`) and `server/index.js:822` — both
    /// 1. A 50-damage missile and a 10-damage bullet chip a rock identically.
    pub asteroid_damage_per_hit: i32,
    /// Least damage from flying into an asteroid. `main.js:2215`
    /// (`15 + floor(random() * 15)`), inclusive.
    pub asteroid_collision_damage_min: i32,
    /// Greatest damage from flying into an asteroid. Same site, inclusive —
    /// the JS expression yields 15..=29, never 30.
    pub asteroid_collision_damage_max: i32,

    /// Least time between two asteroid collision charges, in seconds.
    ///
    /// **Not in the JS**, which charges on every rising edge of contact
    /// (`main.js:2215`). That reads fine for one clean hit and badly for
    /// everything else: `collision_restitution` bounces you off a rock and the
    /// throttle carries you straight back into it, so each bounce is a fresh
    /// rising edge and a fresh 15..=29. Rocks overlap freely — nothing in
    /// `asteroids::populate` separates them — so a knot of them charges once
    /// per rock as well. Measured at 49 of 100 hit points for one pass through
    /// four stacked rocks, and lethal for a denser knot, which is why flying
    /// into an asteroid field read as an instant death rather than as a dent.
    ///
    /// One second bounds it to one charge per second however the contact
    /// behaves, leaving a clean single impact exactly as it was.
    pub asteroid_collision_damage_cooldown: f64,

    /// Restitution when a ship is pushed out of a sphere (asteroid or moon).
    /// `main.js:2209` and `bot.js:228`, both the literal `1.3`. Greater than 1,
    /// so a collision adds energy — that is intentional, it makes rocks feel
    /// like they kick.
    pub collision_restitution: f64,
    /// Restitution when a ship is pushed out of a box (mothership, airfield).
    /// `main.js:2123` (`collideSphereWithBox`), the literal `1.4`.
    pub box_collision_restitution: f64,
    /// Vertical velocity multiplier when a ship is stopped by terrain.
    /// `main.js:2254` (`shipVelocity.y *= -0.5`).
    pub terrain_bounce: f64,

    /// Minimum interval between accepted bullet/beam hit reports from one
    /// shooter. `server/index.js:932` (40 ms). Anti-spam on an otherwise
    /// fully client-trusted damage path; kept in the rules so a Rust server
    /// enforces the same window.
    pub gun_hit_min_interval: f64,
    /// Minimum interval between accepted missile hit reports from one shooter.
    /// `server/index.js:932` (400 ms).
    pub missile_hit_min_interval: f64,
}

impl CombatRules {
    /// The frozen default.
    pub const DEFAULT: Self = Self {
        respawn_delay: 2.0,
        campaign_respawn_delay: 1.5,
        spawn_invuln: 2.0,

        health_regen_delay: 2.0,
        health_regen_interval: 0.1,
        health_regen_amount: 1,

        asteroid_damage_per_hit: 1,
        asteroid_collision_damage_min: 15,
        asteroid_collision_damage_max: 29,
        asteroid_collision_damage_cooldown: 1.0,

        collision_restitution: 1.3,
        box_collision_restitution: 1.4,
        terrain_bounce: -0.5,

        gun_hit_min_interval: 0.04,
        missile_hit_min_interval: 0.4,
    };
}

// ---------------------------------------------------------------------------
// Static world geometry
// ---------------------------------------------------------------------------

/// The shape of a rock, as one description both the renderer and the
/// simulation read.
///
/// A rock is drawn as a unit icosphere whose every vertex is pushed along its
/// own radius by two octaves of noise (`asteroids.js:37`, ported to
/// `client/src/scene.rs::rock_mesh`):
///
/// ```text
/// lobe   = lobe_base + lobe_amp * noise(v * lobe_freq, seed)
/// bump   = bump_base + bump_amp * noise(v * bump_freq, seed + bump_seed_offset)
/// radius = size * lobe * bump
/// ```
///
/// with each `noise` in `[-1, 1)`. It is a *hash*, not a field — two vertices a
/// tenth of a unit apart land in unrelated parts of it — so the two octaves are
/// independent per vertex and the surface is faceted rather than lumpy. See
/// `pseudo_noise` in `scene.rs` for why that discontinuity is the point.
///
/// # Why the simulation has an opinion about a mesh
///
/// It does not draw one. It needs the *statistics*: the collision sphere is
/// [`AsteroidFieldRules::collision_radius_scale`] of a rock's `size`, and that
/// number is only meaningful next to the surface it is standing in for. When
/// the two were written down separately they disagreed by a quarter of a rock —
/// see the note on that field. So the displacement lives here, once, and both
/// sides read it: the renderer to build the mesh, the simulation to size the
/// sphere.
///
/// Nothing here is on a simulation path. [`Self::mean_radius_scale`] and its
/// siblings are `+ - *` on constants, evaluated at compile time; the `sin` that
/// `noise` is built from stays in the renderer, where the determinism ban does
/// not reach.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AsteroidMeshRules {
    /// Mean of the coarse octave — the one that makes a rock lumpy rather than
    /// round. `asteroids.js:37` (`0.78 + 0.34 * n`).
    pub lobe_base: f64,
    /// Half-range of the coarse octave.
    pub lobe_amp: f64,
    /// Coordinate scale the coarse octave is sampled at.
    pub lobe_freq: f64,
    /// Mean of the fine octave — surface roughness. `asteroids.js:38`
    /// (`0.94 + 0.12 * n`).
    pub bump_base: f64,
    /// Half-range of the fine octave.
    pub bump_amp: f64,
    /// Coordinate scale the fine octave is sampled at.
    pub bump_freq: f64,
    /// Added to the variant index for the fine octave's seed, so the two
    /// octaves of one rock are uncorrelated. `asteroids.js:38` (`v + 7`).
    pub bump_seed_offset: f64,
    /// Icosphere subdivision level the displacement is applied to.
    ///
    /// Render-side, and listed here because it belongs with the rest of the
    /// description: the noise is per *vertex*, so the subdivision is what sets
    /// the size of a facet. `asteroids.js:31` builds `IcosahedronGeometry(1, 2)`.
    pub subdivisions: u32,
}

impl AsteroidMeshRules {
    /// The frozen default. `asteroids.js:37`–`:38`, digit for digit.
    pub const DEFAULT: Self = Self {
        lobe_base: 0.78,
        lobe_amp: 0.34,
        lobe_freq: 1.3,
        bump_base: 0.94,
        bump_amp: 0.12,
        bump_freq: 4.1,
        bump_seed_offset: 7.0,
        subdivisions: 2,
    };

    /// Mean displaced radius, as a fraction of a rock's `size`.
    ///
    /// The two octaves are independent and each noise term is symmetric about
    /// zero, so the mean of the product is the product of the means and the
    /// amplitudes drop out entirely: `0.78 * 0.94 = 0.7332`.
    ///
    /// This is the sphere that splits the difference — half the drawn surface
    /// inside it, half outside — which is why it is what
    /// [`AsteroidFieldRules::collision_radius_scale`] is set to.
    #[must_use]
    pub const fn mean_radius_scale(self) -> f64 {
        self.lobe_base * self.bump_base
    }

    /// Smallest displaced radius the description can produce, as a fraction of
    /// `size`: both octaves at their trough.
    #[must_use]
    pub const fn min_radius_scale(self) -> f64 {
        (self.lobe_base - self.lobe_amp) * (self.bump_base - self.bump_amp)
    }

    /// Largest displaced radius the description can produce, as a fraction of
    /// `size`: both octaves at their peak. 1.187 with the shipped numbers, so a
    /// spike reaches a fifth of a radius beyond the nominal size.
    #[must_use]
    pub const fn max_radius_scale(self) -> f64 {
        (self.lobe_base + self.lobe_amp) * (self.bump_base + self.bump_amp)
    }
}

/// Asteroid field generation.
///
/// One generator replaces three: `asteroids.js:110` (`createAsteroidField`,
/// client solo), `server/index.js:553` (`generateAsteroidField`, multiplayer)
/// and `main.js:237` (`genCampaignAsteroids`, campaign zones).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AsteroidFieldRules {
    /// Rocks in a standard field. `main.js:278` and `server/index.js:766` —
    /// both 60.
    pub count: u32,
    /// Outer radius of the field. `main.js:278` and `server/index.js:766` —
    /// both 400.
    pub radius: f64,
    /// Rocks are placed no nearer the origin than `min_dist_base + size`.
    /// `asteroids.js:160` and `server/index.js:565` — both 30.
    pub min_dist_base: f64,
    /// The random direction's `y` component is scaled by this, flattening the
    /// field into a disc. `asteroids.js:157` and `server/index.js:561` — both
    /// 0.4.
    pub y_flatten: f64,
    /// Placement attempts before giving up and falling back to a flat random
    /// scatter. `asteroids.js:154` and `server/index.js:559` — both 10.
    pub place_attempts: u32,
    /// Clearance added to a rock's size when testing an avoidance volume.
    /// `asteroids.js:116` and `server/index.js:543` — both 6.
    pub avoid_margin: f64,
    /// Collision radius as a fraction of a rock's `size`.
    ///
    /// **Divergence resolved — this is what "the hitboxes are not accurate"
    /// was.** Both JS generators write `size * 0.95` (`asteroids.js:93` and
    /// `:184`), and 0.95 is a plausible-looking number for a sphere standing in
    /// for a rock — until you measure it against the rock that is actually
    /// drawn. [`AsteroidMeshRules`] displaces every vertex to `lobe * bump` of
    /// the nominal radius, which averages
    /// [`AsteroidMeshRules::mean_radius_scale`] = **0.7332** and never exceeds
    /// [`AsteroidMeshRules::max_radius_scale`] = 1.187.
    ///
    /// So the 0.95 sphere sat 0.217 of a `size` *outside* the mean drawn
    /// surface — 22 % of the sphere's radius, and 12 units of invisible wall on
    /// a 55-unit rock. A ship "hit" a rock it was clearly a ship's width away
    /// from, and a bullet died in the gap. That is the whole complaint, and it
    /// is measurable rather than a matter of taste:
    /// `the_collision_sphere_is_the_drawn_rocks_mean_radius` in
    /// [`crate::asteroids`] integrates the displacement and pins it.
    ///
    /// Set to the mesh's **mean** radius, which is the sphere with as much
    /// drawn rock outside it as empty space inside it. Not the max (1.187 would
    /// be a hitbox three times the rock's volume, all of it invisible) and not
    /// the min (0.36 would let a ship fly through the middle of a rock). A
    /// single sphere cannot describe a faceted surface that ranges over ±25 %
    /// of its own radius; it can be centred on it, and that is what this is.
    ///
    /// Derived rather than written down, so that changing the mesh moves the
    /// hitbox with it and the two cannot drift apart again.
    pub collision_radius_scale: f64,
    /// The displacement that turns a unit icosphere into a rock, which is where
    /// [`Self::collision_radius_scale`] comes from and what the renderer builds
    /// its meshes with.
    pub mesh: AsteroidMeshRules,
    /// Number of deformed-icosahedron meshes a rock can pick from. The index
    /// is simulation state (it must match across clients); the meshes
    /// themselves are render-side. `asteroids.js:126` and
    /// `server/index.js:580` — both 6.
    pub variant_count: u32,

    /// Whether the moon is an avoidance volume during generation.
    ///
    /// **Divergence resolved — this is a live multiplayer bug.** The client
    /// avoids the moon: `main.js:185` builds `moonAvoid` with half-size
    /// `(80, 80, 80)`, `main.js:236` folds it into `_avoidList`, and
    /// `asteroids.js:114` (`clipsAvoidance`) rejects those placements. The
    /// server's `clipsMothership` (`server/index.js:541`) checks only the two
    /// motherships; there is no `MOON_AVOID`. Meanwhile
    /// `server/index.js:565`–`:558` places rocks between `30 + size` and 400
    /// units *from the origin*, and the moon is at the origin with radius 80
    /// (`main.js:181`).
    ///
    /// So in every multiplayer space match, asteroids are generated inside the
    /// moon. Bullets vanish into invisible rocks (`bullets.js:96` runs before
    /// the obstacle test at `:122`), missiles detonate on entry
    /// (`missiles.js:355`), and a player who clips a buried rock eats both the
    /// 15–29 collision damage (`main.js:2215`) *and* the moon's instant kill
    /// (`main.js:2244`).
    ///
    /// **The unified rule includes the moon.** Set `true` and keep it true.
    pub avoid_moon: bool,

    /// Rock counts for trials 1–4, in order. `main.js:232`
    /// (`_trialRockCount`): 120 / 150 / 180 / 210. Trials use a denser field
    /// as the course number rises.
    pub trials_counts: [u32; 4],
}

/// A campaign asteroid zone: a box the generator fills with rocks.
///
/// `main.js:240` (`ZONES`). Campaign fields are laid out as three slabs along
/// the flight path rather than a sphere around the origin, because the mission
/// is a corridor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AsteroidZone {
    /// Near edge of the slab.
    pub z_min: f64,
    /// Far edge of the slab.
    pub z_max: f64,
    /// Rocks placed in this slab.
    pub count: u32,
    /// Half-width in x; positions are drawn from `[-x_range, x_range)`.
    pub x_range: f64,
    /// Half-height in y.
    pub y_range: f64,
}

/// Fixed world geometry: the moon, the two capital platforms, and the terrain.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldRules {
    /// Moon radius, used both as a collision sphere and as the instant-death
    /// surface. `main.js:181` (`MOON_RADIUS`) into `moon.js:11`.
    pub moon_radius: f64,
    /// Moon centre. `main.js:182` places it at the origin.
    pub moon_pos: Vec3,
    /// Half-extents of the *box* the asteroid generator avoids around the moon.
    /// `main.js:186` — a cube circumscribing the sphere, not the sphere itself.
    /// Kept as a box because that is the shape `clipsAvoidance` tests.
    pub moon_avoid_half: Vec3,

    /// Mothership box half-extents. `main.js:140` (`MOTHERSHIP_HALF`) and
    /// `server/index.js:527` — both `(45, 18, 35)`.
    pub mothership_half: Vec3,
    /// Mothership z positions, at `±mothership_z`. `main.js:154`/`:157` and
    /// `server/index.js:527`–`:519` — both ∓600.
    pub mothership_z: f64,
    /// Airfield box half-extents on the terrain map. `airfield.js:2`
    /// (`AIRFIELD_HALF`).
    pub airfield_half: Vec3,
    /// Airfield z positions, at `±airfield_z`.
    ///
    /// **Changed in the Rust port**, from the JS's ∓1500. A mesa is its pad
    /// plus [`crate::terrain`]'s apron and ramp — 393 units either side of this
    /// — and at 1500 the far ramp finished 93 units *outside* the map, where
    /// the height function has already handed over to open sea. 1400 puts the
    /// whole mesa inside the border, so the back slope runs down into the water
    /// instead of ending in a cliff over nothing.
    pub airfield_z: f64,
    /// Elevation of the landing pads above the waterline.
    ///
    /// **New in the Rust port.** The JS flattened both airfields to `y = 0`, so
    /// each team spawned at the bottom of a circular pit with the surrounding
    /// hills above them on every bearing. [`crate::terrain`] builds a mesa
    /// instead: the pad is held at this elevation and the ground ramps down off
    /// its rim, which is what makes a base high ground rather than a hole.
    ///
    /// Read by three places that must agree — the heightfield that flattens the
    /// pad, the [`BoxVolume`] a ship lands on, and
    /// [`SpawnRules::terrain_y`], which is this plus the launch height.
    ///
    /// [`BoxVolume`]: crate::world::BoxVolume
    pub airfield_elevation: f64,

    /// Terrain extent; outside `±size/2` the height function returns
    /// [`Self::water_level`].
    ///
    /// Must be an exact multiple of [`crate::terrain::LATTICE_SEGMENTS`]; see
    /// that constant for why.
    pub terrain_size: f64,
    /// The waterline: the sea around the island, and the surface of the lake
    /// and rivers cut into it.
    ///
    /// **New in the Rust port**, along with there being any water. Zero is not
    /// arbitrary — the JS height function clamped at zero and the map edge fell
    /// away to a flat plain at zero, so putting the sea there is the reading of
    /// the old map that changes the fewest other numbers. What *is* new is that
    /// the terrain is now allowed below it.
    pub water_level: f64,
    /// Height above the terrain surface at which a ship is killed.
    /// `terrain.js:4` (`TERRAIN_KILL_CLEARANCE`), used at `main.js:2251`.
    ///
    /// Measured from the *surface*, which since the port includes the water: a
    /// lake stops a ship exactly as a hillside does.
    pub terrain_kill_clearance: f64,

    /// Asteroid field generation.
    pub asteroid_field: AsteroidFieldRules,
}

impl WorldRules {
    /// The frozen default.
    pub const DEFAULT: Self = Self {
        moon_radius: 80.0,
        moon_pos: Vec3::ZERO,
        moon_avoid_half: Vec3::splat(80.0),

        mothership_half: Vec3::new(45.0, 18.0, 35.0),
        mothership_z: 600.0,
        airfield_half: Vec3::new(280.0, 4.0, 190.0),
        airfield_z: 1400.0,
        airfield_elevation: 210.0,

        terrain_size: 3600.0,
        water_level: 0.0,
        terrain_kill_clearance: 5.0,

        asteroid_field: AsteroidFieldRules {
            count: 60,
            radius: 400.0,
            min_dist_base: 30.0,
            y_flatten: 0.4,
            place_attempts: 10,
            avoid_margin: 6.0,
            // Was the JS's literal 0.95. See the field's doc comment: that
            // number was a quarter of a rock bigger than the rock.
            collision_radius_scale: AsteroidMeshRules::DEFAULT.mean_radius_scale(),
            mesh: AsteroidMeshRules::DEFAULT,
            variant_count: 6,
            avoid_moon: true,
            trials_counts: [120, 150, 180, 210],
        },
    };
}

// ---------------------------------------------------------------------------
// Spawning
// ---------------------------------------------------------------------------

/// Where ships appear, and how far they scatter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpawnRules {
    /// Team spawn z on the space map, at `∓space_z`. `main.js:228`/`:3328` and
    /// `server/index.js:480`–`:472` — both ∓540, 60 units in front of the
    /// mothership hull so the ship starts at the hangar mouth.
    pub space_z: f64,
    /// Team spawn y on the space map. `server/index.js:509` centres on 0.
    pub space_y: f64,
    /// Full width of the spawn scatter box on the space map.
    ///
    /// **Divergence resolved.** `server/index.js:508`–`:502` scatters
    /// `(8, 4, 6)` full width, i.e. ±(4, 2, 3). `main.js:3331`–`:3333`
    /// scatters `(60, 20, 60)`, i.e. ±(30, 10, 30) — ten times wider.
    ///
    /// Unified on the **server's `(8, 4, 6)`**: the tight box is the one that
    /// was tuned against the mothership hangar mouth, and the wide one drops
    /// solo players outside it, sometimes clipping the hull on frame one.
    pub space_jitter: Vec3,

    /// Team spawn z on the terrain map, at `∓terrain_z`.
    ///
    /// **Changed in the Rust port**, from the JS's ∓1400, tracking
    /// [`WorldRules::airfield_z`]: it is 100 units in front of the pad centre,
    /// exactly as the JS's was.
    ///
    /// [`WorldRules::airfield_z`]: crate::rules::WorldRules::airfield_z
    pub terrain_z: f64,
    /// Team spawn y on the terrain map — above the runway.
    ///
    /// **Changed in the Rust port**, from the JS's 40. The runway is no longer
    /// at sea level, so this is
    /// [`WorldRules::airfield_elevation`]` + `[`Self::TERRAIN_LAUNCH_HEIGHT`]
    /// and the 40 units of clearance over the tarmac are unchanged.
    ///
    /// [`WorldRules::airfield_elevation`]: crate::rules::WorldRules::airfield_elevation
    pub terrain_y: f64,
    /// Full width of the spawn scatter box on the terrain map.
    ///
    /// **Divergence resolved.** `server/index.js:499`–`:492` uses
    /// `(60, 10, 40)`; `main.js:3331`–`:3333` uses `(60, 20, 60)`. Same
    /// reasoning as [`Self::space_jitter`]: take the server's.
    pub terrain_jitter: Vec3,

    /// Trials start position. `main.js:224` and `main.js:3312` — both
    /// `(0, 20, -510)`, just outside checkpoint 0.
    pub trials_start: Vec3,

    /// Distance ahead of the player at which the training bot spawns.
    /// `main.js:2899` (`addScaledVector(fwd, 250)`).
    pub train_bot_distance: f64,
    /// Distance from the player at which a training bot *respawns*, on a random
    /// horizontal bearing. `main.js:3290`.
    pub train_bot_respawn_distance: f64,
    /// Full width of the scatter applied to a respawning solo bot.
    /// `main.js:3293`–`:3295`.
    pub bot_respawn_jitter: Vec3,
    /// Full width of the scatter applied to skirmish bots at match start.
    /// `main.js:2906` (`jitter(80), jitter(30), jitter(80)`).
    pub skirmish_jitter: Vec3,
    /// Allies spawned in a solo skirmish. `main.js:2905`.
    pub skirmish_ally_count: u32,
    /// Enemies spawned in a solo skirmish. `main.js:2909` — one more than the
    /// allies, because the player counts.
    pub skirmish_enemy_count: u32,
}

impl SpawnRules {
    /// Clearance a ship starts with over its own landing pad. The JS's 40, kept
    /// as the *height above the tarmac* now that the tarmac has moved up.
    pub const TERRAIN_LAUNCH_HEIGHT: f64 = 40.0;

    /// The frozen default.
    pub const DEFAULT: Self = Self {
        space_z: 540.0,
        space_y: 0.0,
        space_jitter: Vec3::new(8.0, 4.0, 6.0),

        terrain_z: WorldRules::DEFAULT.airfield_z - 100.0,
        terrain_y: WorldRules::DEFAULT.airfield_elevation + Self::TERRAIN_LAUNCH_HEIGHT,
        terrain_jitter: Vec3::new(60.0, 10.0, 40.0),

        trials_start: Vec3::new(0.0, 20.0, -510.0),

        train_bot_distance: 250.0,
        train_bot_respawn_distance: 280.0,
        bot_respawn_jitter: Vec3::new(60.0, 20.0, 60.0),
        skirmish_jitter: Vec3::new(80.0, 30.0, 80.0),
        skirmish_ally_count: 4,
        skirmish_enemy_count: 5,
    };
}

// ---------------------------------------------------------------------------
// Match
// ---------------------------------------------------------------------------

/// Match clock, scoring, and the network-facing timing the simulation has to
/// know about.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatchRules {
    /// Training-mode match length. `main.js:2266` (the `'train'` branch).
    pub train_duration: f64,
    /// Skirmish and multiplayer match length. `main.js:2266` (the else branch,
    /// 300) and `server/index.js:436` (`300 * 1000` ms) — the two agree.
    pub duration: f64,
    /// Number of teams. Two, everywhere, and the win condition compares exactly
    /// two counters (`server/index.js:449`).
    pub team_count: usize,

    /// Interval between outbound position updates. `main.js:978`
    /// (`STATE_INTERVAL = 1 / 20`).
    pub state_send_interval: f64,
    /// Exponential rate at which a remote ship's rendered pose chases its last
    /// received pose. `main.js:1721`, as `1 - 0.001^(dt * 8)`.
    pub remote_lerp_rate: f64,
    /// Blend applied to each new remote velocity estimate. `main.js:837`
    /// (`r.vel.lerp(measured, 0.45)`).
    pub remote_vel_blend: f64,
    /// Estimates from intervals shorter than this are discarded as jitter.
    /// `main.js:834`.
    pub remote_vel_dt_min: f64,
    /// Estimates from intervals longer than this are discarded as a stall.
    /// `main.js:834`.
    pub remote_vel_dt_max: f64,
}

impl MatchRules {
    /// The frozen default.
    pub const DEFAULT: Self = Self {
        train_duration: 180.0,
        duration: 300.0,
        team_count: 2,

        state_send_interval: 1.0 / 20.0,
        remote_lerp_rate: 8.0,
        remote_vel_blend: 0.45,
        remote_vel_dt_min: 0.005,
        remote_vel_dt_max: 0.5,
    };
}

// ---------------------------------------------------------------------------
// Trials
// ---------------------------------------------------------------------------

/// Time-trial checkpoint racing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrialsRules {
    /// Distance at which a checkpoint counts as passed. `main.js:357`
    /// (`CP_TRIGGER_DIST`).
    pub cp_trigger_dist: f64,
    /// Dead time after passing a checkpoint, so one pass cannot trigger twice.
    /// `main.js:372` (initial) and `main.js:1755` (after each pass).
    pub cp_cooldown: f64,
    /// Longer dead time after a crash reset, so respawning on top of a ring
    /// does not instantly re-arm it. `main.js:3317`.
    pub cp_cooldown_after_reset: f64,
    /// Countdown before a trial run begins. `main.js:398`.
    pub countdown: f64,
    /// Boost meter refunded for passing a checkpoint. `main.js:1751`
    /// (`boostMeter + 3.5`), clamped to [`ShipRules::max_boost`].
    pub cp_boost_award: f64,
}

impl TrialsRules {
    /// The frozen default.
    pub const DEFAULT: Self = Self {
        cp_trigger_dist: 55.0,
        cp_cooldown: 1.5,
        cp_cooldown_after_reset: 2.0,
        countdown: 3.0,
        cp_boost_award: 3.5,
    };
}

/// Trial 1 checkpoint ring positions, in lap order. `main.js:280`.
pub const TRIAL1_CHECKPOINTS: [Vec3; 12] = [
    Vec3::new(0.0, 20.0, -380.0),
    Vec3::new(180.0, 60.0, -260.0),
    Vec3::new(340.0, 0.0, -80.0),
    Vec3::new(360.0, -50.0, 120.0),
    Vec3::new(220.0, 80.0, 280.0),
    Vec3::new(60.0, -60.0, 370.0),
    Vec3::new(-150.0, 40.0, 360.0),
    Vec3::new(-320.0, -40.0, 180.0),
    Vec3::new(-370.0, 60.0, -60.0),
    Vec3::new(-260.0, -80.0, -240.0),
    Vec3::new(-100.0, 30.0, -360.0),
    Vec3::new(100.0, -40.0, -350.0),
];

/// Trial 2 checkpoint ring positions, in lap order. `main.js:294`.
pub const TRIAL2_CHECKPOINTS: [Vec3; 14] = [
    Vec3::new(0.0, 20.0, -360.0),
    Vec3::new(160.0, 80.0, -220.0),
    Vec3::new(290.0, -40.0, -80.0),
    Vec3::new(310.0, -80.0, 100.0),
    Vec3::new(190.0, 100.0, 270.0),
    Vec3::new(40.0, -90.0, 330.0),
    Vec3::new(-120.0, 70.0, 310.0),
    Vec3::new(-270.0, -60.0, 190.0),
    Vec3::new(-300.0, 90.0, 20.0),
    Vec3::new(-270.0, -100.0, -170.0),
    Vec3::new(-120.0, 60.0, -310.0),
    Vec3::new(20.0, -80.0, -310.0),
    Vec3::new(140.0, 90.0, -240.0),
    Vec3::new(260.0, -60.0, -120.0),
];

/// Trial 3 checkpoint ring positions, in lap order. `main.js:310`.
pub const TRIAL3_CHECKPOINTS: [Vec3; 16] = [
    Vec3::new(0.0, -30.0, -370.0),
    Vec3::new(150.0, 100.0, -240.0),
    Vec3::new(300.0, -80.0, -60.0),
    Vec3::new(350.0, 100.0, 120.0),
    Vec3::new(220.0, -110.0, 280.0),
    Vec3::new(60.0, 100.0, 350.0),
    Vec3::new(-80.0, -110.0, 300.0),
    Vec3::new(-240.0, 100.0, 160.0),
    Vec3::new(-330.0, -90.0, 0.0),
    Vec3::new(-260.0, 110.0, -180.0),
    Vec3::new(-120.0, -100.0, -290.0),
    Vec3::new(20.0, 110.0, -350.0),
    Vec3::new(170.0, -100.0, -250.0),
    Vec3::new(310.0, 100.0, -70.0),
    Vec3::new(220.0, -110.0, 120.0),
    Vec3::new(80.0, 80.0, -200.0),
];

/// Trial 4 checkpoint ring positions, in lap order. `main.js:328`.
pub const TRIAL4_CHECKPOINTS: [Vec3; 18] = [
    Vec3::new(0.0, 50.0, -370.0),
    Vec3::new(180.0, -100.0, -210.0),
    Vec3::new(340.0, 110.0, -40.0),
    Vec3::new(210.0, -110.0, 240.0),
    Vec3::new(40.0, 110.0, 340.0),
    Vec3::new(-180.0, -110.0, 210.0),
    Vec3::new(-160.0, 80.0, 0.0),
    Vec3::new(-200.0, -100.0, -210.0),
    Vec3::new(0.0, 110.0, -180.0),
    Vec3::new(200.0, -100.0, -40.0),
    Vec3::new(300.0, 100.0, 180.0),
    Vec3::new(80.0, -110.0, 320.0),
    Vec3::new(-200.0, 100.0, 180.0),
    Vec3::new(-320.0, -100.0, -40.0),
    Vec3::new(-200.0, 100.0, -220.0),
    Vec3::new(0.0, -110.0, -340.0),
    Vec3::new(200.0, 100.0, -220.0),
    Vec3::new(100.0, -80.0, -330.0),
];

/// The checkpoint ring for trial `n` (1–4). Out-of-range values fall back to
/// trial 1, matching the JS ternary chain at `main.js:348`.
#[must_use]
pub fn trial_checkpoints(n: u8) -> &'static [Vec3] {
    match n {
        2 => &TRIAL2_CHECKPOINTS,
        3 => &TRIAL3_CHECKPOINTS,
        4 => &TRIAL4_CHECKPOINTS,
        _ => &TRIAL1_CHECKPOINTS,
    }
}

// ---------------------------------------------------------------------------
// Campaign
// ---------------------------------------------------------------------------

/// One campaign wave.
///
/// `main.js:2275` (`CAMPAIGN_WAVES`). The JS rows also carry `label` and
/// `objective` strings; those are HUD copy and stay in JS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CampaignWave {
    /// Enemies spawned.
    pub count: u32,
    /// Anchor z for the spawn cluster. Stored as an integer because every
    /// value in the JS table is one and it keeps the type `Eq`.
    pub spawn_z: i32,
}

/// Waves for mission 1 (`OPERATION: IRONCLAD`). `main.js:2287`.
#[rustfmt::skip]
pub const CAMPAIGN_WAVES_M1: [CampaignWave; 3] = [
    CampaignWave { count: 3, spawn_z: -280 },
    CampaignWave { count: 5, spawn_z: 20 },
    CampaignWave { count: 4, spawn_z: 330 },
];

/// Waves for mission 2 (`OPERATION: STORMFRONT`). `main.js:2282`.
#[rustfmt::skip]
pub const CAMPAIGN_WAVES_M2: [CampaignWave; 3] = [
    CampaignWave { count: 4, spawn_z: -280 },
    CampaignWave { count: 6, spawn_z: 20 },
    CampaignWave { count: 5, spawn_z: 330 },
];

/// Waves for mission 3 (`OPERATION: FINAL SIEGE`). `main.js:2276`.
#[rustfmt::skip]
pub const CAMPAIGN_WAVES_M3: [CampaignWave; 3] = [
    CampaignWave { count: 5, spawn_z: -280 },
    CampaignWave { count: 7, spawn_z: 20 },
    CampaignWave { count: 6, spawn_z: 330 },
];

/// The three slabs a campaign asteroid field is built from. `main.js:240`
/// (`ZONES`) — a corridor, not a sphere, because the mission is a flight path.
#[rustfmt::skip]
pub const CAMPAIGN_ASTEROID_ZONES: [AsteroidZone; 3] = [
    AsteroidZone { z_min: -520.0, z_max: -150.0, count:  90, x_range: 110.0, y_range: 55.0 },
    AsteroidZone { z_min: -180.0, z_max:  200.0, count: 100, x_range: 130.0, y_range: 65.0 },
    AsteroidZone { z_min:  160.0, z_max:  540.0, count:  90, x_range: 110.0, y_range: 55.0 },
];

/// The wave table for `mission` (1–3). Unknown missions fall back to mission 1,
/// matching the JS ternary chain at `main.js:2275`.
#[must_use]
pub fn campaign_waves(mission: u8) -> &'static [CampaignWave; 3] {
    match mission {
        2 => &CAMPAIGN_WAVES_M2,
        3 => &CAMPAIGN_WAVES_M3,
        _ => &CAMPAIGN_WAVES_M1,
    }
}

/// Campaign mission structure and the capital ship boss fight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CampaignRules {
    /// Lives before the mission fails. `main.js:2332` (`campaignLives = 3`).
    pub lives: i32,
    /// Fraction of [`ShipRules::max_hp`] restored on a campaign respawn — you
    /// come back hurt. `main.js:3343` (`floor(SHIP_MAX_HP * 0.55)`).
    pub respawn_hp_fraction: f64,
    /// Warp-in effect duration after a campaign death. `main.js:3252`.
    pub warp_duration: f64,
    /// First entity id handed to a campaign bot; each wave increments.
    /// `main.js:2330` (`campaignNextBotId = 100`).
    pub first_bot_id: i32,
    /// Pause between clearing a wave and the next one spawning.
    /// `main.js:2873`.
    pub wave_gap: f64,
    /// Pause between clearing the last wave and the boss activating.
    /// `main.js:2879` — slightly longer, to let the "shields offline" banner
    /// land.
    pub boss_gap: f64,
    /// Full width of the spawn scatter for a campaign wave. `main.js:2708`.
    pub wave_spawn_jitter: Vec3,
    /// Anchor y for a campaign wave. `main.js:2704`.
    pub wave_anchor_y: f64,
    /// The three asteroid slabs a campaign map is built from. Defaults to
    /// [`CAMPAIGN_ASTEROID_ZONES`].
    pub asteroid_zones: [AsteroidZone; 3],

    /// Boss hit points. `main.js:2274` (`BOSS_MAX_HP`).
    pub boss_max_hp: i32,
    /// Capital ship rest position. `main.js:2339` (`CAPITAL_SHIP_BASE_POS`).
    pub boss_base_pos: Vec3,
    /// Amplitude of the capital ship's lateral drift. `main.js:2659`.
    pub boss_drift_x_amp: f64,
    /// Angular rate of the lateral drift. `main.js:2659`.
    pub boss_drift_x_rate: f64,
    /// Amplitude of the capital ship's vertical drift. `main.js:2660`.
    pub boss_drift_y_amp: f64,
    /// Angular rate of the vertical drift. `main.js:2660`.
    pub boss_drift_y_rate: f64,
    /// Turret pitch limit, applied symmetrically. `main.js:2679`.
    pub turret_pitch_limit: f64,
    /// Full width of the random aim error added to each turret round.
    /// `main.js:2684`–`:2685` (`(random() - 0.5) * 0.09` on x and y).
    pub turret_spread: f64,
    /// Turret reload while the boss is above [`Self::boss_hp_threshold_high`],
    /// as `[min, range]`: the delay is `min + random() * range`.
    /// `main.js:2692`.
    pub turret_reload_healthy: [f64; 2],
    /// Turret reload between the two thresholds. `main.js:2693`.
    pub turret_reload_wounded: [f64; 2],
    /// Turret reload below [`Self::boss_hp_threshold_low`]. `main.js:2694`.
    pub turret_reload_critical: [f64; 2],
    /// HP fraction above which turrets use the slow reload. `main.js:2692`.
    pub boss_hp_threshold_high: f64,
    /// HP fraction below which turrets use the fast reload. `main.js:2693`.
    pub boss_hp_threshold_low: f64,
    /// Per-turret initial reload, as `index * stagger + offset`, so the four
    /// turrets do not fire in unison. `main.js:2649`
    /// (`fireTimer: i * 0.85 + 0.4`).
    pub turret_stagger: f64,
    /// Offset half of the same expression.
    pub turret_stagger_offset: f64,
}

impl CampaignRules {
    /// The frozen default.
    pub const DEFAULT: Self = Self {
        lives: 3,
        respawn_hp_fraction: 0.55,
        warp_duration: 1.5,
        first_bot_id: 100,
        wave_gap: 3.5,
        boss_gap: 4.8,
        wave_spawn_jitter: Vec3::new(160.0, 60.0, 130.0),
        wave_anchor_y: 20.0,
        asteroid_zones: CAMPAIGN_ASTEROID_ZONES,

        boss_max_hp: 2500,
        boss_base_pos: Vec3::new(0.0, 0.0, 600.0),
        boss_drift_x_amp: 88.0,
        boss_drift_x_rate: 0.09,
        boss_drift_y_amp: 9.0,
        boss_drift_y_rate: 0.055,
        turret_pitch_limit: 0.7,
        turret_spread: 0.09,
        turret_reload_healthy: [2.8, 0.7],
        turret_reload_wounded: [1.6, 0.5],
        turret_reload_critical: [0.9, 0.3],
        boss_hp_threshold_high: 0.65,
        boss_hp_threshold_low: 0.35,
        turret_stagger: 0.85,
        turret_stagger_offset: 0.4,
    };
}

// ---------------------------------------------------------------------------
// Bots
// ---------------------------------------------------------------------------

/// Bot AI tuning. All from `bot.js:13`–`:56` unless noted.
///
/// Note what is *not* here: the bot's own bullet radius. `bot.js` runs a second,
/// shadow bullet simulation (`fireBullet` at `:301` spawns both a visual bullet
/// and a `myProjectiles` entry, updated at `:314`) with its own hit radius of
/// 4.0, while the bullet the player sees has an effective radius of 6.5. There
/// is one bullet simulation in this port, using
/// [`ShipRules::hit_radius`] + [`WeaponRules::bullet_radius`] like everything
/// else.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BotRules {
    /// Cruise speed. `bot.js:15` (`SPEED`).
    pub speed: f64,
    /// Turn rate. `bot.js:16` (`TURN_RATE`).
    pub turn_rate: f64,
    /// Exponential acceleration rate toward the desired velocity.
    /// `bot.js:17` (`ACCEL`), used as `1 - e^(-ACCEL * dt)`.
    pub accel: f64,
    /// Speed fraction used when no target exists — a slow patrol.
    /// `bot.js:139` (`SPEED * 0.3`).
    pub idle_speed_fraction: f64,

    /// Maximum gun range. `bot.js:17` (`FIRE_RANGE`).
    pub fire_range: f64,
    /// Minimum `dot(forward, to_aimpoint)` before firing. `bot.js:18`
    /// (`FIRE_DOT`).
    pub fire_dot: f64,
    /// Gun cooldown on normal difficulty. `bot.js:19` (`FIRE_COOLDOWN`, the
    /// `: 0.15` branch).
    pub fire_cooldown: f64,
    /// Gun cooldown on hard difficulty. `bot.js:19` (the `? 0.05` branch) —
    /// three times the rate of fire.
    pub fire_cooldown_hard: f64,
    /// Muzzle offset ahead of the bot. `bot.js:304`.
    pub muzzle_offset: f64,

    /// Closest range at which a bot will launch a missile. `bot.js:21`
    /// (`MISSILE_MIN_RANGE`).
    pub missile_min_range: f64,
    /// Furthest range at which a bot will launch a missile. `bot.js:22`
    /// (`MISSILE_MAX_RANGE`).
    pub missile_max_range: f64,
    /// Minimum `dot(forward, to_target)` before launching. `bot.js:23`
    /// (`MISSILE_FIRE_DOT`) — tighter than the gun's.
    pub missile_fire_dot: f64,
    /// Missile cooldown. `bot.js:24` (`MISSILE_COOLDOWN`).
    pub missile_cooldown: f64,
    /// Minimum of the randomized initial missile delay. `bot.js:25`
    /// (`2.5 + random() * 4.0`).
    pub missile_delay_min: f64,
    /// Width of the randomized initial missile delay range. `bot.js:25`.
    pub missile_delay_range: f64,
    /// Missiles carried on normal difficulty. `main.js:2497`
    /// (`missileMax: opts.hardMode ? 3 : 1`).
    pub missile_max: u8,
    /// Missiles carried on hard difficulty. Same site.
    pub missile_max_hard: u8,

    /// Range at which a seeking bot switches to attacking. `bot.js:28`
    /// (`SEEK_DIST`). It switches back at `SEEK_DIST * 1.3` (`bot.js:159`).
    pub seek_dist: f64,
    /// Hysteresis multiplier for leaving the attack state. `bot.js:159`.
    pub seek_exit_multiplier: f64,
    /// Range at which an attacking bot breaks off to evade. `bot.js:29`
    /// (`ATTACK_TOO_CLOSE`).
    pub attack_too_close: f64,
    /// How long an evade lasts. `bot.js:30` (`EVADE_DURATION`).
    pub evade_duration: f64,

    /// Maximum magnitude of the bot's wandering aim error. `bot.js:32`
    /// (`AIM_OFFSET_MAX`).
    pub aim_offset_max: f64,
    /// Rate at which the aim error random-walks. `bot.js:33`
    /// (`AIM_OFFSET_DRIFT`).
    pub aim_offset_drift: f64,
    /// Range at which the aim error is applied at full magnitude; it scales
    /// linearly with distance. `bot.js:34` (`AIM_REF_DIST`).
    pub aim_ref_dist: f64,
    /// Exponential rate at which the bot's tracked lead point chases the true
    /// intercept. `bot.js:35` (`AIM_TRACK_RATE`).
    pub aim_track_rate: f64,

    /// Minimum obstacle-avoidance lookahead. `bot.js:36` (`AVOID_LOOKAHEAD`);
    /// the actual lookahead is `max(this, radius * 2.5)` (`bot.js:88`).
    pub avoid_lookahead: f64,
    /// Clearance the bot tries to keep from an obstacle surface. `bot.js:37`
    /// (`AVOID_MARGIN`).
    pub avoid_margin: f64,
    /// Weight of the avoidance vector against the steering vector.
    /// `bot.js:38` (`AVOID_WEIGHT`).
    pub avoid_weight: f64,

    /// Speed below which the bot may be stuck. `bot.js:39`
    /// (`STUCK_SPEED_THRESH`).
    pub stuck_speed_threshold: f64,
    /// How long it must stay that slow before forcing an evade. `bot.js:40`
    /// (`STUCK_TIME`).
    pub stuck_time: f64,

    /// Terrain clearance below which the bot pulls up. `bot.js:196`.
    pub terrain_margin: f64,
    /// Strength of the pull-up, applied to the desired direction's y before
    /// renormalizing. `bot.js:205`.
    pub terrain_pull: f64,
    /// Hard floor the bot is clamped to above the terrain. `bot.js:264`.
    pub terrain_min_clearance: f64,
    /// Lookahead multiple of [`Self::speed`] used for the terrain check.
    /// `bot.js:199` (`SPEED * 1.5`).
    pub terrain_lookahead_seconds: f64,
}

impl BotRules {
    /// The frozen default.
    pub const DEFAULT: Self = Self {
        speed: 60.0,
        turn_rate: 1.3,
        accel: 3.0,
        idle_speed_fraction: 0.3,

        fire_range: 600.0,
        fire_dot: 0.97,
        fire_cooldown: 0.15,
        fire_cooldown_hard: 0.05,
        muzzle_offset: 2.5,

        missile_min_range: 130.0,
        missile_max_range: 560.0,
        missile_fire_dot: 0.90,
        missile_cooldown: 8.0,
        missile_delay_min: 2.5,
        missile_delay_range: 4.0,
        missile_max: 1,
        missile_max_hard: 3,

        seek_dist: 250.0,
        seek_exit_multiplier: 1.3,
        attack_too_close: 35.0,
        evade_duration: 0.6,

        // `bot.js:32` is 14.0, and that was tuned against a bot hit radius of
        // 4.0. Unifying the hit radius took bots to 6.0 -- 50% wider, so about
        // 2.25x the cross-section -- and they became markedly deadlier than the
        // game this reproduces. Widening the wander restores roughly the
        // original hit rate without giving bots back a private geometry, which
        // is the thing the unification existed to remove.
        //
        // 14.0 * (6.0 / 4.0) keeps error proportional to the target they are
        // now being handed.
        aim_offset_max: 21.0,
        aim_offset_drift: 12.0,
        aim_ref_dist: 200.0,
        aim_track_rate: 10.0,

        avoid_lookahead: 80.0,
        avoid_margin: 4.0,
        avoid_weight: 2.0,

        stuck_speed_threshold: 6.0,
        stuck_time: 1.5,

        terrain_margin: 180.0,
        terrain_pull: 6.0,
        terrain_min_clearance: 5.0,
        terrain_lookahead_seconds: 1.5,
    };

    /// Gun cooldown for the given difficulty.
    #[must_use]
    pub fn fire_cooldown_for(&self, hard: bool) -> f64 {
        if hard {
            self.fire_cooldown_hard
        } else {
            self.fire_cooldown
        }
    }

    /// Missile load-out for the given difficulty.
    #[must_use]
    pub fn missile_max_for(&self, hard: bool) -> u8 {
        if hard {
            self.missile_max_hard
        } else {
            self.missile_max
        }
    }
}

// ---------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------

/// Every tunable in one value.
///
/// A `World` owns one of these. Nothing in the simulation may read a game
/// constant from anywhere else — that is the entire point of the module.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rules {
    /// Hull, flight model, boost.
    pub ship: ShipRules,
    /// Guns, missiles, flares.
    pub weapons: WeaponRules,
    /// Damage, death, respawn, regeneration.
    pub combat: CombatRules,
    /// Static world geometry and asteroid field generation.
    pub world: WorldRules,
    /// Spawn positions and scatter.
    pub spawn: SpawnRules,
    /// Match clock and scoring.
    pub match_rules: MatchRules,
    /// Time trials.
    pub trials: TrialsRules,
    /// Campaign missions and the boss fight.
    pub campaign: CampaignRules,
    /// Bot AI.
    pub bot: BotRules,
    /// Aim assist.
    pub aim_assist: AimAssistRules,
}

impl Rules {
    /// The game as shipped, with every client/server divergence resolved.
    ///
    /// Read the individual field docs for the decisions; the summary is:
    ///
    /// | Rule | Client | Server | Chosen |
    /// |---|---|---|---|
    /// | Respawn delay | 2.5 s | 2.0 s | **2.0 s** |
    /// | Ship hit radius | 6.0 / 7.0 / 5.5 / 4.0 / 7.0 | — | **6.0** (+1.0 coarse aim) |
    /// | Boss hit radius | 28 / 6 / 95 | — | **28** |
    /// | Ship collide radius | 3.3 player, 3.5 bot | — | **3.3** |
    /// | Spawn invuln | player only | everyone | **everyone** |
    /// | Asteroids avoid moon | yes | no | **yes** |
    /// | Space spawn jitter | ±(30, 10, 30) | ±(4, 2, 3) | **±(4, 2, 3)** |
    /// | Terrain spawn jitter | ±(30, 10, 30) | ±(30, 5, 20) | **±(30, 5, 20)** |
    pub const DEFAULT: Self = Self {
        ship: ShipRules::DEFAULT,
        weapons: WeaponRules::DEFAULT,
        combat: CombatRules::DEFAULT,
        world: WorldRules::DEFAULT,
        spawn: SpawnRules::DEFAULT,
        match_rules: MatchRules::DEFAULT,
        trials: TrialsRules::DEFAULT,
        campaign: CampaignRules::DEFAULT,
        bot: BotRules::DEFAULT,
        aim_assist: AimAssistRules::DEFAULT,
    };

    /// Checks the invariants the simulation relies on.
    ///
    /// [`Rules::DEFAULT`] passes by construction (there is a test). This exists
    /// for the cases where `Rules` is *not* the default: a test that tweaks one
    /// field, a future game mode, or a config loaded at the edge and handed in.
    /// A rule set that fails these will not merely play differently, it will
    /// hang, divide by zero, or generate a field that never terminates.
    ///
    /// # Errors
    ///
    /// Returns the first violated invariant. The error names the field and what
    /// is wrong with it.
    pub fn validate(&self) -> Result<(), RulesError> {
        fn check(ok: bool, field: &'static str, problem: &'static str) -> Result<(), RulesError> {
            if ok {
                Ok(())
            } else {
                Err(RulesError { field, problem })
            }
        }

        check(self.ship.max_hp > 0, "ship.max_hp", "must be positive")?;
        check(
            self.ship.collide_radius > 0.0,
            "ship.collide_radius",
            "must be positive",
        )?;
        check(
            self.ship.hit_radius > 0.0,
            "ship.hit_radius",
            "must be positive",
        )?;
        check(
            self.ship.hit_radius_coarse_aim_bonus >= 0.0,
            "ship.hit_radius_coarse_aim_bonus",
            "must not be negative: coarse aim may only widen the target",
        )?;
        check(
            self.ship.max_throttle > 0.0,
            "ship.max_throttle",
            "must be positive",
        )?;
        check(
            self.ship.max_boost > 0.0,
            "ship.max_boost",
            "must be positive",
        )?;
        check(
            self.ship.drift_drag > 0.0 && self.ship.drift_drag <= 1.0,
            "ship.drift_drag",
            "is a per-second retention factor and must lie in (0, 1]",
        )?;
        check(
            self.ship.drift_brake > 0.0 && self.ship.drift_brake <= 1.0,
            "ship.drift_brake",
            "is a per-second retention factor and must lie in (0, 1]",
        )?;
        check(
            self.ship.brake_full_time > 0.0,
            "ship.brake_full_time",
            "must be positive: it is a divisor",
        )?;

        check(
            self.weapons.bullet_cooldown > 0.0,
            "weapons.bullet_cooldown",
            "must be positive or the gun fires unbounded shots per tick",
        )?;
        check(
            self.weapons.beam_cooldown > 0.0,
            "weapons.beam_cooldown",
            "must be positive or the gun fires unbounded shots per tick",
        )?;
        check(
            self.weapons.bullet_life > 0.0,
            "weapons.bullet_life",
            "must be positive or bullets never expire",
        )?;
        check(
            self.weapons.missile_life > 0.0,
            "weapons.missile_life",
            "must be positive or missiles never expire",
        )?;
        check(
            self.weapons.flare_life > 0.0,
            "weapons.flare_life",
            "must be positive: it is a divisor in the flare fade",
        )?;
        check(
            self.weapons.beam_ammo_cost <= self.weapons.max_ammo,
            "weapons.beam_ammo_cost",
            "exceeds max_ammo, so the beam could never be fired",
        )?;
        check(
            self.weapons.bullet_ammo_cost <= self.weapons.max_ammo,
            "weapons.bullet_ammo_cost",
            "exceeds max_ammo, so the gun could never be fired",
        )?;
        check(
            self.weapons.gun_damage > 0 && self.weapons.missile_damage > 0,
            "weapons.gun_damage",
            "damage values must be positive",
        )?;
        check(
            self.weapons.boss_hitbox_radius > 0.0,
            "weapons.boss_hitbox_radius",
            "must be positive",
        )?;

        check(
            self.combat.respawn_delay >= 0.0,
            "combat.respawn_delay",
            "must not be negative",
        )?;
        check(
            self.combat.spawn_invuln >= 0.0,
            "combat.spawn_invuln",
            "must not be negative",
        )?;
        check(
            self.combat.health_regen_interval > 0.0,
            "combat.health_regen_interval",
            "must be positive or regeneration is unbounded per tick",
        )?;
        check(
            self.combat.asteroid_damage_per_hit > 0,
            "combat.asteroid_damage_per_hit",
            "must be positive or asteroids can never be destroyed",
        )?;
        check(
            self.combat.asteroid_collision_damage_min <= self.combat.asteroid_collision_damage_max,
            "combat.asteroid_collision_damage_min",
            "exceeds asteroid_collision_damage_max",
        )?;
        check(
            self.combat.asteroid_collision_damage_cooldown >= 0.0,
            "combat.asteroid_collision_damage_cooldown",
            "must not be negative; zero restores the JS's charge-every-edge",
        )?;

        let field = &self.world.asteroid_field;
        check(
            field.place_attempts > 0,
            "world.asteroid_field.place_attempts",
            "must be positive",
        )?;
        check(
            field.collision_radius_scale > 0.0,
            "world.asteroid_field.collision_radius_scale",
            "must be positive",
        )?;
        check(
            field.mesh.lobe_amp >= 0.0
                && field.mesh.lobe_amp < field.mesh.lobe_base
                && field.mesh.bump_amp >= 0.0
                && field.mesh.bump_amp < field.mesh.bump_base,
            "world.asteroid_field.mesh",
            "each octave's amplitude must be below its base, or a rock can turn \
             inside out at a vertex",
        )?;
        check(
            field.collision_radius_scale >= field.mesh.min_radius_scale()
                && field.collision_radius_scale <= field.mesh.max_radius_scale(),
            "world.asteroid_field.collision_radius_scale",
            "must lie inside the range of radii the mesh description can draw, \
             or the hitbox is not standing in for any part of the rock",
        )?;
        check(
            field.avoid_moon,
            "world.asteroid_field.avoid_moon",
            "must stay true: with it false, rocks generate inside the moon",
        )?;

        let mut weight_sum = 0.0;
        for tier in &ASTEROID_TIERS {
            check(
                tier.min_size > 0.0 && tier.min_size <= tier.max_size,
                "ASTEROID_TIERS.min_size",
                "must be positive and no greater than max_size",
            )?;
            check(tier.hp > 0, "ASTEROID_TIERS.hp", "must be positive")?;
            check(
                tier.weight > 0.0,
                "ASTEROID_TIERS.weight",
                "must be positive",
            )?;
            weight_sum += tier.weight;
        }
        check(
            (weight_sum - 1.0).abs() < 1e-9,
            "ASTEROID_TIERS.weight",
            "the tier weights must sum to 1",
        )?;

        // Placement draws a distance in `[min_dist_base + size, radius)`. If the
        // largest rock cannot fit, `place_attempts` always fails and every huge
        // asteroid lands in the flat fallback scatter instead.
        let largest = ASTEROID_TIERS[ASTEROID_TIER_COUNT - 1].max_size;
        check(
            field.min_dist_base + largest < field.radius,
            "world.asteroid_field.radius",
            "is too small to place the largest tier outside min_dist_base",
        )?;

        check(
            self.match_rules.duration > 0.0 && self.match_rules.train_duration > 0.0,
            "match_rules.duration",
            "match durations must be positive",
        )?;
        check(
            self.match_rules.team_count >= 2,
            "match_rules.team_count",
            "must be at least 2",
        )?;
        check(
            self.match_rules.state_send_interval > 0.0,
            "match_rules.state_send_interval",
            "must be positive",
        )?;

        check(
            self.trials.cp_trigger_dist > 0.0,
            "trials.cp_trigger_dist",
            "must be positive or checkpoints can never be passed",
        )?;
        check(
            self.trials.cp_cooldown > 0.0,
            "trials.cp_cooldown",
            "must be positive or one pass triggers repeatedly",
        )?;

        check(
            self.campaign.lives > 0,
            "campaign.lives",
            "must be positive or the mission fails on spawn",
        )?;
        check(
            self.campaign.boss_max_hp > 0,
            "campaign.boss_max_hp",
            "must be positive",
        )?;
        check(
            self.campaign.respawn_hp_fraction > 0.0 && self.campaign.respawn_hp_fraction <= 1.0,
            "campaign.respawn_hp_fraction",
            "must lie in (0, 1]",
        )?;
        check(
            self.campaign.boss_hp_threshold_low < self.campaign.boss_hp_threshold_high,
            "campaign.boss_hp_threshold_low",
            "must be below boss_hp_threshold_high",
        )?;

        check(self.bot.speed > 0.0, "bot.speed", "must be positive")?;
        check(
            self.bot.fire_cooldown > 0.0 && self.bot.fire_cooldown_hard > 0.0,
            "bot.fire_cooldown",
            "must be positive or bots fire unbounded shots per tick",
        )?;
        check(
            self.bot.missile_min_range < self.bot.missile_max_range,
            "bot.missile_min_range",
            "must be below missile_max_range or bots never launch",
        )?;
        check(
            self.bot.aim_ref_dist > 0.0,
            "bot.aim_ref_dist",
            "must be positive: it is a divisor",
        )?;
        check(
            self.bot.avoid_lookahead > 0.0,
            "bot.avoid_lookahead",
            "must be positive: it is a divisor",
        )?;

        check(
            self.aim_assist.range > self.aim_assist.min_range,
            "aim_assist.range",
            "must exceed min_range",
        )?;
        check(
            self.aim_assist.strength_damp_rate > 0.0,
            "aim_assist.strength_damp_rate",
            "must be positive or assist never engages",
        )?;
        check(
            self.aim_assist.dir_track_rate > 0.0,
            "aim_assist.dir_track_rate",
            "must be positive or the held direction never tracks",
        )?;
        for (intent_field, falloff_field, t) in [
            (
                "aim_assist.precise.intent_break",
                "aim_assist.precise.falloff_start",
                &self.aim_assist.precise,
            ),
            (
                "aim_assist.coarse.intent_break",
                "aim_assist.coarse.falloff_start",
                &self.aim_assist.coarse,
            ),
        ] {
            check(
                t.intent_break > 0.0,
                intent_field,
                "must be positive: it is a divisor",
            )?;
            check(
                t.falloff_start > t.dead_angle,
                falloff_field,
                "must exceed dead_angle: the gap between them is a divisor",
            )?;
        }

        Ok(())
    }

    /// Match length for a mode, in seconds.
    ///
    /// `main.js:2266`: training runs short because it is a warm-up with one
    /// bot; everything else runs the standard match.
    #[must_use]
    pub fn match_duration(&self, training: bool) -> f64 {
        if training {
            self.match_rules.train_duration
        } else {
            self.match_rules.duration
        }
    }
}

impl Default for Rules {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A violated [`Rules`] invariant, as reported by [`Rules::validate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RulesError {
    /// Dotted path of the offending field, e.g. `"weapons.bullet_cooldown"`.
    pub field: &'static str,
    /// What is wrong with it.
    pub problem: &'static str,
}

impl core::fmt::Display for RulesError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "rule `{}` {}", self.field, self.problem)
    }
}

impl std::error::Error for RulesError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_rules_satisfy_their_own_invariants() {
        assert_eq!(Rules::DEFAULT.validate(), Ok(()));
        assert_eq!(Rules::default(), Rules::DEFAULT);
    }

    #[test]
    fn validate_catches_a_broken_field() {
        let mut rules = Rules::DEFAULT;
        rules.weapons.bullet_cooldown = 0.0;
        let err = rules
            .validate()
            .expect_err("zero cooldown must be rejected");
        assert_eq!(err.field, "weapons.bullet_cooldown");
        assert!(err.to_string().contains("bullet_cooldown"));
    }

    #[test]
    fn validate_refuses_to_let_the_moon_bug_back_in() {
        // Bug #2: server-generated asteroids spawn inside the moon because the
        // server's placement filter only knows about motherships. The unified
        // generator must never be configured back into that state.
        let mut rules = Rules::DEFAULT;
        rules.world.asteroid_field.avoid_moon = false;
        assert_eq!(
            rules.validate().unwrap_err().field,
            "world.asteroid_field.avoid_moon"
        );
    }

    #[test]
    fn validate_catches_an_unplaceable_field() {
        let mut rules = Rules::DEFAULT;
        // 30 + 55 > 60, so no huge asteroid could ever be placed.
        rules.world.asteroid_field.radius = 60.0;
        assert_eq!(
            rules.validate().unwrap_err().field,
            "world.asteroid_field.radius"
        );
    }

    #[test]
    fn asteroid_tier_weights_sum_to_one_and_match_the_table() {
        let weights = asteroid_tier_weights();
        assert_eq!(weights, [0.45, 0.30, 0.18, 0.07]);
        let sum: f64 = weights.iter().sum();
        assert!((sum - 1.0).abs() < 1e-12, "weights sum to {sum}");
        for (i, tier) in ASTEROID_TIERS.iter().enumerate() {
            assert_eq!(tier.weight, weights[i]);
        }
    }

    #[test]
    fn asteroid_tiers_are_ordered_by_size_and_hp() {
        // The generator walks cumulative weights in table order, so the order is
        // part of the wire-compatible behaviour, not a presentation choice.
        for pair in ASTEROID_TIERS.windows(2) {
            assert!(pair[0].max_size <= pair[1].min_size, "tier sizes overlap");
            assert!(pair[0].hp < pair[1].hp, "tier HP is not increasing");
            assert!(
                pair[0].spin_scale > pair[1].spin_scale,
                "bigger rocks must spin slower"
            );
        }
    }

    #[test]
    fn coarse_aim_only_ever_widens_the_target() {
        let ship = ShipRules::DEFAULT;
        assert_eq!(ship.hit_radius_for(AimProfile::Precise), 6.0);
        assert_eq!(ship.hit_radius_for(AimProfile::Coarse), 7.0);
        assert!(
            ship.hit_radius_for(AimProfile::Coarse) >= ship.hit_radius_for(AimProfile::Precise)
        );
    }

    #[test]
    fn the_five_ship_hit_radii_collapsed_to_one() {
        // Guards the whole point of the exercise: there is exactly one ship hit
        // radius, and the boss has exactly one hitbox radius.
        let rules = Rules::DEFAULT;
        assert_eq!(rules.ship.hit_radius, 6.0);
        assert_eq!(rules.weapons.boss_hitbox_radius, 28.0);
    }

    #[test]
    fn respawn_delay_matches_the_server_not_the_client() {
        // 2.0 (server/index.js RESPAWN_DELAY_MS) not 2.5 (main.js
        // RESPAWN_DELAY). Also equal to the spawn invulnerability window, so a
        // respawned ship's protection expires exactly when it could next die.
        let combat = CombatRules::DEFAULT;
        assert_eq!(combat.respawn_delay, 2.0);
        assert_eq!(combat.respawn_delay, combat.spawn_invuln);
    }

    #[test]
    fn boss_hitbox_tables_agree_on_length() {
        assert_eq!(BOSS_HITBOX_OFFSETS.len(), BOSS_HITBOX_COUNT);
        assert_eq!(BOSS_TURRET_PIVOTS.len(), BOSS_TURRET_COUNT);
        // The centre hitbox is the one the HUD reads the boss HP from
        // (main.js:2722 reads remotePlayers.get(BOSS_ID_BASE)).
        assert_eq!(BOSS_HITBOX_OFFSETS[9], Vec3::ZERO);
    }

    #[test]
    fn trial_checkpoint_rings_grow_with_difficulty() {
        let lengths: Vec<usize> = (1..=4).map(|n| trial_checkpoints(n).len()).collect();
        assert_eq!(lengths, vec![12, 14, 16, 18]);
        // Unknown trial numbers fall back to trial 1, like the JS ternary chain.
        assert_eq!(trial_checkpoints(0).len(), 12);
        assert_eq!(trial_checkpoints(99).len(), 12);
    }

    #[test]
    fn campaign_waves_escalate_across_missions() {
        for mission in 1..=3u8 {
            let waves = campaign_waves(mission);
            assert_eq!(waves.len(), 3);
            // Wave 2 is always the biggest push.
            assert!(waves[1].count > waves[0].count);
            assert!(waves[1].count > waves[2].count);
        }
        // Later missions are never easier than earlier ones.
        for i in 0..3 {
            assert!(CAMPAIGN_WAVES_M2[i].count > CAMPAIGN_WAVES_M1[i].count);
            assert!(CAMPAIGN_WAVES_M3[i].count > CAMPAIGN_WAVES_M2[i].count);
        }
        assert_eq!(campaign_waves(0), &CAMPAIGN_WAVES_M1);
    }

    #[test]
    fn bot_difficulty_switches_pick_the_harder_value() {
        let bot = BotRules::DEFAULT;
        assert!(bot.fire_cooldown_for(true) < bot.fire_cooldown_for(false));
        assert!(bot.missile_max_for(true) > bot.missile_max_for(false));
    }

    #[test]
    fn aim_assist_profiles_are_distinct_and_selectable() {
        let assist = AimAssistRules::DEFAULT;
        assert_eq!(assist.tuning(AimProfile::Precise).cone_dot, 0.60);
        assert_eq!(assist.tuning(AimProfile::Coarse).cone_dot, 0.5);
        // Coarse aim opens a wider cone (lower dot) but nudges less hard.
        assert!(
            assist.tuning(AimProfile::Coarse).cone_dot
                < assist.tuning(AimProfile::Precise).cone_dot
        );
        assert!(
            assist.tuning(AimProfile::Coarse).strength
                < assist.tuning(AimProfile::Precise).strength
        );
        assert_eq!(AimProfile::default(), AimProfile::Precise);
    }

    #[test]
    fn match_duration_is_shorter_in_training() {
        let rules = Rules::DEFAULT;
        assert_eq!(rules.match_duration(true), 180.0);
        assert_eq!(rules.match_duration(false), 300.0);
    }

    #[test]
    fn rules_stay_a_plain_copyable_value() {
        // `Rules` is copied into every World and read on every hot path; if it
        // ever grows a heap allocation that becomes a per-tick cost and a
        // determinism hazard (allocation order is not part of the sim).
        fn assert_copy<T: Copy>() {}
        assert_copy::<Rules>();
        let mut a = Rules::DEFAULT;
        let b = a;
        a.ship.max_hp = 1;
        assert_eq!(a.ship.max_hp, 1);
        assert_eq!(b.ship.max_hp, 100, "copy must not alias");
    }
}
