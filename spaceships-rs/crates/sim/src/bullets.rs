//! Bullet ballistics, the beam, and projectile hit resolution.
//!
//! Ports `public/src/bullets.js` (bolt flight and impact), `public/src/beams.js`
//! plus the beam raycast in `main.js` (`castWorldRay`, `raySphereDist`,
//! `BEAM_SHIP_RADIUS`), and the firing block at `main.js:1461`–`:1527` that
//! decides when either is allowed to go off.
//!
//! # The bug this module exists to fix
//!
//! `bullets.js:92` moves a bolt and `bullets.js:102` then asks whether its new
//! *point* is inside a target sphere. At `BULLET_SPEED = 780` and the 0.05 s
//! step the shadow bot simulation runs at, one frame moves a bullet 39 units,
//! while ships are 12–14 units across and small asteroids 10–14. The sampled
//! point lands inside a target on roughly a third of the passes and the bullet
//! goes straight through on the rest. A swept test *does* exist at
//! `bullets.js:75`, but it is wired to the `obstacles` list (the moon) and
//! nothing else.
//!
//! Every hit query here is a swept one, from [`crate::collision`], over the
//! segment the projectile actually covers. `collision.rs` pins the 780 u/s case
//! in `bullet_tunnels_through_a_ship_that_the_per_frame_point_test_never_sees`;
//! [`tests::a_bullet_that_the_js_point_test_misses_now_lands`] pins the same
//! case through this module's integration loop.
//!
//! # Earliest hit wins
//!
//! `bullets.js` tests categories in a fixed order — every asteroid, then every
//! obstacle, then every ship — and the first contact it stumbles on consumes the
//! bullet. Distance is not consulted. A shot that crosses a ship at 5 units and
//! an asteroid at 30 damages the *asteroid*, and within the asteroid list it
//! damages whichever rock sits later in the array, because `bullets.js:96`
//! walks it backwards.
//!
//! [`resolve_impact`] instead collects a candidate `t` — the fraction of the
//! step at which contact occurs — from every category and applies the smallest.
//! Exact ties (measure-zero, but they must not be resolved by luck) fall back to
//! a fixed category order, then to the lowest index within the category. See
//! [`resolve_impact`] for the order and why it is that one.
//!
//! # Ordering requirement
//!
//! [`step`] treats each [`crate::world::Ship`] as moving through the step, at
//! `vel * dt`, from the pose in [`crate::world::Ship::pos`]
//! ([`crate::collision::swept_sphere_vs_moving_sphere`]). That makes `pos` the
//! *start-of-step* pose, so **[`step`] must run before ship integration in the
//! tick.** A `Ship` carries no `prev_pos` (unlike [`crate::world::Bullet`], which
//! does), so if ships move first there is no way to recover where they started;
//! the sweep then spans the step *after* the one it should, and the error is
//! bounded by one step of ship displacement (~1.3 units at 60 Hz and
//! `max_throttle`). That is no worse than the JS, which tests against
//! end-of-frame positions outright, but it is worth not paying.
//!
//! # Determinism
//!
//! Only `+ - * /`, `sqrt`, `min`/`max` and comparisons, all inherited from
//! [`crate::collision`] and [`crate::math`], all IEEE-754 exact. No
//! transcendentals: the muzzle offset is applied through a caller-supplied
//! orthonormal basis ([`ShipBasis`]) rather than by rotating a quaternion here,
//! and the beam is a zero-radius sweep rather than a trigonometric cone. No
//! allocation beyond the output vectors, no map iteration, no clock.

use crate::collision::{
    sweep_first_hit, swept_sphere_aabb, swept_sphere_sphere, swept_sphere_vs_moving_sphere, Aabb,
    Sphere,
};
use crate::math::Vec3;
use crate::rules::Rules;
use crate::world::{
    is_boss_hitbox, Authority, Bullet, EntityId, ExplosionKind, GunMode, NetIntent, Ship, ShipKind,
    SimEvent, Team, WeaponKind, World,
};

// ---------------------------------------------------------------------------
// Cosmetic sizes
// ---------------------------------------------------------------------------
//
// These three are the `spawnExplosion` scales in `bullets.js`. They are
// render-only — nothing in the simulation reads them back — which is why they
// are consts here rather than fields on `WeaponRules`. They are named so that no
// other module invents a second set. If explosion sizing ever becomes a tunable,
// they belong in `rules.rs`.

/// Explosion scale for a bullet landing on a ship or a boss hull.
/// `bullets.js:146` (`spawnExplosion(pos, 1.0)`).
pub const EXPLOSION_SCALE_SHIP: f64 = 1.0;

/// Explosion scale for a bullet landing on the moon or a mothership.
/// `bullets.js:131` (`spawnExplosion(pos, 0.4)`).
pub const EXPLOSION_SCALE_OBSTACLE: f64 = 0.4;

/// Explosion scale for a bullet landing on an asteroid, as a fraction of the
/// rock's collision radius. `bullets.js:103` (`spawnExplosion(pos, a.radius *
/// 0.25)`).
///
/// The beam uses a flat `0.6` in the JS (`main.js:1505`); it uses this instead,
/// so one impact does not change size with the weapon that caused it.
pub const ASTEROID_IMPACT_SCALE: f64 = 0.25;

// ---------------------------------------------------------------------------
// Ship-local axes
// ---------------------------------------------------------------------------

/// A ship's local axes expressed in world space.
///
/// `main.js:1467` builds a muzzle as `off.clone().applyQuaternion(q).add(pos)`.
/// Rotating a vector by a unit quaternion *is* mapping it through the
/// orthonormal frame that quaternion denotes, so passing the frame is exactly
/// equivalent and keeps quaternion algebra out of this module —
/// [`crate::world::Quat`] is deliberately storage-only and [`crate::math`] has no
/// quaternion type, so there is nowhere to do the rotation without inventing one.
///
/// The three axes are expected to be orthonormal and right-handed
/// (`right × up == forward` in the game's space). Nothing here checks that; a
/// non-orthonormal basis simply produces a skewed muzzle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShipBasis {
    /// Local `+x` in world space.
    pub right: Vec3,
    /// Local `+y` in world space.
    pub up: Vec3,
    /// Local `+z` in world space — the direction the ship flies and fires.
    pub forward: Vec3,
}

impl ShipBasis {
    /// The world axes: a ship with identity orientation.
    pub const IDENTITY: ShipBasis = ShipBasis {
        right: Vec3::X,
        up: Vec3::Y,
        forward: Vec3::Z,
    };

    /// A basis in which only `forward` is meaningful.
    ///
    /// `right` and `up` are left as the world axes, so this is correct for any
    /// local offset whose `x` and `y` are zero — which
    /// [`crate::rules::WeaponRules::muzzle_offset`] is today, at `(0, 0, 0.6)` —
    /// and wrong for anything else. Use it when the caller has a facing but not a
    /// full frame; use the struct literal when it has one.
    #[must_use]
    pub fn along(forward: Vec3) -> ShipBasis {
        ShipBasis {
            forward,
            ..ShipBasis::IDENTITY
        }
    }

    /// Maps a ship-local vector into world space.
    #[must_use]
    pub fn transform(self, local: Vec3) -> Vec3 {
        self.right * local.x + self.up * local.y + self.forward * local.z
    }
}

impl Default for ShipBasis {
    fn default() -> Self {
        ShipBasis::IDENTITY
    }
}

/// Where a ship's gun muzzle sits in world space.
///
/// `main.js:1467`. One function, so the bullet, the beam, and the HUD reticle
/// cast (`main.js:1805`) cannot drift apart the way the JS lets them.
#[must_use]
pub fn muzzle_origin(pos: Vec3, basis: ShipBasis, rules: &Rules) -> Vec3 {
    pos + basis.transform(rules.weapons.muzzle_offset)
}

// ---------------------------------------------------------------------------
// Caller-supplied hulls
// ---------------------------------------------------------------------------

/// Extra volumes a projectile collides with, supplied by the caller.
///
/// This exists so the campaign capital ship can be whatever shape
/// `campaign.rs` decides it is. The JS models it as 20 spheres of radius 28 on a
/// grid 75 units apart in z (`main.js` `BOSS_HB_OFFSETS_WORLD`,
/// [`crate::rules::BOSS_HITBOX_OFFSETS`]) — a layout with 19-unit gaps between
/// the z rows that a bullet can thread while visually inside the hull. Whether
/// that stays, gains rows, or becomes a box is not this module's call, so this
/// module only asks for volumes.
///
/// Two slices rather than one slice of an enum, because a boss built from
/// spheres is then literally a `&[Sphere]` and goes straight into
/// [`crate::collision::sweep_first_hit`], the allocation-free earliest-hit
/// broadphase, with no repacking.
///
/// A boss modelled instead as [`crate::world::Ship`] records with
/// `hit_radius_override` (ids [`crate::rules::BOSS_ID_BASE`]`..`) needs nothing
/// here: those are hit through the ordinary ship path and reported as
/// [`HullPart::Hitbox`]. Both representations end up in
/// [`BulletOutput::hull_hits`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HullVolumes<'a> {
    /// Spherical parts.
    pub spheres: &'a [Sphere],
    /// Box parts.
    pub boxes: &'a [Aabb],
}

impl HullVolumes<'_> {
    /// No hull at all — every mode except the campaign boss fight.
    pub const EMPTY: HullVolumes<'static> = HullVolumes {
        spheres: &[],
        boxes: &[],
    };

    /// True when there is nothing to test against.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.spheres.is_empty() && self.boxes.is_empty()
    }
}

/// Which piece of a caller-supplied hull, or which boss-hitbox ship, was struck.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HullPart {
    /// Index into [`HullVolumes::spheres`].
    Sphere(usize),
    /// Index into [`HullVolumes::boxes`].
    Box(usize),
    /// A ship in [`World::ships`] whose id is in the boss range
    /// ([`crate::world::is_boss_hitbox`]).
    ///
    /// These are *not* damaged as ships. `main.js:2719` (`applyBossHit`)
    /// decrements one shared `bossHp` and only mirrors it onto hitbox 0 for the
    /// HUD, so running the ordinary ship damage path over them would "kill"
    /// individual hitboxes at 100 HP each. The hit is reported and
    /// `campaign.rs` decides what it means.
    Hitbox(EntityId),
}

/// One projectile impact on a hull, for `campaign.rs` to apply.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HullHit {
    /// Which piece.
    pub part: HullPart,
    /// Where contact occurred.
    pub pos: Vec3,
    /// Damage the projectile carried.
    pub damage: i32,
    /// Who fired it.
    pub owner: EntityId,
    /// Which weapon landed it.
    pub weapon: WeaponKind,
}

/// Sweeps a moving sphere against a hull and returns the earliest contact.
///
/// Spheres are tested through [`crate::collision::sweep_first_hit`], boxes
/// through [`crate::collision::swept_sphere_aabb`]. On an exact tie the sphere
/// wins, then the lowest index — the same rule [`resolve_impact`] uses, for the
/// same reason.
///
/// Public because `campaign.rs` may want the query without a bullet attached
/// (a turret checking line of sight, say).
#[must_use]
pub fn sweep_hulls(
    origin: Vec3,
    motion: Vec3,
    radius: f64,
    hulls: HullVolumes<'_>,
) -> Option<(HullPart, f64)> {
    let mut best: Option<(HullPart, f64)> = None;
    if let Some((i, t)) = sweep_first_hit(origin, motion, radius, hulls.spheres) {
        best = Some((HullPart::Sphere(i), t));
    }
    for (i, b) in hulls.boxes.iter().enumerate() {
        if let Some(t) = swept_sphere_aabb(origin, motion, radius, *b) {
            match best {
                Some((_, bt)) if t >= bt => {}
                _ => best = Some((HullPart::Box(i), t)),
            }
        }
    }
    best
}

// ---------------------------------------------------------------------------
// The query
// ---------------------------------------------------------------------------

/// One swept projectile query: the body, the step it covers, and who owns it.
///
/// Bundled rather than passed as eight arguments so the same value serves a
/// bullet, a beam, and (should `missiles.rs` want it) a missile, and so adding
/// a field is not a signature change at every call site.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sweep {
    /// Position at the start of the step.
    pub origin: Vec3,
    /// Displacement over the whole step, i.e. `velocity * dt`. For a beam this
    /// is `direction * beam_range`.
    pub motion: Vec3,
    /// The projectile's own collision radius, added to every target's.
    /// [`crate::rules::WeaponRules::bullet_radius`] for a bullet, `0` for the
    /// beam, which is a ray.
    pub radius: f64,
    /// How much simulated time [`Self::motion`] spans.
    ///
    /// Ships are displaced by `vel * step_dt` during the sweep. Zero for the
    /// beam, which is hitscan and resolves instantaneously — and
    /// `swept_sphere_vs_moving_sphere` with a zero target motion reduces
    /// bit-for-bit to the stationary sweep, so that costs nothing.
    pub step_dt: f64,
    /// Who fired. Never hits itself (`server/index.js:940`).
    pub owner: EntityId,
    /// The owner's team *at launch*, so a mid-flight team change cannot turn a
    /// shot friendly.
    pub owner_team: Option<Team>,
    /// Whether the owner had coarse aim, which widens every target ship by
    /// [`crate::rules::ShipRules::hit_radius_coarse_aim_bonus`]. A property of
    /// the shooter, never of the target.
    pub owner_coarse_aim: bool,
}

impl Sweep {
    /// The query for one bullet advancing `dt` seconds.
    #[must_use]
    pub fn from_bullet(bullet: &Bullet, dt: f64) -> Sweep {
        Sweep {
            origin: bullet.pos,
            motion: bullet.vel * dt,
            radius: 0.0, // filled by the caller from the rules; see `step`.
            step_dt: dt,
            owner: bullet.owner,
            owner_team: bullet.owner_team,
            owner_coarse_aim: bullet.owner_coarse_aim,
        }
    }

    /// The query for a beam fired from `origin` along unit `dir`.
    ///
    /// A ray of length [`crate::rules::WeaponRules::beam_range`] and zero
    /// radius, which is exactly what `raySphereDist` (`main.js:1074`) computes —
    /// with one deliberate difference, documented on [`cast_beam`].
    #[must_use]
    pub fn beam(origin: Vec3, dir: Vec3, shooter: &Ship, rules: &Rules) -> Sweep {
        Sweep {
            origin,
            motion: dir * rules.weapons.beam_range,
            radius: 0.0,
            step_dt: 0.0,
            owner: shooter.id,
            owner_team: shooter.team,
            owner_coarse_aim: shooter.coarse_aim,
        }
    }
}

/// What a projectile hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A ship, by index into [`World::ships`] and by id. The index is valid only
    /// until the next mutation of that vector.
    Ship {
        /// Position in [`World::ships`].
        index: usize,
        /// Entity id.
        id: EntityId,
    },
    /// An asteroid, by index into [`World::asteroids`] and by id.
    Asteroid {
        /// Position in [`World::asteroids`].
        index: usize,
        /// Asteroid id.
        id: u32,
    },
    /// An indestructible sphere — the moon. Index into [`World::obstacles`].
    Obstacle {
        /// Position in [`World::obstacles`].
        index: usize,
    },
    /// A solid box — a mothership or an airfield. Index into [`World::boxes`].
    BoxVolume {
        /// Position in [`World::boxes`].
        index: usize,
    },
    /// A caller-supplied hull volume.
    Hull {
        /// Which piece.
        part: HullPart,
    },
}

/// The earliest contact a [`Sweep`] makes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Impact {
    /// What was hit.
    pub target: Target,
    /// Fraction of the step at which contact occurs, in `[0, 1]`.
    pub t: f64,
    /// Contact position, `sweep.origin + sweep.motion * t`.
    pub pos: Vec3,
}

/// Resolves a swept projectile against the whole world and returns the
/// **earliest** contact.
///
/// Replaces `bullets.js:88`–`:152` (bullet) and `castWorldRay`
/// (`main.js:1042`, beam) with one query, so the two weapons cannot disagree
/// about what is solid.
///
/// # What blocks, and what takes damage
///
/// These are two different questions and the JS answers them in two places.
///
/// - **Skipped entirely** (the projectile passes through): the owner, a ship on
///   the owner's team, a dead ship, an asteroid already at zero hit points.
///   `bullets.js:139`–`:140` and `main.js:1049`–`:1050` both `continue` past
///   these rather than consuming the shot, so a friendly in the line of fire is
///   not cover.
/// - **Blocks but takes no damage**: a ship inside its spawn-protection window.
///   `bullets.js` checks only `r.alive`, so the bolt is consumed and the hit
///   report the client sends is then rejected by `server/index.js:942`. Damage
///   is gated separately, on [`World::can_damage`], which also covers the
///   invulnerability the JS grants to the local player only.
/// - **Blocks and takes damage**: everything else.
///
/// Only the first of those three is decided here; the caller applies the rest.
///
/// # Tie-breaking
///
/// Candidates are gathered from five categories and the smallest `t` wins. An
/// exact tie falls back to this category order, then to the lowest index inside
/// the category:
///
/// 1. asteroids, 2. obstacles, 3. boxes, 4. hulls, 5. ships.
///
/// The first, second and fifth reproduce the order `bullets.js` scans in, so in
/// the (measure-zero) tie case the port picks what the JS picked. The point is
/// not that this order is better than another — it is that the answer depends
/// only on the contents of the world, never on iteration luck.
///
/// Note the difference from the JS beam, whose `castWorldRay` scans ships,
/// then asteroids, then obstacles with a strict `<`, i.e. the exact reverse.
/// Two orders for two weapons is one more than is defensible; this is the one.
#[must_use]
pub fn resolve_impact(world: &World, sweep: &Sweep, hulls: HullVolumes<'_>) -> Option<Impact> {
    // Broadphase: everything this sweep can touch lies within `half_len +
    // radius` of the segment's midpoint. One subtraction, one dot and one
    // compare rejects a candidate before its quadratic is set up. Conservative,
    // so it cannot change the answer.
    let mid = sweep.origin + sweep.motion * 0.5;
    let half_len = sweep.motion.length() * 0.5;
    let reach_base = half_len + sweep.radius;

    let mut best: Option<(Target, f64)> = None;
    let mut keep = |target: Target, t: f64| match best {
        // Strict: an exact tie keeps the candidate offered first, which is what
        // makes the category order above load-bearing.
        Some((_, bt)) if t >= bt => {}
        _ => best = Some((target, t)),
    };

    // 1. Asteroids. Stationary: `asteroids.js` only spins them.
    for (i, a) in world.asteroids.iter().enumerate() {
        if a.hp <= 0 {
            continue;
        }
        let reach = reach_base + a.radius;
        if mid.distance_squared(a.pos) > reach * reach {
            continue;
        }
        let rock = Sphere::new(a.pos, a.radius);
        if let Some(t) = swept_sphere_sphere(sweep.origin, sweep.motion, sweep.radius, rock) {
            keep(Target::Asteroid { index: i, id: a.id }, t);
        }
    }

    // 2. Obstacles — the moon.
    for (i, o) in world.obstacles.iter().enumerate() {
        let reach = reach_base + o.radius;
        if mid.distance_squared(o.pos) > reach * reach {
            continue;
        }
        let sphere = Sphere::new(o.pos, o.radius);
        if let Some(t) = swept_sphere_sphere(sweep.origin, sweep.motion, sweep.radius, sphere) {
            keep(Target::Obstacle { index: i }, t);
        }
    }

    // 3. Boxes — motherships and airfields.
    //
    // New: `main.js:1644` hands `bullets.update` the *sphere* obstacle list and
    // nothing else, so in the JS a bullet flies straight through a mothership
    // hull. The box sweep is exact at edges and corners, so this does not put a
    // shell of phantom wall around each platform the way a grown-box ray test
    // would.
    for (i, b) in world.boxes.iter().enumerate() {
        let aabb = Aabb::new(b.pos, b.half);
        if let Some(t) = swept_sphere_aabb(sweep.origin, sweep.motion, sweep.radius, aabb) {
            keep(Target::BoxVolume { index: i }, t);
        }
    }

    // 4. Caller-supplied hulls — the campaign capital ship.
    if let Some((part, t)) = sweep_hulls(sweep.origin, sweep.motion, sweep.radius, hulls) {
        keep(Target::Hull { part }, t);
    }

    // 5. Ships, moving.
    for (i, s) in world.ships.iter().enumerate() {
        if !s.alive || s.id == sweep.owner {
            continue;
        }
        if let (Some(a), Some(b)) = (sweep.owner_team, s.team) {
            if a == b {
                continue;
            }
        }
        let hit_radius = s.hit_radius(&world.rules, sweep.owner_coarse_aim);
        let motion = s.vel * sweep.step_dt;
        // Widen the cull by the target's own displacement, so a ship crossing
        // the segment is not culled at its start position.
        let reach = reach_base + hit_radius + motion.length();
        if mid.distance_squared(s.pos) > reach * reach {
            continue;
        }
        let hull = Sphere::new(s.pos, hit_radius);
        if let Some(t) =
            swept_sphere_vs_moving_sphere(sweep.origin, sweep.motion, sweep.radius, hull, motion)
        {
            keep(Target::Ship { index: i, id: s.id }, t);
        }
    }

    best.map(|(target, t)| Impact {
        target,
        t,
        pos: sweep.origin + sweep.motion * t,
    })
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// Everything one bullet or beam step produced that the caller must route.
///
/// Three lists rather than three out-parameters, so the signatures stay short
/// and a caller can keep one of these across ticks and [`Self::clear`] it,
/// reaching a steady state where no tick allocates. Same bargain as
/// [`crate::world::Frame`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BulletOutput {
    /// Explosions, damage, deaths — for particles, audio and the HUD.
    pub events: Vec<SimEvent>,
    /// Messages the transport should send. Populated only when
    /// [`World::authority`] is [`Authority::Server`].
    pub net_out: Vec<NetIntent>,
    /// Impacts on the campaign boss, in the order they were resolved. Damage is
    /// deliberately *not* applied: `campaign.rs` owns the boss's hit points.
    pub hull_hits: Vec<HullHit>,
}

impl BulletOutput {
    /// An empty output.
    #[must_use]
    pub fn new() -> BulletOutput {
        BulletOutput::default()
    }

    /// Empties every list but keeps the allocations.
    pub fn clear(&mut self) {
        self.events.clear();
        self.net_out.clear();
        self.hull_hits.clear();
    }

    /// True when nothing happened.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty() && self.net_out.is_empty() && self.hull_hits.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Spawning
// ---------------------------------------------------------------------------

/// One bullet to be created.
///
/// Speed, life and damage are explicit rather than read from the rules inside
/// [`spawn_bullet`], because the boss's turret rounds share this list: they are
/// slower ([`crate::rules::WeaponRules::boss_bullet_speed`]), longer-lived, and
/// hit harder than a player's. `main.js:2327` keeps them in a second array
/// (`bossBullets`) with a duplicate integrator; there is one list here. Use the
/// constructors rather than filling the fields by hand.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BulletSpawn {
    /// Muzzle position.
    pub origin: Vec3,
    /// Unit direction of travel. Normalised on spawn, so a caller handing over a
    /// scaled vector does not get a fast bullet.
    pub dir: Vec3,
    /// Muzzle speed.
    pub speed: f64,
    /// Lifetime in seconds.
    pub life: f64,
    /// Who fired it.
    pub owner: EntityId,
    /// The owner's team at launch.
    pub owner_team: Option<Team>,
    /// Whether the owner had coarse aim.
    pub owner_coarse_aim: bool,
    /// Damage on impact.
    pub damage: i32,
}

impl BulletSpawn {
    /// A round from a ship's gun.
    ///
    /// `bullets.js:44`: velocity is `direction * BULLET_SPEED` and nothing else
    /// — the shooter's own velocity is **not** inherited. That is deliberate
    /// here too: it is what makes a bolt's flight path identical for the shooter
    /// and for every observer, given only the origin and the direction, which is
    /// exactly what the `fire` network message carries (`main.js:1517`).
    #[must_use]
    pub fn gun(rules: &Rules, origin: Vec3, dir: Vec3, shooter: &Ship) -> BulletSpawn {
        BulletSpawn {
            origin,
            dir,
            speed: rules.weapons.bullet_speed,
            life: rules.weapons.bullet_life,
            owner: shooter.id,
            owner_team: shooter.team,
            owner_coarse_aim: shooter.coarse_aim,
            damage: rules.weapons.gun_damage,
        }
    }

    /// A round from a capital-ship turret. `main.js:2688`.
    #[must_use]
    pub fn boss_turret(
        rules: &Rules,
        origin: Vec3,
        dir: Vec3,
        owner: EntityId,
        owner_team: Option<Team>,
    ) -> BulletSpawn {
        BulletSpawn {
            origin,
            dir,
            speed: rules.weapons.boss_bullet_speed,
            life: rules.weapons.boss_bullet_life,
            owner,
            owner_team,
            owner_coarse_aim: false,
            damage: rules.weapons.boss_bullet_damage,
        }
    }
}

/// Adds a bullet to the world and returns its stable key.
///
/// `prev_pos` starts equal to `pos`, so a bullet that is resolved before it has
/// ever moved sweeps a zero-length segment rather than a garbage one.
pub fn spawn_bullet(world: &mut World, spawn: BulletSpawn) -> u64 {
    let key = world.take_projectile_key();
    let dir = spawn.dir.normalize();
    world.bullets.push(Bullet {
        key,
        pos: spawn.origin,
        prev_pos: spawn.origin,
        vel: dir * spawn.speed,
        life: spawn.life,
        owner: spawn.owner,
        owner_team: spawn.owner_team,
        owner_coarse_aim: spawn.owner_coarse_aim,
        damage: spawn.damage,
    });
    key
}

// ---------------------------------------------------------------------------
// Integration
// ---------------------------------------------------------------------------

/// Advances every bullet by `dt` and resolves the earliest thing each one hits.
///
/// Must run **before** ship integration; see the module docs.
///
/// Per bullet, in [`World::bullets`] order:
///
/// 1. `prev_pos` takes the current `pos`.
/// 2. The bullet travels `vel * min(dt, life)`. Clamping the travel to the
///    remaining lifetime is a small fix on `bullets.js:92`–`:94`, which moves a
///    full step, *then* expires the bullet, and skips the hit test entirely — so
///    a bolt in its last frame passes harmlessly through whatever it was about
///    to hit.
/// 3. [`resolve_impact`] finds the earliest contact over that segment.
/// 4. Damage is applied and the bullet is consumed, or it survives to the next
///    step.
///
/// Bullets are removed in place, preserving order. Survivors keep their indices'
/// relative order, so the list never depends on how many bullets died.
pub fn step(world: &mut World, dt: f64, hulls: HullVolumes<'_>, out: &mut BulletOutput) {
    // Moved out so the per-bullet resolution can take `&mut World`. Nothing in
    // the loop reads `world.bullets`, and nothing spawns into it.
    let mut bullets = core::mem::take(&mut world.bullets);
    let mut write = 0usize;
    for read in 0..bullets.len() {
        let mut bullet = bullets[read];
        if advance_bullet(world, &mut bullet, dt, hulls, out) {
            continue;
        }
        bullets[write] = bullet;
        write += 1;
    }
    bullets.truncate(write);
    world.bullets = bullets;
}

/// Advances one bullet. Returns whether it was consumed.
fn advance_bullet(
    world: &mut World,
    bullet: &mut Bullet,
    dt: f64,
    hulls: HullVolumes<'_>,
    out: &mut BulletOutput,
) -> bool {
    let travel_dt = if bullet.life < dt { bullet.life } else { dt };
    let mut sweep = Sweep::from_bullet(bullet, travel_dt);
    sweep.radius = world.rules.weapons.bullet_radius;

    let impact = resolve_impact(world, &sweep, hulls);

    bullet.prev_pos = bullet.pos;
    bullet.life -= dt;
    match impact {
        Some(hit) => {
            // Park the bullet at the contact point. Nothing reads it after this
            // step, but a renderer that samples the frame the bullet died on
            // should see it where it went off, not past its target.
            bullet.pos = hit.pos;
            apply_impact(
                world,
                hit,
                bullet.damage,
                bullet.owner,
                WeaponKind::Bullet,
                out,
            );
            true
        }
        None => {
            bullet.pos = sweep.origin + sweep.motion;
            bullet.life <= 0.0
        }
    }
}

/// Applies one resolved impact: damage, events, and the hit report.
///
/// Shared by bullets and the beam so a beam hit and a bullet hit cannot mean
/// different things. `main.js` writes the two out separately at `:1489`–`:1509`
/// and `:1624`–`:1642`, which is how the beam ended up with a boss test the
/// bullet does not have.
fn apply_impact(
    world: &mut World,
    impact: Impact,
    damage: i32,
    owner: EntityId,
    weapon: WeaponKind,
    out: &mut BulletOutput,
) {
    let report = should_report(world, owner);
    match impact.target {
        Target::Ship { id, .. } => {
            explode(out, impact.pos, EXPLOSION_SCALE_SHIP, ExplosionKind::Impact);
            if is_boss_hitbox(id) {
                // The boss is one hit-point pool behind twenty hitboxes.
                out.hull_hits.push(HullHit {
                    part: HullPart::Hitbox(id),
                    pos: impact.pos,
                    damage,
                    owner,
                    weapon,
                });
                return;
            }
            apply_ship_damage(world, id, damage, Some(owner), out);
            if report {
                out.net_out.push(NetIntent::Hit {
                    target: id,
                    weapon,
                    from_bot: locally_driven_bot(world, owner),
                });
            }
        }
        Target::Asteroid { id, .. } => {
            let scale = world
                .asteroid(id)
                .map_or(0.0, |a| a.radius * ASTEROID_IMPACT_SCALE);
            explode(out, impact.pos, scale, ExplosionKind::Impact);
            apply_asteroid_damage(world, id, out);
            if report {
                out.net_out.push(NetIntent::AsteroidHit { id });
            }
        }
        Target::Obstacle { .. } | Target::BoxVolume { .. } => {
            explode(
                out,
                impact.pos,
                EXPLOSION_SCALE_OBSTACLE,
                ExplosionKind::Impact,
            );
        }
        Target::Hull { part } => {
            explode(out, impact.pos, EXPLOSION_SCALE_SHIP, ExplosionKind::Impact);
            out.hull_hits.push(HullHit {
                part,
                pos: impact.pos,
                damage,
                owner,
                weapon,
            });
        }
    }
}

fn explode(out: &mut BulletOutput, pos: Vec3, scale: f64, kind: ExplosionKind) {
    out.events.push(SimEvent::Explosion { pos, scale, kind });
}

/// Whether a hit landed by `owner` should be reported to the server.
///
/// Only when the server is authoritative *and* the shot came from something
/// simulated on this machine — the local player or a bot this client drives.
/// A remote player's bullet is simulated here for display; claiming its hits
/// would double-count.
fn should_report(world: &World, owner: EntityId) -> bool {
    world.authority == Authority::Server
        && world
            .ship(owner)
            .is_some_and(|s| matches!(s.kind, ShipKind::Local | ShipKind::Bot))
}

/// `Some(id)` when the shot came from a bot this client drives, which the server
/// wants spelled out (`server/index.js:917`, `fromBotId`).
fn locally_driven_bot(world: &World, owner: EntityId) -> Option<EntityId> {
    world
        .ship(owner)
        .filter(|s| s.kind == ShipKind::Bot)
        .map(|s| s.id)
}

// ---------------------------------------------------------------------------
// Damage
// ---------------------------------------------------------------------------

/// Applies weapon damage to a ship, and handles the death that may follow.
///
/// Returns whether the damage landed. It does not when the target is dead,
/// inside its spawn-protection window, or absent — the same three conditions
/// [`World::can_damage`] tests, minus the friendly-fire clause, which
/// [`resolve_impact`] has already applied by refusing to report the contact at
/// all.
///
/// **One damage path.** The JS has three that must agree and do not:
/// `applyHitToBot` (`main.js:3199`) never checks invulnerability because bot
/// records have no such field; `applyPlayerDamageLocal` (`main.js:3231`) does;
/// `server/index.js:942` does. Missiles and collision damage should call this
/// too rather than growing a fourth.
///
/// Respawn uses [`crate::rules::CombatRules::respawn_delay`] for every ship.
/// The campaign's warp-in respawn
/// ([`crate::rules::CombatRules::campaign_respawn_delay`], `main.js:3253`)
/// applies to the local player only and is `campaign.rs`'s to schedule.
pub fn apply_ship_damage(
    world: &mut World,
    target: EntityId,
    amount: i32,
    source: Option<EntityId>,
    out: &mut BulletOutput,
) -> bool {
    let Some(index) = world.ships.iter().position(|s| s.id == target) else {
        return false;
    };
    if !world.ships[index].is_damageable() {
        return false;
    }

    let respawn_delay = world.rules.combat.respawn_delay;
    let ship = &mut world.ships[index];
    ship.hp = (ship.hp - amount).max(0);
    ship.hit_flash = 1.0;
    // Being shot restarts the health-regeneration clock (`main.js:3235`).
    ship.health_idle_damage = 0.0;
    let new_hp = ship.hp;
    let killed = new_hp <= 0;
    let pos = ship.pos;
    let victim_is_bot = ship.kind == ShipKind::Bot;

    out.events.push(SimEvent::Damaged {
        id: target,
        amount,
        new_hp,
        source,
    });
    if !killed {
        return true;
    }

    let ship = &mut world.ships[index];
    ship.alive = false;
    ship.respawn_timer = respawn_delay;
    out.events.push(SimEvent::ShipDestroyed {
        id: target,
        killer: source,
        pos,
    });

    // Scoring. `main.js:3205`–`:3220`.
    let killer_team = source.and_then(|k| world.ship(k)).and_then(|s| s.team);
    if world.match_state.active {
        if let Some(t) = killer_team {
            // Self-inflicted deaths score for nobody; a friendly-fire kill
            // cannot happen because `resolve_impact` never returns one.
            if Some(target) != source {
                world.match_state.team_kills[t.index()] += 1;
            }
        }
    }
    if let Some(killer) = source {
        if let Some(row) = world.match_state.scores.iter_mut().find(|r| r.id == killer) {
            row.kills += 1;
        }
        if world.mode.is_solo() && victim_is_bot && world.local_id == Some(killer) {
            world.match_state.solo_bots_killed += 1;
        }
    }
    if let Some(row) = world.match_state.scores.iter_mut().find(|r| r.id == target) {
        row.deaths += 1;
    }
    true
}

/// Applies one weapon impact to an asteroid.
///
/// Returns whether it landed. A rock already at zero is not hit again, matching
/// `damageAsteroidLocal` (`main.js:2183`).
///
/// Damage is always [`crate::rules::CombatRules::asteroid_damage_per_hit`]
/// regardless of weapon — a 50-damage missile and a 10-damage bullet chip a rock
/// identically, in `main.js:2183` and `server/index.js:822` alike.
///
/// A destroyed rock is left in [`World::asteroids`] with `hp == 0`; reaping it
/// is `asteroids.rs`'s job. [`resolve_impact`] already treats a zero-HP rock as
/// not there, so a second bullet in the same step passes through it, which is
/// what the JS gets from removing it from the list mid-frame.
pub fn apply_asteroid_damage(world: &mut World, id: u32, out: &mut BulletOutput) -> bool {
    let per_hit = world.rules.combat.asteroid_damage_per_hit;
    let Some(rock) = world.asteroid_mut(id) else {
        return false;
    };
    if rock.hp <= 0 {
        return false;
    }
    rock.hp = (rock.hp - per_hit).max(0);
    rock.hit_flash = 1.0;
    let hp = rock.hp;
    let pos = rock.pos;
    let radius = rock.radius;
    out.events.push(SimEvent::AsteroidDamaged { id, hp });
    if hp <= 0 {
        out.events
            .push(SimEvent::AsteroidDestroyed { id, pos, radius });
    }
    true
}

// ---------------------------------------------------------------------------
// The beam
// ---------------------------------------------------------------------------

/// What a beam cast found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BeamCast {
    /// Where the beam starts — the muzzle, not the visual start.
    pub origin: Vec3,
    /// Where it stops: the impact point, or the end of its range.
    pub end: Vec3,
    /// Length, in world units.
    pub dist: f64,
    /// What it hit, if anything.
    pub hit: Option<Target>,
}

impl BeamCast {
    /// Where the beam *visual* should start, so the cylinder does not clip the
    /// cockpit. `main.js:1485` (`BEAM_FORWARD_OFFSET`). Visual only — the hit
    /// test always starts at the muzzle.
    #[must_use]
    pub fn visual_start(&self, dir: Vec3, rules: &Rules) -> Vec3 {
        let offset = rules.weapons.beam_forward_offset;
        if self.dist > offset {
            self.origin + dir * offset
        } else {
            self.origin
        }
    }
}

/// Casts a beam and returns the first thing it meets.
///
/// The beam is hitscan: a zero-radius sweep over
/// [`crate::rules::WeaponRules::beam_range`], resolved by the same
/// [`resolve_impact`] that resolves bullets. Everything solid to a bullet is
/// solid to a beam.
///
/// # Deliberate differences from `castWorldRay` (`main.js:1042`)
///
/// - **Ships use [`crate::world::Ship::hit_radius`] (6.0), not the beam's own
///   `BEAM_SHIP_RADIUS` (5.5).** One weapon, one hitbox; see the divergence
///   table on [`crate::rules::ShipRules::hit_radius`].
/// - **The boss's free 95-unit proxy sphere is gone.** `main.js:1476` gives the
///   beam a single sphere of radius 95 at the capital ship's centre, so a beam
///   hits the boss from any angle while a bullet must find one of the 20
///   hitboxes. Both now go through [`HullVolumes`].
/// - **The `hasTarget` filter is gone.** `main.js:1049` skips remote ships that
///   have never sent a pose. Harmless for real remotes, but `activateBossPhase`
///   (`main.js:2743`) sets `hasTarget` on hitbox 0 only, which hid the other 19
///   from every beam — and is very likely *why* the 95-unit proxy was added.
/// - **A beam whose origin is already inside a target stops at distance 0.**
///   `raySphereDist` (`main.js:1074`) falls through to the far root and returns
///   the *exit* distance, i.e. a beam fired from inside a rock emerges from the
///   far side and continues. First contact is at the origin.
#[must_use]
pub fn cast_beam(world: &World, sweep: &Sweep, hulls: HullVolumes<'_>) -> BeamCast {
    let range = sweep.motion.length();
    match resolve_impact(world, sweep, hulls) {
        Some(impact) => BeamCast {
            origin: sweep.origin,
            end: impact.pos,
            dist: range * impact.t,
            hit: Some(impact.target),
        },
        None => BeamCast {
            origin: sweep.origin,
            end: sweep.origin + sweep.motion,
            dist: range,
            hit: None,
        },
    }
}

// ---------------------------------------------------------------------------
// Firing
// ---------------------------------------------------------------------------

/// Why a trigger pull did or did not produce a shot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum FireOutcome {
    /// A bullet was spawned.
    Bullet,
    /// A beam was cast.
    Beam,
    /// The gun is still cooling down.
    /// [`crate::rules::WeaponRules::bullet_cooldown`] or `beam_cooldown`.
    Cooling,
    /// Not enough ammo for the selected gun — the "overheated" state the HUD
    /// shows at `main.js:1343`.
    Overheated,
    /// The shooter is dead, or not in this world.
    Unavailable,
}

/// Pulls the trigger for `shooter`.
///
/// `main.js:1461`–`:1527`, which is one block for both guns. In order:
///
/// 1. The shooter must be alive.
/// 2. `fire_timer` must have run out — see [`tick_weapon_cooldown`].
/// 3. `ammo` must cover the cost: 1 for a bullet, 3 for a beam
///    ([`crate::rules::WeaponRules::beam_ammo_cost`]). Ammo *is* the heat gauge;
///    `camTel.heat01` is `ammo / MAX_AMMO` (`main.js:1911`) and the bar reads
///    "overheated" when the pool cannot cover the next shot (`main.js:1343`).
///    At 3 ammo per 0.25 s the beam sustains for 7.5 s from full before it locks
///    out; at 1 per 0.05 s the bullet sustains for 4.5 s. Regeneration
///    ([`regen_ammo`]) does not start until
///    [`crate::rules::WeaponRules::ammo_regen_delay`] after the last shot, so
///    holding the trigger never tops the pool up.
///
/// On success it spends the ammo, restarts both idle clocks (ammo regeneration
/// and health regeneration — firing suppresses healing, `main.js:1524`), reloads
/// the cooldown, and either spawns a bullet or resolves a beam immediately.
pub fn fire_gun(
    world: &mut World,
    shooter: EntityId,
    basis: ShipBasis,
    hulls: HullVolumes<'_>,
    out: &mut BulletOutput,
) -> FireOutcome {
    let rules = world.rules;
    let Some(index) = world.ships.iter().position(|s| s.id == shooter) else {
        return FireOutcome::Unavailable;
    };
    let ship = &world.ships[index];
    if !ship.alive {
        return FireOutcome::Unavailable;
    }
    let beam = ship.gun_mode == GunMode::Beam;
    let cost = if beam {
        rules.weapons.beam_ammo_cost
    } else {
        rules.weapons.bullet_ammo_cost
    };
    if ship.fire_timer > 0.0 {
        return FireOutcome::Cooling;
    }
    if ship.ammo < cost {
        return FireOutcome::Overheated;
    }

    let origin = muzzle_origin(ship.pos, basis, &rules);
    let dir = basis.forward.normalize();

    let ship = &mut world.ships[index];
    ship.ammo = (ship.ammo - cost).max(0.0);
    ship.ammo_idle = 0.0;
    ship.health_idle_shot = 0.0;
    ship.fire_timer = if beam {
        rules.weapons.beam_cooldown
    } else {
        rules.weapons.bullet_cooldown
    };

    let local = world.local_id == Some(shooter);
    let report = world.authority == Authority::Server && local;

    if !beam {
        let spawn = BulletSpawn::gun(&rules, origin, dir, &world.ships[index]);
        spawn_bullet(world, spawn);
        out.events.push(SimEvent::Fired {
            owner: shooter,
            weapon: WeaponKind::Bullet,
            origin,
            dir,
        });
        if report {
            out.net_out.push(NetIntent::Fire {
                weapon: WeaponKind::Bullet,
                origin,
                dir,
                target: None,
            });
        }
        return FireOutcome::Bullet;
    }

    let sweep = Sweep::beam(origin, dir, &world.ships[index], &rules);
    let cast = cast_beam(world, &sweep, hulls);
    let visual_start = cast.visual_start(dir, &rules);
    // `SimEvent::Fired` carries the beam's *endpoint* in `dir`, and its visual
    // start in `origin` — the shape `main.js:1509` puts on the wire.
    out.events.push(SimEvent::Fired {
        owner: shooter,
        weapon: WeaponKind::Beam,
        origin: visual_start,
        dir: cast.end,
    });
    if report {
        out.net_out.push(NetIntent::Fire {
            weapon: WeaponKind::Beam,
            origin: visual_start,
            dir: cast.end,
            target: None,
        });
    }
    if let Some(target) = cast.hit {
        apply_impact(
            world,
            Impact {
                target,
                t: 1.0,
                pos: cast.end,
            },
            rules.weapons.gun_damage,
            shooter,
            WeaponKind::Beam,
            out,
        );
    }
    FireOutcome::Beam
}

/// Runs down a ship's gun cooldown and runs up its ammo idle clock.
///
/// `main.js:1459`–`:1460`, which does exactly this and nothing else. Call it,
/// then [`fire_gun`], then [`regen_ammo`] — the JS order, and it matters: a shot
/// fired this tick must not also regenerate ammo this tick.
///
/// Separate from [`fire_gun`] because a ship whose trigger is not held still has
/// to cool down.
pub fn tick_weapon_cooldown(ship: &mut Ship, dt: f64) {
    ship.fire_timer -= dt;
    ship.ammo_idle += dt;
}

/// Regenerates ammo once the idle window has elapsed.
///
/// `main.js:1529`. No regeneration at all until
/// [`crate::rules::WeaponRules::ammo_regen_delay`] has passed since the last
/// shot, which is what makes a full magazine a resource rather than a formality.
pub fn regen_ammo(ship: &mut Ship, rules: &Rules, dt: f64) {
    let max = rules.weapons.max_ammo;
    if ship.ammo < max && ship.ammo_idle >= rules.weapons.ammo_regen_delay {
        ship.ammo = (ship.ammo + rules.weapons.ammo_regen * dt).min(max);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_asteroid_damage, apply_ship_damage, cast_beam, fire_gun, muzzle_origin, regen_ammo,
        resolve_impact, spawn_bullet, step, sweep_hulls, tick_weapon_cooldown, BulletOutput,
        BulletSpawn, FireOutcome, HullPart, HullVolumes, ShipBasis, Sweep, Target,
    };
    use crate::collision::{Aabb, Sphere};
    use crate::math::Vec3;
    use crate::rules::{Rules, BOSS_HITBOX_OFFSETS, BOSS_ID_BASE};
    use crate::world::{
        Asteroid, AsteroidTier, Authority, ExplosionKind, GunMode, MapKind, Mode, NetIntent, Quat,
        Score, Ship, ShipKind, SimEvent, Team, WeaponKind, World,
    };

    /// The step the tunneling analysis and the shadow bot simulation use.
    const FRAME_DT: f64 = 0.05;
    /// The fixed simulation step.
    const TICK_DT: f64 = 1.0 / 60.0;

    fn v(x: f64, y: f64, z: f64) -> Vec3 {
        Vec3::new(x, y, z)
    }

    fn world() -> World {
        // Terrain, so the moon is not sitting at the origin on top of every
        // test fixture. Tests that want the moon build a Space world.
        let mut w = World::new(0xB011E7, Rules::DEFAULT, Mode::Skirmish, MapKind::Terrain);
        w.boxes.clear();
        w
    }

    /// A ship at `pos`, on `team`, with spawn protection already expired.
    fn ship(w: &mut World, id: i32, team: Option<Team>, pos: Vec3) -> usize {
        let mut s = Ship::spawn(id, ShipKind::Bot, pos, Quat::IDENTITY, &w.rules);
        s.team = team;
        s.invuln_timer = 0.0;
        w.ships.push(s);
        w.ships.len() - 1
    }

    fn rock(w: &mut World, id: u32, pos: Vec3, radius: f64, hp: i32) {
        w.asteroids.push(Asteroid {
            id,
            pos,
            size: radius / w.rules.world.asteroid_field.collision_radius_scale,
            radius,
            hp,
            tier: AsteroidTier::Small,
            variant: 0,
            rot: Vec3::ZERO,
            spin: Vec3::ZERO,
            hit_flash: 0.0,
        });
    }

    fn shoot(w: &mut World, owner: i32, origin: Vec3, dir: Vec3) {
        let rules = w.rules;
        let spawn = BulletSpawn::gun(&rules, origin, dir, w.ship(owner).expect("shooter exists"));
        spawn_bullet(w, spawn);
    }

    fn hp_of(w: &World, id: i32) -> i32 {
        w.ship(id).expect("ship exists").hp
    }

    // ---------------------------------------------------------------------
    // The regression this module exists for.
    // ---------------------------------------------------------------------

    #[test]
    fn a_bullet_that_the_js_point_test_misses_now_lands() {
        // The exact geometry `collision.rs` pins: a target 100 units downrange,
        // a 780 u/s bullet, a 0.05 s step. The sampled positions are 0, 39, 78,
        // 117 — and the target spans 93.5 to 106.5, so no sample is ever inside
        // it. `bullets.js` scores nothing; this must score once.
        let mut w = world();
        ship(&mut w, 1, Some(Team::Zero), Vec3::ZERO);
        ship(&mut w, 2, Some(Team::One), v(0.0, 0.0, 100.0));
        let mut out = BulletOutput::new();
        shoot(&mut w, 1, Vec3::ZERO, Vec3::Z);

        let reach = w.rules.ship.hit_radius + w.rules.weapons.bullet_radius;
        let travel = w.rules.weapons.bullet_speed * FRAME_DT;
        assert!(
            travel > 2.0 * reach,
            "the step must overshoot the target for this test to mean anything"
        );

        for _ in 0..6 {
            step(&mut w, FRAME_DT, HullVolumes::EMPTY, &mut out);
        }

        assert!(w.bullets.is_empty(), "the bullet must have been consumed");
        assert_eq!(
            hp_of(&w, 2),
            w.rules.ship.max_hp - w.rules.weapons.gun_damage
        );
        assert!(out
            .events
            .iter()
            .any(|e| matches!(e, SimEvent::Damaged { id: 2, .. })));
    }

    #[test]
    fn one_bullet_damages_exactly_once() {
        let mut w = world();
        ship(&mut w, 1, Some(Team::Zero), Vec3::ZERO);
        ship(&mut w, 2, Some(Team::One), v(0.0, 0.0, 60.0));
        let mut out = BulletOutput::new();
        shoot(&mut w, 1, Vec3::ZERO, Vec3::Z);
        for _ in 0..40 {
            step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
        }
        let hits = out
            .events
            .iter()
            .filter(|e| matches!(e, SimEvent::Damaged { id: 2, .. }))
            .count();
        assert_eq!(hits, 1);
    }

    // ---------------------------------------------------------------------
    // Earliest hit wins.
    // ---------------------------------------------------------------------

    #[test]
    fn the_nearest_target_is_hit_even_when_a_later_category_is_scanned_first() {
        // A ship at 40 and a rock at 120, both on the line of fire. `bullets.js`
        // scans every asteroid before any ship and consumes the bolt on the
        // first contact it finds, so it damages the *rock* 80 units further
        // away. The ship is hit here.
        let mut w = world();
        ship(&mut w, 1, Some(Team::Zero), Vec3::ZERO);
        ship(&mut w, 2, Some(Team::One), v(0.0, 0.0, 40.0));
        rock(&mut w, 7, v(0.0, 0.0, 120.0), 10.0, 5);

        let sweep = Sweep {
            origin: Vec3::ZERO,
            motion: v(0.0, 0.0, 200.0),
            radius: w.rules.weapons.bullet_radius,
            step_dt: 0.0,
            owner: 1,
            owner_team: Some(Team::Zero),
            owner_coarse_aim: false,
        };
        let impact = resolve_impact(&w, &sweep, HullVolumes::EMPTY).expect("hit");
        assert!(matches!(impact.target, Target::Ship { id: 2, .. }));
        assert!(impact.pos.z < 40.0);
    }

    #[test]
    fn the_nearest_of_two_overlapping_rocks_is_the_one_damaged() {
        // `bullets.js:96` walks the asteroid list backwards, so with two
        // overlapping rocks it damages whichever sits later in the array. The
        // answer must not depend on array order.
        let mut w = world();
        ship(&mut w, 1, Some(Team::Zero), Vec3::ZERO);
        rock(&mut w, 10, v(0.0, 0.0, 30.0), 10.0, 5);
        rock(&mut w, 11, v(0.0, 0.0, 45.0), 10.0, 5);

        let sweep = Sweep {
            origin: Vec3::ZERO,
            motion: v(0.0, 0.0, 100.0),
            radius: 0.0,
            step_dt: 0.0,
            owner: 1,
            owner_team: Some(Team::Zero),
            owner_coarse_aim: false,
        };
        let near = resolve_impact(&w, &sweep, HullVolumes::EMPTY).expect("hit");
        assert!(matches!(near.target, Target::Asteroid { id: 10, .. }));

        // Reversed in the vector: same answer.
        w.asteroids.swap(0, 1);
        let still_near = resolve_impact(&w, &sweep, HullVolumes::EMPTY).expect("hit");
        assert!(matches!(still_near.target, Target::Asteroid { id: 10, .. }));
        assert_eq!(near.t.to_bits(), still_near.t.to_bits());
    }

    #[test]
    fn the_moon_stops_a_bullet_before_a_ship_behind_it() {
        let mut w = World::new(1, Rules::DEFAULT, Mode::Skirmish, MapKind::Space);
        ship(&mut w, 1, Some(Team::Zero), v(0.0, 0.0, -300.0));
        ship(&mut w, 2, Some(Team::One), v(0.0, 0.0, 300.0));
        let mut out = BulletOutput::new();
        shoot(&mut w, 1, v(0.0, 0.0, -300.0), Vec3::Z);
        for _ in 0..60 {
            step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
        }
        assert_eq!(hp_of(&w, 2), w.rules.ship.max_hp, "the moon is opaque");
        assert!(w.bullets.is_empty());
    }

    #[test]
    fn a_mothership_hull_stops_a_bullet() {
        // New behaviour: `main.js:1644` hands `bullets.update` only the sphere
        // obstacle list, so JS bullets fly through mothership hulls.
        let mut w = World::new(1, Rules::DEFAULT, Mode::Skirmish, MapKind::Space);
        w.obstacles.clear();
        ship(&mut w, 1, Some(Team::Zero), v(0.0, 0.0, -700.0));
        ship(&mut w, 2, Some(Team::One), v(0.0, 0.0, -500.0));
        let mut out = BulletOutput::new();
        shoot(&mut w, 1, v(0.0, 0.0, -700.0), Vec3::Z);
        for _ in 0..60 {
            step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
        }
        assert_eq!(hp_of(&w, 2), w.rules.ship.max_hp);
        assert!(w.bullets.is_empty());
    }

    // ---------------------------------------------------------------------
    // Friendly fire and team rules.
    // ---------------------------------------------------------------------

    #[test]
    fn a_shot_passes_through_a_friendly_without_being_consumed() {
        // `bullets.js:140` skips same-team ships with `continue`, so a friendly
        // in the line of fire is not cover. The enemy behind still gets hit.
        let mut w = world();
        ship(&mut w, 1, Some(Team::Zero), Vec3::ZERO);
        ship(&mut w, 2, Some(Team::Zero), v(0.0, 0.0, 40.0));
        ship(&mut w, 3, Some(Team::One), v(0.0, 0.0, 80.0));
        let mut out = BulletOutput::new();
        shoot(&mut w, 1, Vec3::ZERO, Vec3::Z);
        for _ in 0..30 {
            step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
        }
        assert_eq!(hp_of(&w, 2), w.rules.ship.max_hp, "no friendly fire");
        assert_eq!(
            hp_of(&w, 3),
            w.rules.ship.max_hp - w.rules.weapons.gun_damage
        );
    }

    #[test]
    fn a_shot_never_hits_its_own_shooter() {
        let mut w = world();
        ship(&mut w, 1, None, Vec3::ZERO);
        let mut out = BulletOutput::new();
        // Fired from inside its own hull, pointing backwards through it.
        shoot(&mut w, 1, Vec3::ZERO, -Vec3::Z);
        step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
        assert_eq!(hp_of(&w, 1), w.rules.ship.max_hp);
    }

    #[test]
    fn unassigned_teams_can_shoot_each_other() {
        // `server/index.js:941` rejects friendly fire only when *both* sides
        // have a team; a `null` team is not a team.
        let mut w = world();
        ship(&mut w, 1, None, Vec3::ZERO);
        ship(&mut w, 2, None, v(0.0, 0.0, 40.0));
        let mut out = BulletOutput::new();
        shoot(&mut w, 1, Vec3::ZERO, Vec3::Z);
        for _ in 0..30 {
            step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
        }
        assert_eq!(
            hp_of(&w, 2),
            w.rules.ship.max_hp - w.rules.weapons.gun_damage
        );
    }

    #[test]
    fn a_spawn_protected_ship_stops_the_bullet_but_takes_no_damage() {
        // `bullets.js` gates only on `alive`, so the bolt is consumed; the
        // server then rejects the hit (`server/index.js:942`). Both halves.
        let mut w = world();
        ship(&mut w, 1, Some(Team::Zero), Vec3::ZERO);
        let idx = ship(&mut w, 2, Some(Team::One), v(0.0, 0.0, 40.0));
        w.ships[idx].invuln_timer = w.rules.combat.spawn_invuln;
        let mut out = BulletOutput::new();
        shoot(&mut w, 1, Vec3::ZERO, Vec3::Z);
        for _ in 0..30 {
            step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
        }
        assert_eq!(hp_of(&w, 2), w.rules.ship.max_hp);
        assert!(w.bullets.is_empty(), "the bolt was still consumed");
    }

    #[test]
    fn a_dead_ship_is_not_a_target() {
        let mut w = world();
        ship(&mut w, 1, Some(Team::Zero), Vec3::ZERO);
        let a = ship(&mut w, 2, Some(Team::One), v(0.0, 0.0, 40.0));
        w.ships[a].alive = false;
        ship(&mut w, 3, Some(Team::One), v(0.0, 0.0, 80.0));
        let mut out = BulletOutput::new();
        shoot(&mut w, 1, Vec3::ZERO, Vec3::Z);
        for _ in 0..30 {
            step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
        }
        assert_eq!(
            hp_of(&w, 3),
            w.rules.ship.max_hp - w.rules.weapons.gun_damage
        );
    }

    #[test]
    fn coarse_aim_widens_the_target_for_the_shooter_that_has_it() {
        // 6.0 precise, 7.0 coarse, plus the bullet's own 0.5. A shot offset by
        // 7.0 laterally misses for a mouse pilot and lands for a keyboard one.
        let mut w = world();
        ship(&mut w, 1, Some(Team::Zero), Vec3::ZERO);
        ship(&mut w, 2, Some(Team::One), v(0.0, 0.0, 40.0));
        let base = Sweep {
            origin: v(7.0, 0.0, 0.0),
            motion: v(0.0, 0.0, 80.0),
            radius: w.rules.weapons.bullet_radius,
            step_dt: 0.0,
            owner: 1,
            owner_team: Some(Team::Zero),
            owner_coarse_aim: false,
        };
        assert!(resolve_impact(&w, &base, HullVolumes::EMPTY).is_none());
        let coarse = Sweep {
            owner_coarse_aim: true,
            ..base
        };
        assert!(resolve_impact(&w, &coarse, HullVolumes::EMPTY).is_some());
    }

    // ---------------------------------------------------------------------
    // Lifetime and integration.
    // ---------------------------------------------------------------------

    #[test]
    fn a_bullet_expires_after_its_lifetime() {
        let mut w = world();
        ship(&mut w, 1, None, Vec3::ZERO);
        let mut out = BulletOutput::new();
        shoot(&mut w, 1, Vec3::ZERO, Vec3::Z);
        let steps = (w.rules.weapons.bullet_life / TICK_DT).ceil() as u32;
        for _ in 0..steps - 1 {
            step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
            assert_eq!(w.bullets.len(), 1);
        }
        // Two more, not one: `life` after `steps` subtractions of `1/60` from
        // `2.0` lands within an ulp of zero and may still be a hair positive.
        step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
        step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
        assert!(w.bullets.is_empty());
    }

    #[test]
    fn a_bullet_in_its_final_frame_still_hits() {
        // `bullets.js:93` decrements life and returns before testing, so a bolt
        // whose last frame carries it into a target passes through. Travel is
        // clamped to the remaining life here instead.
        let mut w = world();
        ship(&mut w, 1, Some(Team::Zero), Vec3::ZERO);
        ship(&mut w, 2, Some(Team::One), v(0.0, 0.0, 4.0));
        let mut out = BulletOutput::new();
        let rules = w.rules;
        let mut spawn = BulletSpawn::gun(&rules, Vec3::ZERO, Vec3::Z, w.ship(1).expect("shooter"));
        // Enough life to cross the 4 units to the target, less than one step.
        spawn.life = 0.01;
        spawn_bullet(&mut w, spawn);
        step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
        assert!(w.bullets.is_empty());
        assert_eq!(
            hp_of(&w, 2),
            w.rules.ship.max_hp - w.rules.weapons.gun_damage
        );
    }

    #[test]
    fn prev_pos_tracks_the_previous_step() {
        let mut w = world();
        ship(&mut w, 1, None, Vec3::ZERO);
        let mut out = BulletOutput::new();
        shoot(&mut w, 1, Vec3::ZERO, Vec3::Z);
        assert_eq!(w.bullets[0].prev_pos, w.bullets[0].pos);
        step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
        assert_eq!(w.bullets[0].prev_pos, Vec3::ZERO);
        let after_one = w.bullets[0].pos;
        step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
        assert_eq!(w.bullets[0].prev_pos, after_one);
    }

    #[test]
    fn a_crossing_target_is_hit_where_it_will_be_not_where_it_was() {
        // A ship sprinting across the line of fire. Treating it as parked at its
        // start-of-step position misses; the moving-sphere sweep lands it.
        let mut w = world();
        ship(&mut w, 1, Some(Team::Zero), Vec3::ZERO);
        let idx = ship(&mut w, 2, Some(Team::One), v(-30.0, 0.0, 20.0));
        // Fast enough to reach x = 0 as the bolt passes z = 20.
        w.ships[idx].vel = v(1170.0, 0.0, 0.0);
        let sweep = Sweep {
            origin: Vec3::ZERO,
            motion: v(0.0, 0.0, 39.0),
            radius: w.rules.weapons.bullet_radius,
            step_dt: FRAME_DT,
            owner: 1,
            owner_team: Some(Team::Zero),
            owner_coarse_aim: false,
        };
        assert!(resolve_impact(&w, &sweep, HullVolumes::EMPTY).is_some());
        // With the target frozen it is 30 units off the line and cannot be hit.
        let frozen = Sweep {
            step_dt: 0.0,
            ..sweep
        };
        assert!(resolve_impact(&w, &frozen, HullVolumes::EMPTY).is_none());
    }

    #[test]
    fn bullets_survive_in_order_when_one_of_several_is_consumed() {
        let mut w = world();
        ship(&mut w, 1, Some(Team::Zero), Vec3::ZERO);
        ship(&mut w, 2, Some(Team::One), v(0.0, 0.0, 40.0));
        let mut out = BulletOutput::new();
        // Three shots: down the y axis, at the ship, down the x axis.
        shoot(&mut w, 1, Vec3::ZERO, Vec3::Y);
        shoot(&mut w, 1, Vec3::ZERO, Vec3::Z);
        shoot(&mut w, 1, Vec3::ZERO, Vec3::X);
        let keys: Vec<u64> = w.bullets.iter().map(|b| b.key).collect();
        for _ in 0..10 {
            step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
        }
        let left: Vec<u64> = w.bullets.iter().map(|b| b.key).collect();
        assert_eq!(left, vec![keys[0], keys[2]]);
    }

    // ---------------------------------------------------------------------
    // Damage, death, scoring.
    // ---------------------------------------------------------------------

    #[test]
    fn enough_hits_kill_and_the_kill_is_scored() {
        let mut w = world();
        w.local_id = Some(1);
        w.match_state.scores.push(Score {
            id: 1,
            team: Some(Team::Zero),
            kills: 0,
            deaths: 0,
        });
        w.match_state.scores.push(Score {
            id: 2,
            team: Some(Team::One),
            kills: 0,
            deaths: 0,
        });
        ship(&mut w, 1, Some(Team::Zero), Vec3::ZERO);
        let idx = ship(&mut w, 2, Some(Team::One), v(0.0, 0.0, 40.0));
        w.ships[idx].kind = ShipKind::Bot;

        let mut out = BulletOutput::new();
        let shots = w.rules.ship.max_hp / w.rules.weapons.gun_damage;
        for _ in 0..shots {
            shoot(&mut w, 1, Vec3::ZERO, Vec3::Z);
            for _ in 0..10 {
                step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
            }
        }

        let victim = w.ship(2).expect("victim");
        assert_eq!(victim.hp, 0);
        assert!(!victim.alive);
        assert_eq!(victim.respawn_timer, w.rules.combat.respawn_delay);
        assert_eq!(w.match_state.team_kills[Team::Zero.index()], 1);
        assert_eq!(w.match_state.scores[0].kills, 1);
        assert_eq!(w.match_state.scores[1].deaths, 1);
        assert_eq!(w.match_state.solo_bots_killed, 1);
        assert_eq!(
            out.events
                .iter()
                .filter(|e| matches!(e, SimEvent::ShipDestroyed { id: 2, .. }))
                .count(),
            1
        );
    }

    #[test]
    fn a_dead_ship_takes_no_further_damage() {
        let mut w = world();
        let idx = ship(&mut w, 2, Some(Team::One), Vec3::ZERO);
        w.ships[idx].alive = false;
        w.ships[idx].hp = 0;
        let mut out = BulletOutput::new();
        assert!(!apply_ship_damage(&mut w, 2, 10, None, &mut out));
        assert!(out.is_empty());
    }

    #[test]
    fn every_weapon_chips_a_rock_by_exactly_one_point() {
        let mut w = world();
        rock(&mut w, 5, Vec3::ZERO, 6.0, 3);
        let mut out = BulletOutput::new();
        assert!(apply_asteroid_damage(&mut w, 5, &mut out));
        assert_eq!(w.asteroid(5).expect("rock").hp, 2);
        assert!(apply_asteroid_damage(&mut w, 5, &mut out));
        assert!(apply_asteroid_damage(&mut w, 5, &mut out));
        assert_eq!(w.asteroid(5).expect("rock").hp, 0);
        assert!(
            !apply_asteroid_damage(&mut w, 5, &mut out),
            "a destroyed rock is not hit again"
        );
        assert_eq!(
            out.events
                .iter()
                .filter(|e| matches!(e, SimEvent::AsteroidDestroyed { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn a_destroyed_rock_stops_blocking_shots() {
        let mut w = world();
        ship(&mut w, 1, Some(Team::Zero), Vec3::ZERO);
        ship(&mut w, 2, Some(Team::One), v(0.0, 0.0, 80.0));
        rock(&mut w, 5, v(0.0, 0.0, 40.0), 10.0, 1);
        let mut out = BulletOutput::new();

        shoot(&mut w, 1, Vec3::ZERO, Vec3::Z);
        for _ in 0..10 {
            step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
        }
        assert_eq!(w.asteroid(5).expect("rock").hp, 0);
        assert_eq!(hp_of(&w, 2), w.rules.ship.max_hp);

        shoot(&mut w, 1, Vec3::ZERO, Vec3::Z);
        for _ in 0..10 {
            step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
        }
        assert_eq!(
            hp_of(&w, 2),
            w.rules.ship.max_hp - w.rules.weapons.gun_damage,
            "the second shot passes through the wreck"
        );
    }

    // ---------------------------------------------------------------------
    // The boss.
    // ---------------------------------------------------------------------

    #[test]
    fn a_boss_hitbox_ship_reports_a_hull_hit_and_takes_no_ship_damage() {
        let mut w = world();
        ship(&mut w, 1, Some(Team::Zero), Vec3::ZERO);
        let idx = ship(&mut w, BOSS_ID_BASE, Some(Team::One), v(0.0, 0.0, 100.0));
        w.ships[idx].hit_radius_override = Some(w.rules.weapons.boss_hitbox_radius);

        let mut out = BulletOutput::new();
        shoot(&mut w, 1, Vec3::ZERO, Vec3::Z);
        for _ in 0..20 {
            step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
        }
        assert_eq!(out.hull_hits.len(), 1);
        assert_eq!(out.hull_hits[0].part, HullPart::Hitbox(BOSS_ID_BASE));
        assert_eq!(out.hull_hits[0].damage, w.rules.weapons.gun_damage);
        assert_eq!(out.hull_hits[0].weapon, WeaponKind::Bullet);
        assert_eq!(
            hp_of(&w, BOSS_ID_BASE),
            w.rules.ship.max_hp,
            "hitbox hit points are not the boss's hit points"
        );
        assert!(!out
            .events
            .iter()
            .any(|e| matches!(e, SimEvent::Damaged { .. })));
    }

    #[test]
    fn a_caller_supplied_hull_takes_hits_as_spheres_or_boxes() {
        let mut w = world();
        ship(&mut w, 1, Some(Team::Zero), Vec3::ZERO);
        let spheres = [Sphere::new(v(0.0, 0.0, 200.0), 28.0)];
        let boxes = [Aabb::new(v(0.0, 0.0, 400.0), v(60.0, 20.0, 40.0))];
        let hulls = HullVolumes {
            spheres: &spheres,
            boxes: &boxes,
        };

        let mut out = BulletOutput::new();
        shoot(&mut w, 1, Vec3::ZERO, Vec3::Z);
        for _ in 0..20 {
            step(&mut w, TICK_DT, hulls, &mut out);
        }
        assert_eq!(out.hull_hits.len(), 1);
        assert_eq!(out.hull_hits[0].part, HullPart::Sphere(0));

        // Past the sphere, the box still stops a shot.
        out.clear();
        let empty: [Sphere; 0] = [];
        let box_only = HullVolumes {
            spheres: &empty,
            boxes: &boxes,
        };
        shoot(&mut w, 1, Vec3::ZERO, Vec3::Z);
        for _ in 0..40 {
            step(&mut w, TICK_DT, box_only, &mut out);
        }
        assert_eq!(out.hull_hits.len(), 1);
        assert_eq!(out.hull_hits[0].part, HullPart::Box(0));
    }

    #[test]
    fn the_js_boss_sphere_layout_leaves_gaps_a_shot_threads() {
        // Not a requirement — a record. The 20 spheres of `BOSS_HB_OFFSETS_WORLD`
        // sit 75 units apart in z at radius 28, so a shot down the z axis at
        // x = +-56.5 passes between the x = +-28 and x = +-85 columns and misses
        // a capital ship it is visually inside. Whoever owns the hull should see
        // this fail once the layout is fixed.
        let center = v(0.0, 0.0, 600.0);
        let spheres: Vec<Sphere> = BOSS_HITBOX_OFFSETS
            .iter()
            .map(|o| Sphere::new(center + *o, 28.0))
            .collect();
        let hulls = HullVolumes {
            spheres: &spheres,
            boxes: &[],
        };
        // Straight down the spine: hits.
        assert!(sweep_hulls(v(0.0, 0.0, 0.0), v(0.0, 0.0, 1000.0), 0.5, hulls).is_some());
        // Across the beam at z + 37.5, midway between the z = 0 and z = 75
        // rows: 800 units of travel through a 340-unit hull, no contact.
        let between = center + v(-400.0, 0.0, 37.5);
        assert!(
            sweep_hulls(between, v(800.0, 0.0, 0.0), 0.5, hulls).is_none(),
            "the gap has closed — the boss hull layout changed, update this test"
        );
    }

    // ---------------------------------------------------------------------
    // The beam.
    // ---------------------------------------------------------------------

    #[test]
    fn the_beam_reaches_its_full_range_when_it_meets_nothing() {
        let mut w = world();
        ship(&mut w, 1, Some(Team::Zero), Vec3::ZERO);
        let sweep = Sweep::beam(Vec3::ZERO, Vec3::Z, w.ship(1).expect("shooter"), &w.rules);
        let cast = cast_beam(&w, &sweep, HullVolumes::EMPTY);
        assert!(cast.hit.is_none());
        assert_eq!(cast.dist, w.rules.weapons.beam_range);
        assert_eq!(cast.end, v(0.0, 0.0, w.rules.weapons.beam_range));
    }

    #[test]
    fn the_beam_stops_at_the_first_thing_in_its_path() {
        let mut w = world();
        ship(&mut w, 1, Some(Team::Zero), Vec3::ZERO);
        ship(&mut w, 2, Some(Team::One), v(0.0, 0.0, 300.0));
        rock(&mut w, 4, v(0.0, 0.0, 150.0), 12.0, 5);
        let sweep = Sweep::beam(Vec3::ZERO, Vec3::Z, w.ship(1).expect("shooter"), &w.rules);
        let cast = cast_beam(&w, &sweep, HullVolumes::EMPTY);
        assert!(matches!(cast.hit, Some(Target::Asteroid { id: 4, .. })));
        assert!((cast.dist - 138.0).abs() < 1e-9, "dist = {}", cast.dist);
    }

    #[test]
    fn the_beam_uses_the_same_ship_hit_radius_as_everything_else() {
        // `main.js:1040` gives the beam its own `BEAM_SHIP_RADIUS = 5.5`. A
        // target offset by 5.75 is inside the unified 6.0 and outside the old
        // 5.5, so this pins which one is in force.
        let mut w = world();
        ship(&mut w, 1, Some(Team::Zero), Vec3::ZERO);
        ship(&mut w, 2, Some(Team::One), v(5.75, 0.0, 200.0));
        let sweep = Sweep::beam(Vec3::ZERO, Vec3::Z, w.ship(1).expect("shooter"), &w.rules);
        assert!(matches!(
            cast_beam(&w, &sweep, HullVolumes::EMPTY).hit,
            Some(Target::Ship { id: 2, .. })
        ));
    }

    #[test]
    fn the_beam_visual_starts_ahead_of_the_muzzle_unless_the_shot_is_point_blank() {
        let mut w = world();
        ship(&mut w, 1, Some(Team::Zero), Vec3::ZERO);
        let sweep = Sweep::beam(Vec3::ZERO, Vec3::Z, w.ship(1).expect("shooter"), &w.rules);
        let long = cast_beam(&w, &sweep, HullVolumes::EMPTY);
        assert_eq!(
            long.visual_start(Vec3::Z, &w.rules),
            v(0.0, 0.0, w.rules.weapons.beam_forward_offset)
        );

        rock(&mut w, 9, v(0.0, 0.0, 2.0), 1.0, 5);
        let short = cast_beam(&w, &sweep, HullVolumes::EMPTY);
        assert!(short.dist < w.rules.weapons.beam_forward_offset);
        assert_eq!(short.visual_start(Vec3::Z, &w.rules), Vec3::ZERO);
    }

    #[test]
    fn firing_the_beam_damages_what_it_hits() {
        let mut w = world();
        w.local_id = Some(1);
        ship(&mut w, 1, Some(Team::Zero), Vec3::ZERO);
        ship(&mut w, 2, Some(Team::One), v(0.0, 0.0, 200.0));
        w.ship_mut(1).expect("shooter").gun_mode = GunMode::Beam;

        let mut out = BulletOutput::new();
        let outcome = fire_gun(&mut w, 1, ShipBasis::IDENTITY, HullVolumes::EMPTY, &mut out);
        assert_eq!(outcome, FireOutcome::Beam);
        assert_eq!(
            hp_of(&w, 2),
            w.rules.ship.max_hp - w.rules.weapons.gun_damage
        );
        assert!(w.bullets.is_empty(), "a beam spawns no projectile");
        assert!(out.events.iter().any(|e| matches!(
            e,
            SimEvent::Fired {
                weapon: WeaponKind::Beam,
                ..
            }
        )));
    }

    // ---------------------------------------------------------------------
    // Fire rate, ammo, overheat.
    // ---------------------------------------------------------------------

    #[test]
    fn the_gun_holds_its_cadence() {
        let mut w = world();
        ship(&mut w, 1, None, Vec3::ZERO);
        let mut out = BulletOutput::new();
        let rules = w.rules;

        assert_eq!(
            fire_gun(&mut w, 1, ShipBasis::IDENTITY, HullVolumes::EMPTY, &mut out),
            FireOutcome::Bullet
        );
        assert_eq!(
            fire_gun(&mut w, 1, ShipBasis::IDENTITY, HullVolumes::EMPTY, &mut out),
            FireOutcome::Cooling
        );
        // Cool down exactly one cooldown's worth.
        let ticks = (rules.weapons.bullet_cooldown / TICK_DT).ceil() as u32 + 1;
        for _ in 0..ticks {
            tick_weapon_cooldown(w.ship_mut(1).expect("ship"), TICK_DT);
        }
        assert_eq!(
            fire_gun(&mut w, 1, ShipBasis::IDENTITY, HullVolumes::EMPTY, &mut out),
            FireOutcome::Bullet
        );
        assert_eq!(w.bullets.len(), 2);
    }

    #[test]
    fn the_beam_costs_three_ammo_and_reloads_slower_than_the_gun() {
        let mut w = world();
        ship(&mut w, 1, None, Vec3::ZERO);
        w.ship_mut(1).expect("ship").gun_mode = GunMode::Beam;
        let mut out = BulletOutput::new();
        let full = w.rules.weapons.max_ammo;

        assert_eq!(
            fire_gun(&mut w, 1, ShipBasis::IDENTITY, HullVolumes::EMPTY, &mut out),
            FireOutcome::Beam
        );
        let s = w.ship(1).expect("ship");
        assert_eq!(s.ammo, full - w.rules.weapons.beam_ammo_cost);
        assert_eq!(s.fire_timer, w.rules.weapons.beam_cooldown);
        assert!(w.rules.weapons.beam_cooldown > w.rules.weapons.bullet_cooldown);
    }

    #[test]
    fn an_empty_pool_overheats_and_recovers_only_after_the_idle_window() {
        let mut w = world();
        ship(&mut w, 1, None, Vec3::ZERO);
        w.ship_mut(1).expect("ship").gun_mode = GunMode::Beam;
        let mut out = BulletOutput::new();
        let rules = w.rules;

        // Drain the pool: fire, then cool down, repeatedly.
        let cool_ticks = (rules.weapons.beam_cooldown / TICK_DT).ceil() as u32 + 1;
        let mut shots = 0;
        loop {
            match fire_gun(&mut w, 1, ShipBasis::IDENTITY, HullVolumes::EMPTY, &mut out) {
                FireOutcome::Beam => shots += 1,
                FireOutcome::Overheated => break,
                other => panic!("unexpected outcome {other:?}"),
            }
            for _ in 0..cool_ticks {
                tick_weapon_cooldown(w.ship_mut(1).expect("ship"), TICK_DT);
                // Firing every cooldown keeps `ammo_idle` reset, so nothing
                // regenerates: the pool really does run dry.
                regen_ammo(w.ship_mut(1).expect("ship"), &rules, TICK_DT);
            }
            assert!(shots < 100, "the pool never emptied");
        }
        assert_eq!(shots, 30, "90 ammo at 3 per shot");
        assert!(w.ship(1).expect("ship").ammo < rules.weapons.beam_ammo_cost);

        // Nothing regenerates until the idle window has passed.
        w.ship_mut(1).expect("ship").ammo_idle = 0.0;
        let before = w.ship(1).expect("ship").ammo;
        regen_ammo(w.ship_mut(1).expect("ship"), &rules, TICK_DT);
        assert_eq!(w.ship(1).expect("ship").ammo, before);

        w.ship_mut(1).expect("ship").ammo_idle = rules.weapons.ammo_regen_delay;
        regen_ammo(w.ship_mut(1).expect("ship"), &rules, TICK_DT);
        assert!(w.ship(1).expect("ship").ammo > before);
    }

    #[test]
    fn ammo_regeneration_stops_at_the_cap() {
        let mut w = world();
        ship(&mut w, 1, None, Vec3::ZERO);
        let rules = w.rules;
        let s = w.ship_mut(1).expect("ship");
        s.ammo = rules.weapons.max_ammo - 0.1;
        s.ammo_idle = rules.weapons.ammo_regen_delay;
        regen_ammo(w.ship_mut(1).expect("ship"), &rules, 1.0);
        assert_eq!(w.ship(1).expect("ship").ammo, rules.weapons.max_ammo);
    }

    #[test]
    fn a_dead_ship_cannot_fire() {
        let mut w = world();
        let idx = ship(&mut w, 1, None, Vec3::ZERO);
        w.ships[idx].alive = false;
        let mut out = BulletOutput::new();
        assert_eq!(
            fire_gun(&mut w, 1, ShipBasis::IDENTITY, HullVolumes::EMPTY, &mut out),
            FireOutcome::Unavailable
        );
        assert_eq!(
            fire_gun(
                &mut w,
                99,
                ShipBasis::IDENTITY,
                HullVolumes::EMPTY,
                &mut out
            ),
            FireOutcome::Unavailable
        );
    }

    #[test]
    fn firing_suppresses_health_regeneration_and_ammo_regeneration() {
        let mut w = world();
        ship(&mut w, 1, None, Vec3::ZERO);
        {
            let s = w.ship_mut(1).expect("ship");
            s.health_idle_shot = 99.0;
            s.ammo_idle = 99.0;
        }
        let mut out = BulletOutput::new();
        assert_eq!(
            fire_gun(&mut w, 1, ShipBasis::IDENTITY, HullVolumes::EMPTY, &mut out),
            FireOutcome::Bullet
        );
        let s = w.ship(1).expect("ship");
        assert_eq!(s.health_idle_shot, 0.0);
        assert_eq!(s.ammo_idle, 0.0);
    }

    // ---------------------------------------------------------------------
    // Muzzle, spawn, network reporting.
    // ---------------------------------------------------------------------

    #[test]
    fn the_muzzle_sits_ahead_of_the_ship_along_its_own_forward() {
        let rules = Rules::DEFAULT;
        let off = rules.weapons.muzzle_offset.z;
        assert_eq!(
            muzzle_origin(v(1.0, 2.0, 3.0), ShipBasis::IDENTITY, &rules),
            v(1.0, 2.0, 3.0 + off)
        );
        // Turned around: the muzzle follows.
        let flipped = ShipBasis {
            right: -Vec3::X,
            up: Vec3::Y,
            forward: -Vec3::Z,
        };
        assert_eq!(
            muzzle_origin(Vec3::ZERO, flipped, &rules),
            v(0.0, 0.0, -off)
        );
        assert_eq!(
            muzzle_origin(Vec3::ZERO, ShipBasis::along(Vec3::Y), &rules),
            v(0.0, off, 0.0)
        );
    }

    #[test]
    fn a_bullet_does_not_inherit_the_shooters_velocity() {
        // `bullets.js:44` is `direction * SPEED` and nothing else.
        let mut w = world();
        let idx = ship(&mut w, 1, None, Vec3::ZERO);
        w.ships[idx].vel = v(0.0, 0.0, 500.0);
        let mut out = BulletOutput::new();
        assert_eq!(
            fire_gun(&mut w, 1, ShipBasis::IDENTITY, HullVolumes::EMPTY, &mut out),
            FireOutcome::Bullet
        );
        assert_eq!(w.bullets[0].vel, Vec3::Z * w.rules.weapons.bullet_speed);
    }

    #[test]
    fn a_spawned_bullet_normalises_its_direction() {
        let mut w = world();
        ship(&mut w, 1, None, Vec3::ZERO);
        let rules = w.rules;
        let spawn = BulletSpawn::gun(
            &rules,
            Vec3::ZERO,
            v(0.0, 0.0, 17.0),
            w.ship(1).expect("shooter"),
        );
        spawn_bullet(&mut w, spawn);
        assert_eq!(w.bullets[0].vel.length(), rules.weapons.bullet_speed);
    }

    #[test]
    fn boss_turret_rounds_share_the_bullet_list_with_their_own_numbers() {
        let mut w = world();
        let rules = w.rules;
        let spawn =
            BulletSpawn::boss_turret(&rules, Vec3::ZERO, Vec3::Z, BOSS_ID_BASE, Some(Team::One));
        spawn_bullet(&mut w, spawn);
        let b = w.bullets[0];
        assert_eq!(b.vel.length(), rules.weapons.boss_bullet_speed);
        assert_eq!(b.life, rules.weapons.boss_bullet_life);
        assert_eq!(b.damage, rules.weapons.boss_bullet_damage);
    }

    #[test]
    fn hits_are_reported_to_the_server_only_when_the_server_owns_hit_points() {
        let mut w = World::new(1, Rules::DEFAULT, Mode::Multiplayer, MapKind::Space);
        w.obstacles.clear();
        w.boxes.clear();
        w.local_id = Some(1);
        assert_eq!(w.authority, Authority::Server);
        let idx = ship(&mut w, 1, Some(Team::Zero), Vec3::ZERO);
        w.ships[idx].kind = ShipKind::Local;
        ship(&mut w, 2, Some(Team::One), v(0.0, 0.0, 40.0));
        rock(&mut w, 3, v(0.0, 40.0, 0.0), 10.0, 5);

        let mut out = BulletOutput::new();
        shoot(&mut w, 1, Vec3::ZERO, Vec3::Z);
        shoot(&mut w, 1, Vec3::ZERO, Vec3::Y);
        for _ in 0..20 {
            step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
        }
        assert!(out.net_out.contains(&NetIntent::Hit {
            target: 2,
            weapon: WeaponKind::Bullet,
            from_bot: None,
        }));
        assert!(out.net_out.contains(&NetIntent::AsteroidHit { id: 3 }));

        // Solo: the same shots report nothing.
        let mut solo = world();
        solo.local_id = Some(1);
        let idx = ship(&mut solo, 1, Some(Team::Zero), Vec3::ZERO);
        solo.ships[idx].kind = ShipKind::Local;
        ship(&mut solo, 2, Some(Team::One), v(0.0, 0.0, 40.0));
        let mut solo_out = BulletOutput::new();
        shoot(&mut solo, 1, Vec3::ZERO, Vec3::Z);
        for _ in 0..20 {
            step(&mut solo, TICK_DT, HullVolumes::EMPTY, &mut solo_out);
        }
        assert!(solo_out.net_out.is_empty());
        assert!(
            hp_of(&solo, 2) < solo.rules.ship.max_hp,
            "damage still applies locally in every mode"
        );
    }

    #[test]
    fn a_bots_hit_is_reported_with_its_id() {
        let mut w = World::new(1, Rules::DEFAULT, Mode::Multiplayer, MapKind::Space);
        w.obstacles.clear();
        w.boxes.clear();
        ship(&mut w, 5, Some(Team::Zero), Vec3::ZERO); // ShipKind::Bot by default
        ship(&mut w, 2, Some(Team::One), v(0.0, 0.0, 40.0));
        let mut out = BulletOutput::new();
        shoot(&mut w, 5, Vec3::ZERO, Vec3::Z);
        for _ in 0..20 {
            step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
        }
        assert!(out.net_out.contains(&NetIntent::Hit {
            target: 2,
            weapon: WeaponKind::Bullet,
            from_bot: Some(5),
        }));
    }

    #[test]
    fn a_remote_players_bullet_damages_but_is_never_claimed() {
        let mut w = World::new(1, Rules::DEFAULT, Mode::Multiplayer, MapKind::Space);
        w.obstacles.clear();
        w.boxes.clear();
        let idx = ship(&mut w, 7, Some(Team::Zero), Vec3::ZERO);
        w.ships[idx].kind = ShipKind::Remote;
        ship(&mut w, 2, Some(Team::One), v(0.0, 0.0, 40.0));
        let mut out = BulletOutput::new();
        shoot(&mut w, 7, Vec3::ZERO, Vec3::Z);
        for _ in 0..20 {
            step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
        }
        assert!(out.net_out.is_empty());
    }

    // ---------------------------------------------------------------------
    // Events and determinism.
    // ---------------------------------------------------------------------

    #[test]
    fn every_impact_reports_an_explosion() {
        let mut w = World::new(1, Rules::DEFAULT, Mode::Skirmish, MapKind::Space);
        ship(&mut w, 1, Some(Team::Zero), v(0.0, 0.0, -300.0));
        let mut out = BulletOutput::new();
        shoot(&mut w, 1, v(0.0, 0.0, -300.0), Vec3::Z);
        for _ in 0..60 {
            step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
        }
        let explosions: Vec<&SimEvent> = out
            .events
            .iter()
            .filter(|e| matches!(e, SimEvent::Explosion { .. }))
            .collect();
        assert_eq!(explosions.len(), 1);
        assert!(matches!(
            explosions[0],
            SimEvent::Explosion {
                kind: ExplosionKind::Impact,
                ..
            }
        ));
    }

    #[test]
    fn a_step_is_bit_identical_when_repeated() {
        // Same world, same inputs, same bits — on this machine and on any other.
        let build = || {
            let mut w = world();
            ship(&mut w, 1, Some(Team::Zero), Vec3::ZERO);
            ship(&mut w, 2, Some(Team::One), v(3.5, -1.25, 77.0));
            rock(&mut w, 4, v(-2.0, 0.5, 41.0), 9.0, 5);
            for i in 0..8 {
                let d = v(0.03 * f64::from(i), -0.017 * f64::from(i), 1.0).normalize();
                shoot(&mut w, 1, v(0.0, 0.0, 0.6), d);
            }
            w
        };

        let run = || {
            let mut w = build();
            let mut out = BulletOutput::new();
            for _ in 0..12 {
                step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
            }
            let bits: Vec<u64> = w
                .bullets
                .iter()
                .flat_map(|b| {
                    [
                        b.pos.x.to_bits(),
                        b.pos.y.to_bits(),
                        b.pos.z.to_bits(),
                        b.life.to_bits(),
                    ]
                })
                .collect();
            (bits, w.ships[1].hp, w.asteroids[0].hp, out.events.len())
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn an_empty_world_steps_without_incident() {
        let mut w = world();
        let mut out = BulletOutput::new();
        step(&mut w, TICK_DT, HullVolumes::EMPTY, &mut out);
        assert!(w.bullets.is_empty());
        assert!(out.is_empty());
    }

    #[test]
    fn output_buffers_are_reusable() {
        let mut out = BulletOutput::new();
        out.events.push(SimEvent::BossPhaseStarted);
        assert!(!out.is_empty());
        out.clear();
        assert!(out.is_empty());
    }
}
