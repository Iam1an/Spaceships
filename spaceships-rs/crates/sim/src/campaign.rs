//! The three-mission campaign: waves, checkpoints, lives, and the capital ship.
//!
//! # What this module owns
//!
//! The mission state machine ([`update`]), wave spawning ([`spawn_wave`]), the
//! three-lives system with its 55 %-health respawn ([`on_player_death`],
//! [`respawn_pose`]), and the capital ship boss — its hull, its patrol, its four
//! turrets, and its hit points.
//!
//! What it deliberately does **not** own: ballistics, bot AI, ship physics, and
//! asteroid generation. The boss's turret rounds are pushed into
//! [`World::bullets`] as ordinary [`Bullet`]s carrying
//! [`WeaponRules::boss_bullet_damage`], so the bullets module moves them and
//! resolves them against the player with exactly the rules every other bullet
//! obeys. There is no second projectile list; `main.js:2327` (`bossBullets`) and
//! its private point-test in `updateBoss` (`main.js:2767`) are gone.
//!
//! # The hull: why 20 spheres became 4 boxes
//!
//! `main.js:2298` (`BOSS_HB_OFFSETS_WORLD`) models a 200 × 30 × 360 capital ship
//! as 20 spheres of radius 28 on a grid — x at −85/−28/28/85, z at
//! −150/−75/0/75/150 — **every one of them at y = 0** except a single sphere at
//! (0, 30, 50). That layout has three holes, all of them reachable:
//!
//! | Hole | Why | Example |
//! |---|---|---|
//! | x corridors | Columns are 57 apart, spheres are 56 across | a ray at x = 56.5 threads between the 28 and 85 columns |
//! | z corridors | Rows are 75 apart, spheres are 56 across | a ray at z = 37.5 passes between the 0 and 75 rows |
//! | everything off the deck plane | 19 of 20 spheres sit at y = 0 | a ray at y = 40 over the bridge meets nothing |
//!
//! None of this was visible while the beam had its own test: `main.js:1476`
//! ignores the hitboxes entirely and casts against a single radius-**95** sphere
//! at the hull centre, so beams hit from any angle and felt fine while missiles —
//! which used their own radius-6 test (`missiles.js:402`) — felt broken. Now that
//! [`WeaponRules::boss_hitbox_radius`] is unified at 28 for every weapon, that
//! crutch is gone and the holes are exposed for all three weapons at once.
//!
//! **A capital ship is a box.** The hull here is four axis-aligned boxes tested
//! with the exact [`swept_sphere_aabb`] from [`crate::collision`] — exact at
//! edges and corners, not a conservative expanded box. See [`BOSS_HULL_PARTS`]
//! for how each box's extents were derived from `buildCapitalShip`
//! (`main.js:2561`).
//!
//! The 20 offsets are kept, but demoted: they are no longer the hit test, they
//! are **damage zones**. A hit resolves against the hull volume first, and the
//! contact point is then attributed to the nearest zone ([`nearest_zone`]), which
//! is what locational damage and turret-targeting readouts want and what the
//! spheres were always a bad proxy for.
//!
//! # Determinism
//!
//! One transcendental call reaches simulation state: the patrol drift in
//! [`boss_patrol_pos`] is two sines, and `sin` is not bit-identical across
//! platforms or libm versions (see the crate docs). It is isolated in
//! [`trig::boss_sin`] — a single function, called from a single site, so a
//! future hand-rolled implementation is a one-line change rather than an audit.
//!
//! [`trig::turret_atan2`] is the other trig call, and it is **render-only**: it
//! produces [`Turret::yaw`] and [`Turret::pitch`], which feed [`BossView`] and
//! nothing else. Turret aiming and firing are pure vector arithmetic — see
//! [`solve_turret`] for why that is faithful rather than a shortcut.
//!
//! Everything else here is `+ - * /`, `sqrt`, comparisons, and integer work.
//! Randomness comes from [`World::rng`]: turret spread and reload draw from the
//! combat stream, wave scatter from the spawn stream, matching the stream
//! assignment documented on [`crate::world::WorldRng`].
//!
//! [`World::bullets`]: crate::world::World::bullets
//! [`World::rng`]: crate::world::World::rng
//! [`WeaponRules::boss_bullet_damage`]: crate::rules::WeaponRules::boss_bullet_damage
//! [`WeaponRules::boss_hitbox_radius`]: crate::rules::WeaponRules::boss_hitbox_radius

use crate::collision::{swept_sphere_aabb, Aabb};
use crate::math::Vec3;
use crate::rules::{
    campaign_waves, CampaignRules, Rules, BOSS_HITBOX_COUNT, BOSS_HITBOX_OFFSETS, BOSS_ID_BASE,
    BOSS_TURRET_COUNT, BOSS_TURRET_MUZZLE, BOSS_TURRET_PIVOTS,
};
use crate::world::{
    is_boss_hitbox, Bullet, CampaignHud, CampaignPhase, CampaignState, EntityId, Mode, Quat, Ship,
    ShipKind, SimEvent, Team, Turret, WeaponKind, World,
};

// ---------------------------------------------------------------------------
// Transcendental isolation
// ---------------------------------------------------------------------------

/// The only transcendental functions this module calls.
///
/// Kept in one place because `sin`, `cos`, and `atan2` are **not** guaranteed
/// bit-identical across platforms or libm versions, and this crate must produce
/// bit-identical output on the server and in WASM (see the crate docs). Basic
/// arithmetic and `sqrt` are IEEE-754 exact and are used freely everywhere else.
///
/// Two calls survive the port, and they are not equally dangerous:
///
/// - [`boss_sin`] drives the capital ship's patrol, which moves the hull, which
///   moves the hitboxes. It **does** affect the simulation. It is the one place
///   a cross-platform divergence could accumulate, and the one place to replace
///   if bit-exact server/WASM agreement is ever required.
/// - [`turret_atan2`] produces the turret angles the renderer draws. It affects
///   **nothing** in the simulation; see [`solve_turret`].
pub mod trig {
    /// `sin`, for the capital ship's patrol drift and nothing else.
    ///
    /// `main.js:2659`–`:2660`. Isolated so the one platform-dependent call on a
    /// simulation path is greppable and replaceable in isolation. A polynomial
    /// or table implementation swapped in here changes the patrol path but keeps
    /// it deterministic; nothing else in the module needs to know.
    #[inline]
    #[must_use]
    pub fn boss_sin(radians: f64) -> f64 {
        radians.sin()
    }

    /// `atan2`, for the rendered turret angles and nothing else.
    ///
    /// `main.js:2675` and `:2677`. The result reaches
    /// [`crate::world::Turret::yaw`] / [`crate::world::Turret::pitch`] and from
    /// there [`crate::world::BossView`]. No simulation quantity — not the
    /// muzzle, not the shot direction, not the reload — is derived from it, so a
    /// last-bit difference between two machines shows up as an imperceptibly
    /// different barrel angle and never as a desync.
    #[inline]
    #[must_use]
    pub fn turret_atan2(y: f64, x: f64) -> f64 {
        y.atan2(x)
    }
}

// ---------------------------------------------------------------------------
// Capital ship orientation
// ---------------------------------------------------------------------------

/// Rotates a capital-ship-local offset into world axes.
///
/// The capital ship is built facing `+z` and then yawed 180° about `y`
/// (`main.js:2652`, `setFromAxisAngle(Vector3(0, 1, 0), Math.PI)`), and
/// `updateCapitalShip` never touches its orientation again. A 180° yaw is
/// exactly `(x, y, z) -> (-x, y, -z)`, so the transform is two sign flips: no
/// quaternion, no trig, and every hull box stays axis-aligned, which is what
/// lets [`swept_sphere_aabb`] be used at all.
///
/// **The 20 damage-zone offsets do not go through this.** Despite the JS name
/// `BOSS_HB_OFFSETS_WORLD` they are added to the hull position *unrotated*
/// (`main.js:2664`), and [`BOSS_HITBOX_OFFSETS`] preserves that. The turret
/// pivots (`main.js:2626`) are genuine locals and do go through it.
#[inline]
#[must_use]
pub fn capital_local_to_world(local: Vec3) -> Vec3 {
    Vec3::new(-local.x, local.y, -local.z)
}

// ---------------------------------------------------------------------------
// Hull
// ---------------------------------------------------------------------------

/// Which piece of the capital ship a weapon struck.
///
/// Reported by [`sweep_boss_hull`]. Purely descriptive today — every part takes
/// the same damage — but it is the hook a future locational-damage pass needs,
/// and it makes a failing hull test say *which* box was wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HullPart {
    /// The main slab, plus the engine block at the stern.
    Hull,
    /// The two outboard wings and their accent stripes.
    Wings,
    /// The dorsal spine running most of the ship's length.
    Spine,
    /// The bridge tower and its dome.
    Bridge,
}

/// One axis-aligned piece of the capital ship's hull.
///
/// `center` is a **world-axis offset from the capital ship's position**, i.e.
/// the 180° yaw of [`capital_local_to_world`] has already been applied. Adding
/// the hull position gives a world [`Aabb`] directly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HullBox {
    /// Which piece this is.
    pub part: HullPart,
    /// Offset of the box centre from the capital ship's position, world axes.
    pub center: Vec3,
    /// Half-extents.
    pub half_extents: Vec3,
}

impl HullBox {
    /// This box placed at a capital ship sitting at `boss_pos`.
    #[inline]
    #[must_use]
    pub fn at(self, boss_pos: Vec3) -> Aabb {
        Aabb::new(boss_pos + self.center, self.half_extents)
    }
}

/// The capital ship's collision hull: four boxes, derived from the model.
///
/// # How these extents were chosen
///
/// Straight off `buildCapitalShip` (`main.js:2561`), which is the mesh the
/// player is aiming at. Every part of that model, in ship-local space:
///
/// | Mesh | Geometry | Position | Local extents |
/// |---|---|---|---|
/// | hull | `Box(200, 30, 360)` | `(0, 0, 0)` | x ±100, y ±15, z ±180 |
/// | spine | `Box(40, 10, 340)` | `(0, 20, 0)` | x ±20, y 15‥25, z ±170 |
/// | wing ×2 | `Box(36, 16, 260)` | `(±115, −5, 0)` | \|x\| 97‥133, y −13‥3, z ±130 |
/// | stripe ×2 | `Box(38, 3, 260)` | `(±115, 7, 0)` | \|x\| 96‥134, y 5.5‥8.5, z ±130 |
/// | bridge | `Box(60, 30, 110)` | `(0, 30, 55)` | x ±30, y 15‥45, z 0‥110 |
/// | dome | hemisphere r 18 | `(0, 46, 65)` | x ±18, y 46‥64, z 47‥83 |
/// | accent ×4 | `Box(202, 3, 4)` | `(0, 10, z)` | x ±101 |
/// | engine ring ×8 | cyl r 7 | `(·, ·, −183)` | z −185.5‥−180.5 |
/// | engine glow ×8 | circle r 6.5 | `(·, ·, −186)` | z −186 |
///
/// Grouped into four boxes, each the tight bound of the meshes it covers:
///
/// - **Hull** — hull slab + accent strips + engine block. Local x ±101,
///   y ±15, z −186‥180.
/// - **Wings** — both wings and both stripes. Local x ±134, y −13‥8.5,
///   z ±130. Spanning the full width rather than two separate boxes costs
///   nothing: the middle is already inside Hull, and the wing box's y range sits
///   entirely inside Hull's, so the union is unchanged.
/// - **Spine** — the dorsal ridge. Local x ±20, y 15‥25, z ±170.
/// - **Bridge** — bridge tower + dome. Local x ±30, y 15‥64, z 0‥110.
///
/// Then yawed 180° ([`capital_local_to_world`]), which negates x and z. Only
/// Hull (asymmetric in z because of the engine block) and Bridge (which sits
/// forward of centre) are affected.
///
/// # Cross-check against the 20 offsets
///
/// The sphere cluster agrees with the model on the two axes where it had any
/// resolution: its union spans x ±113 and z ±178 against the model's x ±101
/// (±134 with wings) and z −186‥180. It disagrees on y, spanning ±28 against a
/// deck that is 30 units thick — the spheres were a *fattened* slab, which is
/// why they read as roughly right head-on and are useless from any other angle.
///
/// So the new hull is **not a strict superset** of the old spheres, and that is
/// deliberate: it is tighter than the cluster over the open deck, where the
/// spheres claimed 13 units of empty air above and below a 30-thick hull, and it
/// is solid everywhere the cluster had corridors. There is a test pinning both
/// halves of that trade.
///
/// # Known limitation
///
/// The four turret bases (`main.js:2626`, cylinders at local `(±80, 18, ±110)`
/// reaching y ≈ 25) are **not** hittable volumes. A shot threading 10 units
/// above the deck at a turret's x passes through. They are excluded rather than
/// boxed because the barrel rotates, so no axis-aligned box describes it for
/// more than an instant, and a box drawn around the swept barrel would claim far
/// more air than it earns. The old geometry did not cover them either — nothing
/// above y = 28 was hittable anywhere. Recorded as a gap, not fixed here.
pub const BOSS_HULL_PARTS: [HullBox; 4] = [
    HullBox {
        part: HullPart::Hull,
        // Local centre (0, 0, -3) with half (101, 15, 183); yaw flips z.
        center: Vec3::new(0.0, 0.0, 3.0),
        half_extents: Vec3::new(101.0, 15.0, 183.0),
    },
    HullBox {
        part: HullPart::Wings,
        // Symmetric in z, so the yaw leaves it where it is.
        center: Vec3::new(0.0, -2.25, 0.0),
        half_extents: Vec3::new(134.0, 10.75, 130.0),
    },
    HullBox {
        part: HullPart::Spine,
        center: Vec3::new(0.0, 20.0, 0.0),
        half_extents: Vec3::new(20.0, 5.0, 170.0),
    },
    HullBox {
        part: HullPart::Bridge,
        // Local centre (0, 39.5, 55); the yaw puts the bridge at world -z.
        center: Vec3::new(0.0, 39.5, -55.0),
        half_extents: Vec3::new(30.0, 24.5, 55.0),
    },
];

/// The four hull boxes placed at a capital ship sitting at `boss_pos`.
#[must_use]
pub fn boss_hull_boxes(boss_pos: Vec3) -> [Aabb; 4] {
    [
        BOSS_HULL_PARTS[0].at(boss_pos),
        BOSS_HULL_PARTS[1].at(boss_pos),
        BOSS_HULL_PARTS[2].at(boss_pos),
        BOSS_HULL_PARTS[3].at(boss_pos),
    ]
}

/// One box containing the whole capital ship.
///
/// Used as the broadphase reject in [`sweep_boss_hull`], and useful to a caller
/// that wants to cull the boss before doing anything expensive. It contains
/// every hull box by construction, so missing it is a definite miss.
#[must_use]
pub fn boss_hull_bounds(boss_pos: Vec3) -> Aabb {
    let mut min = Vec3::splat(f64::INFINITY);
    let mut max = Vec3::splat(f64::NEG_INFINITY);
    for b in &BOSS_HULL_PARTS {
        min = min.min(b.center - b.half_extents);
        max = max.max(b.center + b.half_extents);
    }
    Aabb::from_min_max(boss_pos + min, boss_pos + max)
}

/// World centre of damage zone `i`, for a capital ship at `boss_pos`.
///
/// The zone offsets are added without rotation, reproducing `main.js:2664`. Out
/// of range indices clamp to the last zone rather than panicking, so a stale
/// index from a serialized frame cannot take the simulation down.
#[must_use]
pub fn boss_zone_center(i: usize, boss_pos: Vec3) -> Vec3 {
    let i = if i < BOSS_HITBOX_COUNT {
        i
    } else {
        BOSS_HITBOX_COUNT - 1
    };
    boss_pos + BOSS_HITBOX_OFFSETS[i]
}

/// The damage zone nearest to a world point.
///
/// This is what the 20 offsets are *for* now: the hull volume decides whether
/// something was hit, and this decides which part of the ship to credit it to.
/// Ties resolve to the lowest index, and the comparison is on squared distance,
/// so the answer is exact and depends only on the point — never on iteration
/// luck.
#[must_use]
pub fn nearest_zone(point: Vec3, boss_pos: Vec3) -> usize {
    let local = point - boss_pos;
    let mut best = 0usize;
    let mut best_d2 = f64::INFINITY;
    for (i, off) in BOSS_HITBOX_OFFSETS.iter().enumerate() {
        let d2 = local.distance_squared(*off);
        if d2 < best_d2 {
            best_d2 = d2;
            best = i;
        }
    }
    best
}

/// A weapon's contact with the capital ship.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BossHullHit {
    /// Fraction of the step at which contact occurred, in `[0, 1]`.
    pub t: f64,
    /// World contact point, `origin + motion * t`.
    pub point: Vec3,
    /// Which hull box was struck.
    pub part: HullPart,
    /// Nearest damage zone, an index into [`BOSS_HITBOX_OFFSETS`].
    pub zone: usize,
    /// Entity id of that zone's hitbox ship, i.e. `BOSS_ID_BASE + zone`.
    pub zone_id: EntityId,
}

/// Sweeps a moving sphere against the capital ship's hull.
///
/// `motion` is the displacement over the whole step (`velocity * dt`), matching
/// the convention in [`crate::collision`]. A beam or any other hitscan weapon
/// passes its full range as `motion` and `0.0` as `radius`.
///
/// The earliest contact across the four boxes wins; an exact tie resolves to the
/// lowest [`BOSS_HULL_PARTS`] index, so the answer never depends on iteration
/// luck. Each box is tested with the exact [`swept_sphere_aabb`], which solves
/// the rounded edges and corners of the swept volume rather than approximating
/// them with an expanded box — worth having here, where the hull is 268 units
/// wide and a ship is 3.3 across, because the naive version would hang two units
/// of invisible wall off every corner of a very large object.
///
/// This is geometry only: it does not check whether the boss is engaged or
/// damageable. Use [`resolve_weapon_hit`] for that.
#[must_use]
pub fn sweep_boss_hull(
    origin: Vec3,
    motion: Vec3,
    radius: f64,
    boss_pos: Vec3,
) -> Option<BossHullHit> {
    // Conservative reject: the union contains every part, so failing it is a
    // definite miss and the four exact tests are skipped.
    swept_sphere_aabb(origin, motion, radius, boss_hull_bounds(boss_pos))?;

    let mut best: Option<(f64, HullPart)> = None;
    for b in &BOSS_HULL_PARTS {
        if let Some(t) = swept_sphere_aabb(origin, motion, radius, b.at(boss_pos)) {
            match best {
                // Strict `<`: an exact tie keeps the earlier part.
                Some((bt, _)) if t >= bt => {}
                _ => best = Some((t, b.part)),
            }
        }
    }

    let (t, part) = best?;
    let point = origin + motion * t;
    let zone = nearest_zone(point, boss_pos);
    Some(BossHullHit {
        t,
        point,
        part,
        zone,
        zone_id: BOSS_ID_BASE + zone as EntityId,
    })
}

// ---------------------------------------------------------------------------
// Patrol
// ---------------------------------------------------------------------------

/// The capital ship's position after `boss_time` seconds of patrol.
///
/// `main.js:2659`–`:2660`: two independent sines on x and y about
/// [`CampaignRules::boss_base_pos`], with z held. The ship drifts ±88 across the
/// approach lane on a ~70 s period and bobs ±9 on a ~114 s period; the two
/// periods are incommensurate, so the path never repeats on any timescale a
/// mission lasts.
///
/// `boss_time` is [`CampaignState::boss_time`], which advances only while the
/// boss is active (`main.js:2658`), so the drift starts from zero at engagement
/// and the fight opens the same way every run.
///
/// **This is the one place a transcendental function reaches simulation state.**
/// See [`trig`].
#[must_use]
pub fn boss_patrol_pos(rules: &CampaignRules, boss_time: f64) -> Vec3 {
    let base = rules.boss_base_pos;
    Vec3::new(
        base.x + rules.boss_drift_x_amp * trig::boss_sin(boss_time * rules.boss_drift_x_rate),
        base.y + rules.boss_drift_y_amp * trig::boss_sin(boss_time * rules.boss_drift_y_rate),
        base.z,
    )
}

// ---------------------------------------------------------------------------
// Turrets
// ---------------------------------------------------------------------------

/// A turret's solved pose for one tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TurretAim {
    /// World position of the turret pivot.
    pub pivot: Vec3,
    /// World position of the muzzle, where a round is born.
    pub muzzle: Vec3,
    /// Unit direction from the pivot to the target.
    pub aim: Vec3,
    /// Rendered yaw. Cosmetic; see [`solve_turret`].
    pub yaw: f64,
    /// Rendered pitch, clamped to ±[`CampaignRules::turret_pitch_limit`].
    /// Cosmetic.
    pub pitch: f64,
}

/// Solves one turret's pose against a target.
///
/// # What the JS turrets actually do
///
/// Reading `updateCapitalShip` (`main.js:2669`–`:2698`) carefully, the turret
/// rotation is **cosmetic**. Three separate facts establish it:
///
/// 1. The shot direction is recomputed from scratch as
///    `(player - muzzle).normalize()` (`main.js:2683`). It is never taken from
///    the barrel. The ±0.7 rad pitch clamp at `main.js:2679` therefore limits
///    where the barrel *looks*, not where the turret can *shoot* — a turret with
///    the player directly overhead still hits them.
/// 2. The barrel does not point at the target anyway. `pivot.rotation` is a
///    `THREE.Euler` in the default `XYZ` order, so the pitch is applied about
///    the *parent's* x-axis after the yaw rather than about the yawed x-axis.
///    Working the composition through, the barrel's local `+z` ends up at
///    `(dx/h, dy·dz/(h·L), dz/L)` where `h = hypot(dx, dz)` and `L = |d|` —
///    equal to the target direction `d/L` only when `dy = 0`. The gun visibly
///    points wrong whenever the player is above or below the turret.
/// 3. The muzzle *position* is read from `pivot.quaternion` **before** that
///    frame's rotation is assigned (`main.js:2670` precedes `main.js:2678`), so
///    even the spawn point uses the previous frame's aim.
///
/// # What this port does
///
/// Aiming and firing are pure vector arithmetic. The muzzle is placed along the
/// true aim line — `pivot + aim * 22`, plus the barrel's 1.8-unit rise
/// perpendicular to it — which is where the model *intends* the muzzle to be,
/// and which removes `sin`/`cos` from the path that decides where a round is
/// born. Given (2), matching the JS bit-for-bit would mean reproducing a
/// composition bug in order to displace the muzzle by a few units off a barrel
/// that was pointing the wrong way; the round's direction is recomputed from the
/// muzzle in both versions, so the observable difference is a fraction of the
/// ±0.045 rad spread already applied to every shot.
///
/// [`TurretAim::yaw`] and [`TurretAim::pitch`] reproduce the JS angles exactly,
/// clamp included, because the renderer draws them — and *only* the renderer
/// draws them. They are the sole consumers of [`trig::turret_atan2`].
#[must_use]
pub fn solve_turret(
    local_pivot: Vec3,
    boss_pos: Vec3,
    target: Vec3,
    rules: &CampaignRules,
) -> TurretAim {
    let pivot = boss_pos + capital_local_to_world(local_pivot);
    let to_target = target - pivot;

    // A target exactly on the pivot has no direction; hold the turret facing
    // along the hull's nose rather than producing a NaN.
    let aim = to_target
        .try_normalize()
        .unwrap_or_else(|| capital_local_to_world(Vec3::Z));

    // The barrel's 1.8-unit rise, perpendicular to the aim line. Gram-Schmidt
    // against world up, with a fallback for a turret aimed straight up or down.
    let up = (Vec3::Y - aim * aim.dot(Vec3::Y))
        .try_normalize()
        .unwrap_or(Vec3::Y);
    let muzzle = pivot + aim * BOSS_TURRET_MUZZLE.z + up * BOSS_TURRET_MUZZLE.y;

    // Render-only from here down. `local` is the aim expressed in ship space,
    // which for a 180° yaw is the same two sign flips as the forward transform.
    let local = capital_local_to_world(to_target);
    let yaw = trig::turret_atan2(local.x, local.z);
    let horiz = (local.x * local.x + local.z * local.z).sqrt();
    let pitch = -trig::turret_atan2(local.y, horiz);
    let limit = rules.turret_pitch_limit;
    let pitch = pitch.clamp(-limit, limit);

    TurretAim {
        pivot,
        muzzle,
        aim,
        yaw,
        pitch,
    }
}

/// The turret reload window for a boss at `hp_fraction` health.
///
/// `main.js:2692`–`:2694`, as `[min, range]`; the delay drawn is
/// `min + random() * range`. Three bands: a lazy 2.8–3.5 s above 65 % health,
/// 1.6–2.1 s down to 35 %, and a frantic 0.9–1.2 s below that. A dying capital
/// ship fires roughly three times as fast as a healthy one, which is the fight's
/// entire difficulty curve.
#[must_use]
pub fn turret_reload_window(rules: &CampaignRules, hp_fraction: f64) -> [f64; 2] {
    if hp_fraction > rules.boss_hp_threshold_high {
        rules.turret_reload_healthy
    } else if hp_fraction > rules.boss_hp_threshold_low {
        rules.turret_reload_wounded
    } else {
        rules.turret_reload_critical
    }
}

// ---------------------------------------------------------------------------
// Mission geometry the rules module does not carry
// ---------------------------------------------------------------------------

/// Respawn checkpoint at the start of a mission. `main.js:2333`.
///
/// Equal to `(0, spawn.space_y, -spawn.space_z)` — the team-0 spawn at the
/// friendly mothership's hangar mouth — but written as a literal in the JS, so it
/// is written as a literal here too rather than implying a link that the original
/// does not have.
pub const CHECKPOINT_START: Vec3 = Vec3::new(0.0, 0.0, -540.0);

/// Height of a between-waves checkpoint. `main.js:2869`.
pub const CHECKPOINT_WAVE_Y: f64 = 20.0;

/// How far *short* of the next wave's spawn anchor the checkpoint is placed.
/// `main.js:2869` (`(nextWave?.spawnZ ?? 20) - 80`). You respawn 80 units behind
/// the enemies you are about to meet, not on top of them.
pub const CHECKPOINT_WAVE_LEAD: f64 = 80.0;

/// Respawn checkpoint for the boss fight. `main.js:2875` — 150 units short of
/// [`CampaignRules::boss_base_pos`], and lower, so a respawn faces the capital
/// ship head-on.
pub const CHECKPOINT_BOSS: Vec3 = Vec3::new(0.0, 10.0, 450.0);

/// Waves per mission. All three missions run three waves and then the boss.
pub const WAVES_PER_MISSION: usize = 3;

// ---------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------

/// Builds the campaign state for `mission` and puts wave 1 on the field.
///
/// Replaces the scattered initialisation at `main.js:2320`–`:2338`,
/// `main.js:2913` (`spawnSoloEntities`'s campaign branch) and `main.js:2918`
/// (the boss-hitbox loop). Creates:
///
/// - [`World::campaign`], with three lives and the opening checkpoint;
/// - the [`BOSS_HITBOX_COUNT`] hitbox ships, ids `9000..9019`, parked at the
///   capital ship's rest position and **not alive** until the boss engages
///   (`main.js:2938`);
/// - the first wave's bots.
///
/// `hard_mode` is the lobby's difficulty flag (`main.js:2495`), which campaign
/// bots inherit like any other bot.
///
/// Does nothing unless `world.mode` is [`Mode::Campaign`]; the mission number is
/// taken from that mode, so the world and the campaign can never disagree about
/// which mission is running.
///
/// [`World::campaign`]: crate::world::World::campaign
pub fn init(world: &mut World, hard_mode: bool) {
    let Mode::Campaign(mission) = world.mode else {
        return;
    };

    let boss_pos = world.rules.campaign.boss_base_pos;
    let mut turrets = [Turret::default(); BOSS_TURRET_COUNT];
    for (i, t) in turrets.iter_mut().enumerate() {
        t.local_pos = BOSS_TURRET_PIVOTS[i];
        // `main.js:2649`: `fireTimer: i * 0.85 + 0.4`, so the four guns open on
        // a stagger instead of a volley.
        t.fire_timer = i as f64 * world.rules.campaign.turret_stagger
            + world.rules.campaign.turret_stagger_offset;
    }

    world.campaign = Some(CampaignState {
        mission,
        phase: CampaignPhase::Wave,
        wave_index: 0,
        wave_bot_ids: Vec::new(),
        bots_alive: 0,
        between: false,
        between_timer: 0.0,
        lives: world.rules.campaign.lives,
        checkpoint_pos: CHECKPOINT_START,
        next_bot_id: world.rules.campaign.first_bot_id,
        warp_timer: 0.0,
        boss_hp: world.rules.campaign.boss_max_hp,
        boss_active: false,
        boss_pos,
        boss_time: 0.0,
        turrets,
    });

    // The hitboxes exist from the first frame so nothing has to allocate ids
    // mid-fight, but they are dead until `activate_boss`.
    for (i, offset) in BOSS_HITBOX_OFFSETS.iter().enumerate() {
        let mut s = Ship::spawn(
            BOSS_ID_BASE + i as EntityId,
            ShipKind::BossHitbox,
            boss_pos + *offset,
            Quat::IDENTITY,
            &world.rules,
        );
        s.team = Some(Team::One);
        s.hp = world.rules.campaign.boss_max_hp;
        s.alive = false;
        s.invuln_timer = 0.0;
        // The one legitimate hit-radius override in the game. Retained so that
        // any weapon still resolving against individual hitboxes agrees with the
        // rules; the hull sweep is the primary path.
        s.hit_radius_override = Some(world.rules.weapons.boss_hitbox_radius);
        world.ships.push(s);
    }

    spawn_wave_with(world, 0, hard_mode);
}

/// Spawns wave `index` of the current mission.
///
/// `main.js:2700` (`spawnCampaignWave`). The wave's bots are scattered about an
/// anchor at `(0, wave_anchor_y, wave.spawn_z)` by
/// [`CampaignRules::wave_spawn_jitter`], drawing x, y, then z from the spawn
/// stream in that order so a given seed reproduces the JS draw sequence's shape.
///
/// Bots are team 1, flagged [`crate::world::BotState::is_campaign_bot`] so the
/// respawn path leaves them dead (`main.js:3281`) — a campaign wave is finite by
/// definition, and a respawning wave would never clear.
pub fn spawn_wave(world: &mut World, index: usize) {
    // Waves after the first inherit the difficulty of the ones before them.
    // Dead bots stay in `ships`, so a cleared wave is still readable here; the
    // first wave is spawned by `init`, which knows the flag outright.
    let hard = world
        .ships
        .iter()
        .find(|s| s.bot.is_campaign_bot)
        .is_some_and(|s| s.bot.hard_mode);
    spawn_wave_with(world, index, hard);
}

/// [`spawn_wave`] with the difficulty stated rather than inferred.
fn spawn_wave_with(world: &mut World, index: usize, hard: bool) {
    let Some(mut camp) = world.campaign.take() else {
        return;
    };
    let waves = campaign_waves(camp.mission);
    let Some(wave) = waves.get(index) else {
        world.campaign = Some(camp);
        return;
    };

    camp.wave_index = index;
    camp.wave_bot_ids.clear();
    camp.bots_alive = 0;

    let anchor = Vec3::new(
        0.0,
        world.rules.campaign.wave_anchor_y,
        f64::from(wave.spawn_z),
    );
    let jitter = world.rules.campaign.wave_spawn_jitter;

    for _ in 0..wave.count {
        let id = camp.next_bot_id;
        camp.next_bot_id += 1;
        // Draw order is load-bearing: x, y, z, exactly as `main.js:2708`.
        let dx = world.rng.spawn.next_f64_signed() * 0.5 * jitter.x;
        let dy = world.rng.spawn.next_f64_signed() * 0.5 * jitter.y;
        let dz = world.rng.spawn.next_f64_signed() * 0.5 * jitter.z;
        let pos = anchor + Vec3::new(dx, dy, dz);

        let mut s = Ship::spawn(id, ShipKind::Bot, pos, Quat::IDENTITY, &world.rules);
        s.team = Some(Team::One);
        s.bot.is_campaign_bot = true;
        s.bot.hard_mode = hard;
        s.bot.missiles_left = world.rules.bot.missile_max_for(hard);
        s.missiles_left = s.bot.missiles_left;
        world.ships.push(s);

        camp.wave_bot_ids.push(id);
        camp.bots_alive += 1;
    }

    world.campaign = Some(camp);
}

// ---------------------------------------------------------------------------
// Tick
// ---------------------------------------------------------------------------

/// Advances the mission by one step.
///
/// `main.js:2834` (`updateCampaign`), including its control flow: the
/// between-waves pause short-circuits the rest of the function, and the warp
/// timer is updated *after* the phase work and therefore not at all while a pause
/// is running. That ordering is reproduced rather than tidied, because the warp
/// flash's duration is observable.
///
/// Boss rounds fired this tick are appended to [`World::bullets`]; moving them
/// and resolving them against the player is the bullets module's job.
///
/// [`World::bullets`]: crate::world::World::bullets
pub fn update(world: &mut World, dt: f64, events: &mut Vec<SimEvent>) {
    let Some(mut camp) = world.campaign.take() else {
        return;
    };

    // `if (campaignOver) return;`
    if matches!(camp.phase, CampaignPhase::Victory | CampaignPhase::Failed) {
        world.campaign = Some(camp);
        return;
    }

    if camp.between {
        camp.between_timer -= dt;
        if camp.between_timer <= 0.0 {
            camp.between = false;
            camp.between_timer = 0.0;
            if camp.phase == CampaignPhase::Boss {
                world.campaign = Some(camp);
                activate_boss(world, events);
                return;
            }
            let next = camp.wave_index;
            world.campaign = Some(camp);
            spawn_wave(world, next);
            return;
        }
        world.campaign = Some(camp);
        return;
    }

    match camp.phase {
        CampaignPhase::Wave => step_wave(world, &mut camp, events),
        CampaignPhase::Boss => step_boss(world, &mut camp, dt, events),
        CampaignPhase::Victory | CampaignPhase::Failed => {}
    }

    if camp.warp_timer > 0.0 {
        camp.warp_timer -= dt;
        if camp.warp_timer <= 0.0 {
            camp.warp_timer = 0.0;
        }
    }

    world.campaign = Some(camp);
}

/// Wave phase: count survivors, and advance when the field is clear.
///
/// `main.js:2855`–`:2882`. The `!is_empty()` guard is what stops a cleared wave
/// from re-triggering every tick: the id list is emptied on completion, so the
/// clause can only fire once per wave.
fn step_wave(world: &World, camp: &mut CampaignState, events: &mut Vec<SimEvent>) {
    let alive = camp
        .wave_bot_ids
        .iter()
        .filter(|id| world.ship(**id).is_some_and(|s| s.alive))
        .count() as u32;
    camp.bots_alive = alive;

    if alive > 0 || camp.wave_bot_ids.is_empty() {
        return;
    }

    camp.wave_bot_ids.clear();
    let cleared = camp.wave_index;
    events.push(SimEvent::WaveComplete { index: cleared });

    let waves = campaign_waves(camp.mission);
    if cleared + 1 < WAVES_PER_MISSION {
        let next = &waves[cleared + 1];
        camp.checkpoint_pos = Vec3::new(
            0.0,
            CHECKPOINT_WAVE_Y,
            f64::from(next.spawn_z) - CHECKPOINT_WAVE_LEAD,
        );
        camp.wave_index = cleared + 1;
        camp.between = true;
        camp.between_timer = world.rules.campaign.wave_gap;
    } else {
        camp.checkpoint_pos = CHECKPOINT_BOSS;
        camp.phase = CampaignPhase::Boss;
        camp.between = true;
        camp.between_timer = world.rules.campaign.boss_gap;
    }
}

/// Boss phase: drift the hull, drag the hitboxes with it, aim and fire.
///
/// `main.js:2656` (`updateCapitalShip`).
fn step_boss(world: &mut World, camp: &mut CampaignState, dt: f64, events: &mut Vec<SimEvent>) {
    if !camp.boss_active {
        return;
    }

    camp.boss_time += dt;
    camp.boss_pos = boss_patrol_pos(&world.rules.campaign, camp.boss_time);

    // The hitboxes are slaved to the hull, unrotated (`main.js:2664`).
    for s in world.ships.iter_mut().filter(|s| is_boss_hitbox(s.id)) {
        let i = (s.id - BOSS_ID_BASE) as usize;
        s.pos = camp.boss_pos + BOSS_HITBOX_OFFSETS[i];
        s.vel = Vec3::ZERO;
    }

    // The campaign is solo, and the JS aims every turret at the local player
    // unconditionally (`main.js:2673`). With no local ship there is nothing to
    // track, so the turrets hold their pose.
    let Some(player) = world.local_ship() else {
        return;
    };
    let player_pos = player.pos;
    let player_alive = player.alive;

    let rules = world.rules;
    let hp_fraction = f64::from(camp.boss_hp) / f64::from(rules.campaign.boss_max_hp);
    let mut shots: Vec<(Vec3, Vec3)> = Vec::new();

    for t in camp.turrets.iter_mut() {
        let aim = solve_turret(t.local_pos, camp.boss_pos, player_pos, &rules.campaign);
        t.yaw = aim.yaw;
        t.pitch = aim.pitch;

        // `main.js:2680` gates the whole countdown on the player being alive, so
        // reload does not tick away while the respawn timer runs.
        if !player_alive {
            continue;
        }
        t.fire_timer -= dt;
        if t.fire_timer > 0.0 {
            continue;
        }

        // Direction is taken from the muzzle to the player, not from the barrel.
        // See `solve_turret`.
        let mut dir = (player_pos - aim.muzzle)
            .try_normalize()
            .unwrap_or_else(|| capital_local_to_world(Vec3::Z));
        // Three combat draws per shot, in the JS order: spread x, spread y, then
        // the reload roll.
        let spread = rules.campaign.turret_spread;
        dir.x += world.rng.combat.next_f64_signed() * spread;
        dir.y += world.rng.combat.next_f64_signed() * spread;
        let dir = dir.try_normalize().unwrap_or(aim.aim);

        shots.push((aim.muzzle, dir));

        let window = turret_reload_window(&rules.campaign, hp_fraction);
        t.fire_timer = window[0] + world.rng.combat.next_f64() * window[1];
    }

    for (muzzle, dir) in shots {
        let key = world.take_projectile_key();
        world.bullets.push(Bullet {
            key,
            pos: muzzle,
            prev_pos: muzzle,
            vel: dir * rules.weapons.boss_bullet_speed,
            life: rules.weapons.boss_bullet_life,
            owner: BOSS_ID_BASE,
            // The capital ship is team 1 (`main.js:2741`), so friendly fire
            // rejection keeps its rounds off its own hitboxes.
            owner_team: Some(Team::One),
            owner_coarse_aim: false,
            damage: rules.weapons.boss_bullet_damage,
        });
        events.push(SimEvent::Fired {
            owner: BOSS_ID_BASE,
            weapon: WeaponKind::Bullet,
            origin: muzzle,
            dir,
        });
    }
}

/// Brings the capital ship online.
///
/// `main.js:2727` (`activateBossPhase`). Restores full boss hit points, wakes
/// every hitbox, and resets the patrol clock so the fight opens identically each
/// run.
///
/// The hitboxes get the ordinary spawn-invulnerability window. That is the
/// unified rule ([`crate::rules::CombatRules::spawn_invuln`] — *every* ship
/// carries one, including boss hitboxes), and it costs the player nothing in
/// practice: the boss engages at z = 600 while the player respawns at
/// [`CHECKPOINT_BOSS`], 150 units away, which is about two seconds of flight.
pub fn activate_boss(world: &mut World, events: &mut Vec<SimEvent>) {
    let Some(mut camp) = world.campaign.take() else {
        return;
    };

    camp.boss_active = true;
    camp.boss_hp = world.rules.campaign.boss_max_hp;
    camp.boss_time = 0.0;
    camp.boss_pos = boss_patrol_pos(&world.rules.campaign, 0.0);
    camp.phase = CampaignPhase::Boss;

    for s in world.ships.iter_mut().filter(|s| is_boss_hitbox(s.id)) {
        let i = (s.id - BOSS_ID_BASE) as usize;
        s.pos = camp.boss_pos + BOSS_HITBOX_OFFSETS[i];
        s.alive = true;
        s.hp = camp.boss_hp;
        s.invuln_timer = world.rules.combat.spawn_invuln;
    }

    world.campaign = Some(camp);
    events.push(SimEvent::BossPhaseStarted);
}

// ---------------------------------------------------------------------------
// Damage
// ---------------------------------------------------------------------------

/// Whether the capital ship can currently be hit.
///
/// True only during the boss phase, with the boss engaged, and with the hitbox
/// ships past their spawn window. All twenty share one timer, so reading the
/// centre one is well defined.
#[must_use]
pub fn boss_is_engageable(world: &World) -> bool {
    let Some(camp) = world.campaign.as_ref() else {
        return false;
    };
    camp.phase == CampaignPhase::Boss
        && camp.boss_active
        && camp.boss_hp > 0
        && world
            .ship(BOSS_ID_BASE)
            .is_some_and(crate::world::Ship::is_damageable)
}

/// Sweeps a weapon against the boss, gated on the boss being engageable.
///
/// This is the entry point the bullet, missile, and beam modules want: it
/// answers "did this shot hit the capital ship, and where" without any of them
/// needing to know what a campaign phase is. Returns `None` outside the boss
/// fight.
#[must_use]
pub fn resolve_weapon_hit(
    world: &World,
    origin: Vec3,
    motion: Vec3,
    radius: f64,
) -> Option<BossHullHit> {
    if !boss_is_engageable(world) {
        return None;
    }
    let boss_pos = world.campaign.as_ref()?.boss_pos;
    sweep_boss_hull(origin, motion, radius, boss_pos)
}

/// Applies damage to the capital ship, and ends the mission if it dies.
///
/// `main.js:2719` (`applyBossHit`). The boss has one shared hit point pool: all
/// twenty hitboxes are the same target, which is why damage arrives here rather
/// than at a [`Ship`].
///
/// Where the JS mirrors the pool onto the centre hitbox only (`main.js:2723`,
/// for the HUD), this mirrors onto all twenty, so nothing can read a stale
/// number off hitbox 7 and disagree with the health bar.
///
/// Returns `true` if the damage landed.
pub fn apply_boss_damage(
    world: &mut World,
    amount: i32,
    source: Option<EntityId>,
    events: &mut Vec<SimEvent>,
) -> bool {
    if amount <= 0 || !boss_is_engageable(world) {
        return false;
    }
    let Some(mut camp) = world.campaign.take() else {
        return false;
    };

    camp.boss_hp = (camp.boss_hp - amount).max(0);
    let new_hp = camp.boss_hp;
    for s in world.ships.iter_mut().filter(|s| is_boss_hitbox(s.id)) {
        s.hp = new_hp;
        s.hit_flash = 1.0;
    }
    events.push(SimEvent::Damaged {
        id: BOSS_ID_BASE,
        amount,
        new_hp,
        source,
    });

    world.campaign = Some(camp);
    if new_hp <= 0 {
        end_victory(world, events);
    }
    true
}

/// Ends the mission in victory.
///
/// `main.js:2787` (`endCampaignVictory`). The hitboxes go dead so no late
/// projectile can score on a destroyed ship.
pub fn end_victory(world: &mut World, events: &mut Vec<SimEvent>) {
    let Some(mut camp) = world.campaign.take() else {
        return;
    };
    let lives_left = camp.lives;
    camp.phase = CampaignPhase::Victory;
    camp.boss_active = false;
    camp.boss_hp = 0;
    let pos = camp.boss_pos;

    for s in world.ships.iter_mut().filter(|s| is_boss_hitbox(s.id)) {
        s.alive = false;
        s.hp = 0;
    }

    world.campaign = Some(camp);
    events.push(SimEvent::ShipDestroyed {
        id: BOSS_ID_BASE,
        killer: world.local_id,
        pos,
    });
    events.push(SimEvent::CampaignVictory { lives_left });
}

// ---------------------------------------------------------------------------
// Lives and respawn
// ---------------------------------------------------------------------------

/// Spends a life after the player dies, and fails the mission at zero.
///
/// `main.js:3238`–`:3256`, the campaign branch of `applyPlayerDamageLocal`. Call
/// this once per player death, after the death itself has been resolved.
///
/// On the surviving path it arms the warp-in effect and sets the player's
/// respawn timer to [`crate::rules::CombatRules::campaign_respawn_delay`] — the
/// campaign respawns faster than the rest of the game because the warp flash
/// covers the gap. On the last life it clears the respawn timer instead: there
/// is nothing to come back as.
///
/// Returns the lives remaining.
pub fn on_player_death(world: &mut World, events: &mut Vec<SimEvent>) -> i32 {
    let Some(mut camp) = world.campaign.take() else {
        return 0;
    };
    if matches!(camp.phase, CampaignPhase::Victory | CampaignPhase::Failed) {
        let lives = camp.lives;
        world.campaign = Some(camp);
        return lives;
    }

    camp.lives = (camp.lives - 1).max(0);
    let lives = camp.lives;

    if lives <= 0 {
        camp.phase = CampaignPhase::Failed;
        camp.warp_timer = 0.0;
        if let Some(id) = world.local_id {
            if let Some(s) = world.ship_mut(id) {
                s.respawn_timer = 0.0;
            }
        }
        world.campaign = Some(camp);
        events.push(SimEvent::CampaignFailed);
        return 0;
    }

    camp.warp_timer = world.rules.campaign.warp_duration;
    let delay = world.rules.combat.campaign_respawn_delay;
    if let Some(id) = world.local_id {
        if let Some(s) = world.ship_mut(id) {
            s.respawn_timer = delay;
        }
    }
    world.campaign = Some(camp);
    lives
}

/// Where and how healthy the player comes back.
///
/// `main.js:3324` (checkpoint and identity orientation) and `main.js:3343`
/// (`Math.floor(SHIP_MAX_HP * 0.55)`). **You come back hurt**: 55 of 100 hit
/// points, so a life spent is a real cost rather than a short pause, and the
/// health-regeneration rules have something to do.
///
/// The checkpoint advances as waves clear ([`CHECKPOINT_START`], then 80 units
/// short of each wave's anchor, then [`CHECKPOINT_BOSS`]), so a death late in a
/// mission does not send the player back across the map.
///
/// Returns `None` outside the campaign, or once the mission is over.
#[must_use]
pub fn respawn_pose(world: &World) -> Option<(Vec3, Quat, i32)> {
    let camp = world.campaign.as_ref()?;
    if matches!(camp.phase, CampaignPhase::Victory | CampaignPhase::Failed) {
        return None;
    }
    let hp = respawn_hp(&world.rules);
    Some((camp.checkpoint_pos, Quat::IDENTITY, hp))
}

/// Hit points a campaign respawn grants: `floor(max_hp * respawn_hp_fraction)`.
///
/// Separated so a test can assert the flooring without building a world.
#[must_use]
pub fn respawn_hp(rules: &Rules) -> i32 {
    let hp = (f64::from(rules.ship.max_hp) * rules.campaign.respawn_hp_fraction).floor();
    // `floor` of a product of finite positives cannot exceed `max_hp`, but the
    // clamp keeps a custom rule set from producing a negative or absurd value.
    (hp as i32).clamp(1, rules.ship.max_hp)
}

// ---------------------------------------------------------------------------
// Readouts
// ---------------------------------------------------------------------------

/// The capital ship as the renderer wants it, or `None` outside the campaign.
///
/// The turret angles here are the render-only values from
/// [`trig::turret_atan2`]; see [`solve_turret`].
#[must_use]
pub fn boss_view(world: &World) -> Option<crate::world::BossView> {
    let camp = world.campaign.as_ref()?;
    let mut yaw = [0.0f32; BOSS_TURRET_COUNT];
    let mut pitch = [0.0f32; BOSS_TURRET_COUNT];
    for (i, t) in camp.turrets.iter().enumerate() {
        yaw[i] = t.yaw as f32;
        pitch[i] = t.pitch as f32;
    }
    Some(crate::world::BossView {
        pos: [
            camp.boss_pos.x as f32,
            camp.boss_pos.y as f32,
            camp.boss_pos.z as f32,
        ],
        turret_yaw: yaw,
        turret_pitch: pitch,
        hp: camp.boss_hp,
        max_hp: world.rules.campaign.boss_max_hp,
    })
}

/// The campaign HUD readout. Zeroed outside the campaign.
///
/// Replaces `updateCampaignHud` (`main.js:2530`) reading closure variables.
#[must_use]
pub fn campaign_hud(world: &World) -> CampaignHud {
    let Some(camp) = world.campaign.as_ref() else {
        return CampaignHud::default();
    };
    let max = f64::from(world.rules.campaign.boss_max_hp);
    CampaignHud {
        active: true,
        mission: camp.mission,
        wave: camp.wave_index as u8,
        enemies_left: camp.bots_alive,
        lives: camp.lives,
        boss_active: camp.boss_active,
        boss_hp01: if max > 0.0 {
            (f64::from(camp.boss_hp.max(0)) / max) as f32
        } else {
            0.0
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collision::{sweep_first_hit, Sphere};
    use crate::rules::CAMPAIGN_WAVES_M1;
    use crate::world::{MapKind, TICK_DT};

    fn v(x: f64, y: f64, z: f64) -> Vec3 {
        Vec3::new(x, y, z)
    }

    /// A campaign world with a local player, ready to tick.
    fn world_at(mission: u8) -> World {
        let mut w = World::new(
            0x0CA4_79A1_u64,
            Rules::DEFAULT,
            Mode::Campaign(mission),
            MapKind::Space,
        );
        let mut me = Ship::spawn(
            1,
            ShipKind::Local,
            CHECKPOINT_START,
            Quat::IDENTITY,
            &w.rules,
        );
        me.team = Some(Team::Zero);
        me.invuln_timer = 0.0;
        w.ships.push(me);
        w.local_id = Some(1);
        init(&mut w, false);
        w
    }

    /// The hitbox cluster exactly as `main.js:2298` builds it, for the
    /// gap-closure comparisons.
    fn legacy_spheres(boss_pos: Vec3) -> Vec<Sphere> {
        BOSS_HITBOX_OFFSETS
            .iter()
            .map(|o| Sphere::new(boss_pos + *o, Rules::DEFAULT.weapons.boss_hitbox_radius))
            .collect()
    }

    fn legacy_hits(origin: Vec3, motion: Vec3, radius: f64, boss_pos: Vec3) -> bool {
        sweep_first_hit(origin, motion, radius, &legacy_spheres(boss_pos)).is_some()
    }

    // -----------------------------------------------------------------
    // The three gaps this module exists to close.
    // -----------------------------------------------------------------

    #[test]
    fn gap_one_a_shot_along_x_56_5_no_longer_threads_between_the_columns() {
        // The hitbox columns sit at x = ±28 and ±85 with radius 28, so they are
        // 57 apart and 56 across. x = 56.5 is the exact midline of the corridor
        // between the 28 and 85 columns: 28.5 from each centre, half a unit
        // clear of both.
        let boss = Rules::DEFAULT.campaign.boss_base_pos;
        let origin = boss + v(56.5, 0.0, -400.0);
        let motion = v(0.0, 0.0, 800.0);

        assert!(
            !legacy_hits(origin, motion, 0.0, boss),
            "the corridor must really exist, or this test proves nothing"
        );

        let hit = sweep_boss_hull(origin, motion, 0.0, boss).expect("the hull must be solid here");
        assert_eq!(hit.part, HullPart::Hull);
        assert!((0.0..=1.0).contains(&hit.t));
        // Contact on the stern face of the hull box: centre z + 3, half 183.
        assert!(
            (hit.point.z - (boss.z + 3.0 - 183.0)).abs() < 1e-9,
            "entered at z = {}",
            hit.point.z
        );

        // And from the other three approaches down the same corridor.
        for (o, m) in [
            (boss + v(56.5, 0.0, 400.0), v(0.0, 0.0, -800.0)),
            (boss + v(56.5, -300.0, 0.0), v(0.0, 600.0, 0.0)),
            (boss + v(56.5, 300.0, 0.0), v(0.0, -600.0, 0.0)),
        ] {
            assert!(
                sweep_boss_hull(o, m, 0.0, boss).is_some(),
                "missed the hull from {o:?}"
            );
        }
    }

    #[test]
    fn gap_two_a_shot_through_a_z_corridor_no_longer_passes_between_the_rows() {
        // Rows at z = -150/-75/0/75/150, radius 28: 75 apart, 56 across, leaving
        // a 19-unit corridor. z = 37.5 is its midline, 37.5 from both the 0 and
        // 75 rows.
        let boss = Rules::DEFAULT.campaign.boss_base_pos;
        let origin = boss + v(-400.0, 0.0, 37.5);
        let motion = v(800.0, 0.0, 0.0);

        assert!(
            !legacy_hits(origin, motion, 0.5, boss),
            "the z corridor must really exist"
        );

        let hit = sweep_boss_hull(origin, motion, 0.5, boss).expect("hull must be solid");
        // The widest thing at this height is the wing box, half-width 134.
        assert_eq!(hit.part, HullPart::Wings);
        assert!(hit.point.x <= boss.x - 100.0);

        // Every corridor midline, from both sides, at deck height.
        for z_mid in [-112.5, -37.5, 37.5, 112.5] {
            for (o, m) in [
                (boss + v(-400.0, 0.0, z_mid), v(800.0, 0.0, 0.0)),
                (boss + v(400.0, 0.0, z_mid), v(-800.0, 0.0, 0.0)),
            ] {
                assert!(
                    !legacy_hits(o, m, 0.5, boss),
                    "corridor at z = {z_mid} was not actually open"
                );
                assert!(
                    sweep_boss_hull(o, m, 0.5, boss).is_some(),
                    "hull missed at z = {z_mid}"
                );
            }
        }
    }

    #[test]
    fn gap_three_a_shot_above_the_deck_plane_now_hits_the_superstructure() {
        let boss = Rules::DEFAULT.campaign.boss_base_pos;

        // Over the bridge. Note *where* the bridge is: the model puts it at
        // local z = +55, and the hull is yawed 180deg, so in world space it sits
        // at z = -55. The lone off-plane hitbox sphere is at (0, 30, +50) —
        // unrotated, and therefore on the wrong end of the ship entirely, 105
        // units from the tower it was meant to represent.
        let origin = boss + v(-300.0, 40.0, -55.0);
        let motion = v(600.0, 0.0, 0.0);
        assert!(
            !legacy_hits(origin, motion, 0.5, boss),
            "nothing should have been hittable 40 units up at the bridge"
        );
        let hit = sweep_boss_hull(origin, motion, 0.5, boss).expect("bridge must be solid");
        assert_eq!(hit.part, HullPart::Bridge);

        // Over the spine, well aft of the bridge.
        let origin = boss + v(-300.0, 20.0, 120.0);
        assert!(!legacy_hits(origin, motion, 0.5, boss));
        let hit = sweep_boss_hull(origin, motion, 0.5, boss).expect("spine must be solid");
        assert_eq!(hit.part, HullPart::Spine);

        // Straight down onto the dome from above.
        let origin = boss + v(0.0, 300.0, -55.0);
        let motion = v(0.0, -400.0, 0.0);
        assert!(!legacy_hits(origin, motion, 0.5, boss));
        assert_eq!(
            sweep_boss_hull(origin, motion, 0.5, boss).map(|h| h.part),
            Some(HullPart::Bridge)
        );
    }

    #[test]
    fn the_deck_is_solid_where_the_sphere_cluster_was_a_sieve() {
        // Sweep the whole 200x360 deck footprint with vertical rays and count.
        // The old cluster leaks through both corridor families; the box hull
        // must not leak anywhere.
        let boss = Rules::DEFAULT.campaign.boss_base_pos;
        let mut legacy_miss = 0;
        let mut total = 0;
        for xi in -40..=40 {
            for zi in -70..=70 {
                let x = f64::from(xi) * 2.5;
                let z = f64::from(zi) * 2.5;
                let o = boss + v(x, 200.0, z);
                let m = v(0.0, -400.0, 0.0);
                total += 1;
                if !legacy_hits(o, m, 0.5, boss) {
                    legacy_miss += 1;
                }
                assert!(
                    sweep_boss_hull(o, m, 0.5, boss).is_some(),
                    "hull leaked at ({x}, {z})"
                );
            }
        }
        // Not a marginal defect: a large fraction of a capital ship's plan view
        // was not there.
        assert!(
            legacy_miss * 4 > total,
            "expected the cluster to leak badly; {legacy_miss} of {total}"
        );
    }

    #[test]
    fn the_new_hull_is_tighter_than_the_cluster_where_the_cluster_over_claimed() {
        // The trade recorded in `BOSS_HULL_PARTS`: the spheres reached 28 units
        // above a deck that is 15 thick, so a shot 20 up over open deck used to
        // "hit" empty air. It now misses, and that is correct.
        let boss = Rules::DEFAULT.campaign.boss_base_pos;
        let origin = boss + v(85.0, 20.0, -300.0);
        let motion = v(0.0, 0.0, 600.0);
        assert!(legacy_hits(origin, motion, 0.5, boss));
        assert!(sweep_boss_hull(origin, motion, 0.5, boss).is_none());

        // Clean misses stay misses: well clear of the widest part.
        let wide = boss + v(200.0, 0.0, -300.0);
        assert!(sweep_boss_hull(wide, motion, 0.5, boss).is_none());
        assert!(sweep_boss_hull(boss + v(0.0, 200.0, -300.0), motion, 0.5, boss).is_none());
    }

    // -----------------------------------------------------------------
    // Hull mechanics.
    // -----------------------------------------------------------------

    #[test]
    fn hull_bounds_contain_every_part_and_the_sweep_agrees_with_them() {
        let boss = v(10.0, -4.0, 600.0);
        let bounds = boss_hull_bounds(boss);
        for b in boss_hull_boxes(boss) {
            assert!(bounds.contains_point(b.min()));
            assert!(bounds.contains_point(b.max()));
        }
        // The union really is the model's envelope, yawed.
        assert_eq!(bounds.min(), boss + v(-134.0, -15.0, -180.0));
        assert_eq!(bounds.max(), boss + v(134.0, 64.0, 186.0));
    }

    #[test]
    fn a_hit_is_attributed_to_the_nearest_damage_zone() {
        let boss = Rules::DEFAULT.campaign.boss_base_pos;
        // Straight at the sphere that sits at offset (85, 0, 150), index 18.
        let hit = sweep_boss_hull(boss + v(85.0, 0.0, 400.0), v(0.0, 0.0, -600.0), 0.5, boss)
            .expect("hit");
        assert_eq!(hit.zone, 18);
        assert_eq!(hit.zone_id, BOSS_ID_BASE + 18);

        // Exact zone centres map to themselves.
        for i in 0..BOSS_HITBOX_COUNT {
            assert_eq!(nearest_zone(boss_zone_center(i, boss), boss), i);
        }
        // Out-of-range indices clamp instead of panicking.
        assert_eq!(
            boss_zone_center(999, boss),
            boss_zone_center(BOSS_HITBOX_COUNT - 1, boss)
        );
    }

    #[test]
    fn the_earliest_contact_wins_across_the_four_boxes() {
        let boss = Rules::DEFAULT.campaign.boss_base_pos;
        // Fired from above and behind, this segment crosses the bridge tower
        // before it reaches the deck.
        let origin = boss + v(0.0, 55.0, -300.0);
        let motion = v(0.0, -50.0, 400.0);
        let hit = sweep_boss_hull(origin, motion, 0.5, boss).expect("hit");
        assert_eq!(hit.part, HullPart::Bridge);
        for b in boss_hull_boxes(boss) {
            if let Some(t) = swept_sphere_aabb(origin, motion, 0.5, b) {
                assert!(hit.t <= t, "an earlier contact was missed");
            }
        }
    }

    #[test]
    fn the_hull_travels_with_the_capital_ship() {
        // A ray that hits at rest must miss once the hull has drifted away, and
        // the drift is large: 88 units of lateral patrol against a 268-wide ship.
        let rules = Rules::DEFAULT;
        let a = boss_patrol_pos(&rules.campaign, 0.0);
        let origin = a + v(0.0, 0.0, -400.0);
        let motion = v(0.0, 0.0, 800.0);
        assert!(sweep_boss_hull(origin, motion, 0.5, a).is_some());

        let b = boss_patrol_pos(&rules.campaign, 20.0);
        // Twenty seconds in the drift is most of the way to starboard.
        assert!(b.x > 80.0, "the patrol should have moved by now: {}", b.x);
        // Same centreline ray: still hits, because the ship is 268 wide.
        assert!(sweep_boss_hull(origin, motion, 0.5, b).is_some());
        // A ray grazing the port wingtip at rest is left behind entirely.
        let edge = a + v(-133.0, 0.0, -400.0);
        assert!(sweep_boss_hull(edge, motion, 0.5, a).is_some());
        assert!(sweep_boss_hull(edge, motion, 0.5, b).is_none());
    }

    #[test]
    fn non_finite_geometry_never_reports_a_hit() {
        let boss = Rules::DEFAULT.campaign.boss_base_pos;
        let nan = v(f64::NAN, 0.0, 0.0);
        assert!(sweep_boss_hull(nan, v(0.0, 0.0, 1.0), 0.5, boss).is_none());
        assert!(sweep_boss_hull(boss, nan, 0.5, boss).is_none());
        assert!(sweep_boss_hull(v(0.0, 0.0, 0.0), v(0.0, 0.0, 1.0), f64::NAN, boss).is_none());
    }

    // -----------------------------------------------------------------
    // Patrol.
    // -----------------------------------------------------------------

    #[test]
    fn the_patrol_stays_within_its_amplitudes_and_never_moves_in_z() {
        let c = Rules::DEFAULT.campaign;
        assert_eq!(boss_patrol_pos(&c, 0.0), c.boss_base_pos);
        for i in 0..4000 {
            let t = f64::from(i) * 0.25;
            let p = boss_patrol_pos(&c, t);
            assert!((p.x - c.boss_base_pos.x).abs() <= c.boss_drift_x_amp + 1e-9);
            assert!((p.y - c.boss_base_pos.y).abs() <= c.boss_drift_y_amp + 1e-9);
            assert_eq!(p.z, c.boss_base_pos.z);
        }
        // Repeatable to the bit, which is what the isolation of `boss_sin` buys.
        let a = boss_patrol_pos(&c, 17.5);
        let b = boss_patrol_pos(&c, 17.5);
        assert_eq!(a.x.to_bits(), b.x.to_bits());
    }

    // -----------------------------------------------------------------
    // Turrets.
    // -----------------------------------------------------------------

    #[test]
    fn a_turret_muzzle_sits_on_the_line_to_its_target() {
        let c = Rules::DEFAULT.campaign;
        let boss = c.boss_base_pos;
        let target = v(40.0, 60.0, 300.0);
        let aim = solve_turret(BOSS_TURRET_PIVOTS[0], boss, target, &c);

        // Pivot 0 is local (-80, 22, 110); the 180 deg yaw puts it at (80, 22, -110).
        assert_eq!(aim.pivot, boss + v(80.0, 22.0, -110.0));
        // 22 forward along the aim, 1.8 perpendicular: the offset from the pivot
        // has exactly that length, and its projection on the aim is 22.
        let off = aim.muzzle - aim.pivot;
        assert!((off.dot(aim.aim) - BOSS_TURRET_MUZZLE.z).abs() < 1e-9);
        assert!(
            (off.length() - (BOSS_TURRET_MUZZLE.z.powi(2) + BOSS_TURRET_MUZZLE.y.powi(2)).sqrt())
                .abs()
                < 1e-9
        );
        // And the muzzle is nearer the target than the pivot was.
        assert!(aim.muzzle.distance(target) < aim.pivot.distance(target));
    }

    #[test]
    fn the_four_turret_pivots_sit_at_the_corners_of_the_deck() {
        let boss = Vec3::ZERO;
        let c = Rules::DEFAULT.campaign;
        let mut seen = Vec::new();
        for p in BOSS_TURRET_PIVOTS {
            let a = solve_turret(p, boss, v(0.0, 0.0, 1000.0), &c);
            seen.push((a.pivot.x, a.pivot.z));
            // Every pivot is inside the hull's plan view.
            assert!(a.pivot.x.abs() <= 134.0 && a.pivot.z.abs() <= 186.0);
        }
        seen.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(
            seen,
            vec![
                (-80.0, -110.0),
                (-80.0, 110.0),
                (80.0, -110.0),
                (80.0, 110.0)
            ]
        );
    }

    #[test]
    fn turret_pitch_is_clamped_but_the_gun_still_shoots_where_it_likes() {
        let c = Rules::DEFAULT.campaign;
        let boss = c.boss_base_pos;
        // Target directly overhead: the true elevation is 90 deg, far past the
        // 0.7 rad limit.
        let target = boss + v(0.0, 500.0, 0.0);
        let aim = solve_turret(BOSS_TURRET_PIVOTS[0], boss, target, &c);
        assert!(aim.pitch.abs() <= c.turret_pitch_limit + 1e-12);
        assert!(
            (aim.pitch.abs() - c.turret_pitch_limit).abs() < 1e-12,
            "clamped"
        );
        // The *aim* is not clamped — this is the finding that the clamp is
        // cosmetic. The muzzle is genuinely above the pivot.
        assert!(aim.aim.y > 0.9);
        assert!(aim.muzzle.y > aim.pivot.y + 20.0);
    }

    #[test]
    fn a_degenerate_target_produces_no_nan() {
        let c = Rules::DEFAULT.campaign;
        let boss = c.boss_base_pos;
        // Target exactly on the pivot.
        let pivot = boss + capital_local_to_world(BOSS_TURRET_PIVOTS[1]);
        let aim = solve_turret(BOSS_TURRET_PIVOTS[1], boss, pivot, &c);
        assert!(aim.aim.is_finite() && aim.muzzle.is_finite());
        assert!(aim.yaw.is_finite() && aim.pitch.is_finite());
    }

    #[test]
    fn reload_shortens_in_three_bands_as_the_boss_is_hurt() {
        let c = Rules::DEFAULT.campaign;
        assert_eq!(turret_reload_window(&c, 1.0), c.turret_reload_healthy);
        assert_eq!(turret_reload_window(&c, 0.66), c.turret_reload_healthy);
        assert_eq!(turret_reload_window(&c, 0.65), c.turret_reload_wounded);
        assert_eq!(turret_reload_window(&c, 0.36), c.turret_reload_wounded);
        assert_eq!(turret_reload_window(&c, 0.35), c.turret_reload_critical);
        assert_eq!(turret_reload_window(&c, 0.0), c.turret_reload_critical);
        // Monotone: a hurt capital ship is never slower than a healthy one.
        assert!(c.turret_reload_critical[0] < c.turret_reload_wounded[0]);
        assert!(c.turret_reload_wounded[0] < c.turret_reload_healthy[0]);
    }

    #[test]
    fn turrets_open_on_a_stagger_rather_than_a_volley() {
        let w = world_at(1);
        let camp = w.campaign.as_ref().unwrap();
        let c = w.rules.campaign;
        for (i, t) in camp.turrets.iter().enumerate() {
            assert_eq!(
                t.fire_timer,
                i as f64 * c.turret_stagger + c.turret_stagger_offset
            );
            assert_eq!(t.local_pos, BOSS_TURRET_PIVOTS[i]);
        }
        // No two guns are ready on the same tick.
        let mut times: Vec<f64> = camp.turrets.iter().map(|t| t.fire_timer).collect();
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for pair in times.windows(2) {
            assert!(pair[1] - pair[0] >= c.turret_stagger - 1e-12);
        }
    }

    #[test]
    fn turrets_put_their_rounds_in_the_shared_bullet_list() {
        let mut w = world_at(1);
        let mut ev = Vec::new();
        // Jump straight to the fight and park the player in front of the ship.
        activate_boss(&mut w, &mut ev);
        let target = w.rules.campaign.boss_base_pos + v(0.0, 0.0, -220.0);
        w.ship_mut(1).unwrap().pos = target;

        for _ in 0..600 {
            update(&mut w, TICK_DT, &mut ev);
        }
        assert!(
            !w.bullets.is_empty(),
            "ten seconds should be four guns' worth of fire"
        );
        for b in &w.bullets {
            assert_eq!(b.owner, BOSS_ID_BASE);
            assert_eq!(b.owner_team, Some(Team::One));
            assert_eq!(b.damage, w.rules.weapons.boss_bullet_damage);
            assert!((b.vel.length() - w.rules.weapons.boss_bullet_speed).abs() < 1e-9);
            assert_eq!(b.life, w.rules.weapons.boss_bullet_life);
            // Aimed within the spread cone at the player.
            let to_target = (target - b.pos).normalize();
            assert!(b.vel.normalize().dot(to_target) > 0.98);
        }
        // Keys are unique, so the renderer can cache meshes against them.
        let mut keys: Vec<u64> = w.bullets.iter().map(|b| b.key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), w.bullets.len());
        assert!(ev
            .iter()
            .any(|e| matches!(e, SimEvent::Fired { owner, .. } if *owner == BOSS_ID_BASE)));
    }

    #[test]
    fn turrets_hold_their_fire_while_the_player_is_dead() {
        let mut w = world_at(1);
        let mut ev = Vec::new();
        activate_boss(&mut w, &mut ev);
        w.ship_mut(1).unwrap().pos = w.rules.campaign.boss_base_pos + v(0.0, 0.0, -220.0);
        w.ship_mut(1).unwrap().alive = false;

        let before: Vec<f64> = w
            .campaign
            .as_ref()
            .unwrap()
            .turrets
            .iter()
            .map(|t| t.fire_timer)
            .collect();
        for _ in 0..600 {
            update(&mut w, TICK_DT, &mut ev);
        }
        assert!(w.bullets.is_empty(), "a dead player is not shot at");
        let after: Vec<f64> = w
            .campaign
            .as_ref()
            .unwrap()
            .turrets
            .iter()
            .map(|t| t.fire_timer)
            .collect();
        assert_eq!(before, after, "reload must not tick down while dead");
    }

    // -----------------------------------------------------------------
    // Mission state machine.
    // -----------------------------------------------------------------

    #[test]
    fn init_builds_the_boss_hitboxes_and_the_first_wave() {
        let w = world_at(1);
        let camp = w.campaign.as_ref().unwrap();
        assert_eq!(camp.mission, 1);
        assert_eq!(camp.phase, CampaignPhase::Wave);
        assert_eq!(camp.wave_index, 0);
        assert_eq!(camp.lives, 3);
        assert_eq!(camp.boss_hp, 2500);
        assert!(!camp.boss_active);
        assert_eq!(camp.checkpoint_pos, CHECKPOINT_START);

        // 20 hitboxes at 9000..9019, dead until the boss engages.
        let hitboxes: Vec<&Ship> = w.ships.iter().filter(|s| is_boss_hitbox(s.id)).collect();
        assert_eq!(hitboxes.len(), BOSS_HITBOX_COUNT);
        for (i, s) in hitboxes.iter().enumerate() {
            assert_eq!(s.id, BOSS_ID_BASE + i as EntityId);
            assert_eq!(s.kind, ShipKind::BossHitbox);
            assert!(!s.alive);
            assert_eq!(s.team, Some(Team::One));
            assert_eq!(s.hit_radius_override, Some(28.0));
            assert_eq!(
                s.pos,
                w.rules.campaign.boss_base_pos + BOSS_HITBOX_OFFSETS[i]
            );
        }

        // Wave 1 of mission 1 is three scouts, ids from 100.
        assert_eq!(camp.wave_bot_ids.len(), CAMPAIGN_WAVES_M1[0].count as usize);
        assert_eq!(camp.wave_bot_ids, vec![100, 101, 102]);
        assert_eq!(camp.next_bot_id, 103);
        for id in &camp.wave_bot_ids {
            let s = w.ship(*id).expect("wave bot");
            assert!(s.bot.is_campaign_bot);
            assert_eq!(s.team, Some(Team::One));
            // Inside the jitter box around the wave anchor.
            let d = s.pos - v(0.0, 20.0, f64::from(CAMPAIGN_WAVES_M1[0].spawn_z));
            assert!(d.x.abs() <= 80.0 && d.y.abs() <= 30.0 && d.z.abs() <= 65.0);
        }
        assert!(!boss_is_engageable(&w));
    }

    #[test]
    fn every_mission_runs_three_waves_and_then_the_boss() {
        for mission in 1..=3u8 {
            let mut w = world_at(mission);
            let mut ev = Vec::new();
            let waves = campaign_waves(mission);

            for (expected, wave) in waves.iter().enumerate() {
                let camp = w.campaign.as_ref().unwrap();
                assert_eq!(camp.wave_index, expected);
                assert_eq!(camp.phase, CampaignPhase::Wave);
                assert_eq!(camp.wave_bot_ids.len(), wave.count as usize);

                // Clear the wave.
                let ids = camp.wave_bot_ids.clone();
                for id in ids {
                    w.ship_mut(id).unwrap().alive = false;
                }
                update(&mut w, TICK_DT, &mut ev);
                assert!(ev
                    .iter()
                    .any(|e| matches!(e, SimEvent::WaveComplete { index } if *index == expected)));

                // Sit out the pause.
                let gap = if expected + 1 < WAVES_PER_MISSION {
                    w.rules.campaign.wave_gap
                } else {
                    w.rules.campaign.boss_gap
                };
                let steps = (gap / TICK_DT).ceil() as i32 + 2;
                for _ in 0..steps {
                    update(&mut w, TICK_DT, &mut ev);
                }
            }

            let camp = w.campaign.as_ref().unwrap();
            assert_eq!(camp.phase, CampaignPhase::Boss);
            assert!(camp.boss_active);
            assert_eq!(camp.boss_hp, 2500);
            assert!(ev.iter().any(|e| matches!(e, SimEvent::BossPhaseStarted)));
            for s in w.ships.iter().filter(|s| is_boss_hitbox(s.id)) {
                assert!(s.alive);
            }
        }
    }

    #[test]
    fn the_checkpoint_walks_forward_as_waves_clear() {
        let mut w = world_at(1);
        let mut ev = Vec::new();
        let waves = campaign_waves(1);
        let expected = [
            CHECKPOINT_START,
            v(0.0, 20.0, f64::from(waves[1].spawn_z) - 80.0),
            v(0.0, 20.0, f64::from(waves[2].spawn_z) - 80.0),
            CHECKPOINT_BOSS,
        ];
        assert_eq!(w.campaign.as_ref().unwrap().checkpoint_pos, expected[0]);

        for step in 0..WAVES_PER_MISSION {
            let ids = w.campaign.as_ref().unwrap().wave_bot_ids.clone();
            for id in ids {
                w.ship_mut(id).unwrap().alive = false;
            }
            update(&mut w, TICK_DT, &mut ev);
            assert_eq!(
                w.campaign.as_ref().unwrap().checkpoint_pos,
                expected[step + 1],
                "checkpoint after wave {step}"
            );
            let gap = if step + 1 < WAVES_PER_MISSION {
                w.rules.campaign.wave_gap
            } else {
                w.rules.campaign.boss_gap
            };
            for _ in 0..((gap / TICK_DT).ceil() as i32 + 2) {
                update(&mut w, TICK_DT, &mut ev);
            }
        }
        // The boss checkpoint is short of the capital ship, facing it.
        assert!(CHECKPOINT_BOSS.z < w.rules.campaign.boss_base_pos.z);
    }

    #[test]
    fn a_cleared_wave_only_completes_once() {
        let mut w = world_at(1);
        let mut ev = Vec::new();
        let ids = w.campaign.as_ref().unwrap().wave_bot_ids.clone();
        for id in ids {
            w.ship_mut(id).unwrap().alive = false;
        }
        for _ in 0..5 {
            update(&mut w, TICK_DT, &mut ev);
        }
        let completes = ev
            .iter()
            .filter(|e| matches!(e, SimEvent::WaveComplete { .. }))
            .count();
        assert_eq!(completes, 1);
    }

    #[test]
    fn the_pause_between_waves_freezes_the_warp_timer() {
        // `updateCampaign` returns before touching the warp timer while a pause
        // runs (`main.js:2853` precedes `:2887`). Observable: the warp flash
        // lasts longer if you die into a wave transition.
        let mut w = world_at(1);
        let mut ev = Vec::new();
        let ids = w.campaign.as_ref().unwrap().wave_bot_ids.clone();
        for id in ids {
            w.ship_mut(id).unwrap().alive = false;
        }
        update(&mut w, TICK_DT, &mut ev);
        assert!(w.campaign.as_ref().unwrap().between);

        w.campaign.as_mut().unwrap().warp_timer = 1.5;
        for _ in 0..30 {
            update(&mut w, TICK_DT, &mut ev);
        }
        assert_eq!(w.campaign.as_ref().unwrap().warp_timer, 1.5);
    }

    // -----------------------------------------------------------------
    // Lives and respawn.
    // -----------------------------------------------------------------

    #[test]
    fn three_lives_then_the_mission_fails() {
        let mut w = world_at(1);
        let mut ev = Vec::new();
        assert_eq!(on_player_death(&mut w, &mut ev), 2);
        assert_eq!(w.campaign.as_ref().unwrap().phase, CampaignPhase::Wave);
        assert_eq!(
            w.ship(1).unwrap().respawn_timer,
            w.rules.combat.campaign_respawn_delay
        );
        assert_eq!(
            w.campaign.as_ref().unwrap().warp_timer,
            w.rules.campaign.warp_duration
        );

        assert_eq!(on_player_death(&mut w, &mut ev), 1);
        assert_eq!(on_player_death(&mut w, &mut ev), 0);

        let camp = w.campaign.as_ref().unwrap();
        assert_eq!(camp.phase, CampaignPhase::Failed);
        assert_eq!(camp.lives, 0);
        assert_eq!(camp.warp_timer, 0.0);
        // Nothing to come back as.
        assert_eq!(w.ship(1).unwrap().respawn_timer, 0.0);
        assert!(respawn_pose(&w).is_none());
        assert_eq!(
            ev.iter()
                .filter(|e| matches!(e, SimEvent::CampaignFailed))
                .count(),
            1
        );

        // Dying again after failure changes nothing.
        assert_eq!(on_player_death(&mut w, &mut ev), 0);
        assert_eq!(
            ev.iter()
                .filter(|e| matches!(e, SimEvent::CampaignFailed))
                .count(),
            1
        );
    }

    #[test]
    fn a_campaign_respawn_comes_back_hurt_at_the_checkpoint() {
        let mut w = world_at(1);
        let mut ev = Vec::new();
        on_player_death(&mut w, &mut ev);

        let (pos, quat, hp) = respawn_pose(&w).expect("still have lives");
        assert_eq!(pos, CHECKPOINT_START);
        assert_eq!(quat, Quat::IDENTITY);
        // floor(100 * 0.55) — a life costs 45 hit points, not just a pause.
        assert_eq!(hp, 55);
        assert!(hp < w.rules.ship.max_hp);
        assert_eq!(respawn_hp(&Rules::DEFAULT), 55);

        // The pose follows the checkpoint.
        w.campaign.as_mut().unwrap().checkpoint_pos = CHECKPOINT_BOSS;
        assert_eq!(respawn_pose(&w).unwrap().0, CHECKPOINT_BOSS);
    }

    #[test]
    fn the_warp_timer_runs_down_over_its_documented_duration() {
        let mut w = world_at(1);
        let mut ev = Vec::new();
        on_player_death(&mut w, &mut ev);
        let steps = (w.rules.campaign.warp_duration / TICK_DT).ceil() as i32;
        for _ in 0..steps - 1 {
            update(&mut w, TICK_DT, &mut ev);
        }
        assert!(w.campaign.as_ref().unwrap().warp_timer > 0.0);
        for _ in 0..3 {
            update(&mut w, TICK_DT, &mut ev);
        }
        assert_eq!(w.campaign.as_ref().unwrap().warp_timer, 0.0);
    }

    // -----------------------------------------------------------------
    // Boss damage.
    // -----------------------------------------------------------------

    #[test]
    fn the_boss_cannot_be_hurt_before_it_engages() {
        let mut w = world_at(1);
        let mut ev = Vec::new();
        assert!(!apply_boss_damage(&mut w, 500, Some(1), &mut ev));
        assert_eq!(w.campaign.as_ref().unwrap().boss_hp, 2500);
        assert!(resolve_weapon_hit(
            &w,
            w.rules.campaign.boss_base_pos + v(0.0, 0.0, -400.0),
            v(0.0, 0.0, 800.0),
            0.5
        )
        .is_none());
    }

    #[test]
    fn boss_damage_pools_across_every_hitbox_and_ends_in_victory() {
        let mut w = world_at(1);
        let mut ev = Vec::new();
        activate_boss(&mut w, &mut ev);
        // Clear spawn protection.
        for s in w.ships.iter_mut().filter(|s| is_boss_hitbox(s.id)) {
            s.invuln_timer = 0.0;
        }
        assert!(boss_is_engageable(&w));

        assert!(apply_boss_damage(&mut w, 50, Some(1), &mut ev));
        assert_eq!(w.campaign.as_ref().unwrap().boss_hp, 2450);
        // Every hitbox mirrors the pool, not just the centre one.
        for s in w.ships.iter().filter(|s| is_boss_hitbox(s.id)) {
            assert_eq!(s.hp, 2450);
        }
        assert!(!apply_boss_damage(&mut w, 0, None, &mut ev));

        assert!(apply_boss_damage(&mut w, 5000, Some(1), &mut ev));
        let camp = w.campaign.as_ref().unwrap();
        assert_eq!(camp.boss_hp, 0);
        assert_eq!(camp.phase, CampaignPhase::Victory);
        assert!(!camp.boss_active);
        assert_eq!(camp.lives, 3);
        for s in w.ships.iter().filter(|s| is_boss_hitbox(s.id)) {
            assert!(!s.alive);
        }
        assert!(ev
            .iter()
            .any(|e| matches!(e, SimEvent::CampaignVictory { lives_left } if *lives_left == 3)));
        // Dead is dead.
        assert!(!apply_boss_damage(&mut w, 10, Some(1), &mut ev));
        assert!(!boss_is_engageable(&w));
    }

    #[test]
    fn spawn_protection_covers_the_boss_like_every_other_ship() {
        let mut w = world_at(1);
        let mut ev = Vec::new();
        activate_boss(&mut w, &mut ev);
        assert!(!boss_is_engageable(&w), "invuln window is live");
        assert!(!apply_boss_damage(&mut w, 100, Some(1), &mut ev));
        for s in w.ships.iter_mut().filter(|s| is_boss_hitbox(s.id)) {
            s.invuln_timer = 0.0;
        }
        assert!(apply_boss_damage(&mut w, 100, Some(1), &mut ev));
    }

    #[test]
    fn a_weapon_sweep_reaches_the_boss_once_it_is_engaged() {
        let mut w = world_at(1);
        let mut ev = Vec::new();
        activate_boss(&mut w, &mut ev);
        for s in w.ships.iter_mut().filter(|s| is_boss_hitbox(s.id)) {
            s.invuln_timer = 0.0;
        }
        let boss = w.campaign.as_ref().unwrap().boss_pos;
        // The very shot the sphere cluster used to drop.
        let hit = resolve_weapon_hit(&w, boss + v(56.5, 0.0, -400.0), v(0.0, 0.0, 800.0), 0.5)
            .expect("must connect");
        assert_eq!(hit.part, HullPart::Hull);
        assert!(is_boss_hitbox(hit.zone_id));
    }

    // -----------------------------------------------------------------
    // Readouts and determinism.
    // -----------------------------------------------------------------

    #[test]
    fn the_hud_and_boss_view_track_the_fight() {
        let mut w = world_at(2);
        let mut ev = Vec::new();
        let hud = campaign_hud(&w);
        assert!(hud.active && !hud.boss_active);
        assert_eq!(hud.mission, 2);
        assert_eq!(hud.lives, 3);
        assert_eq!(hud.enemies_left, campaign_waves(2)[0].count);

        activate_boss(&mut w, &mut ev);
        for s in w.ships.iter_mut().filter(|s| is_boss_hitbox(s.id)) {
            s.invuln_timer = 0.0;
        }
        apply_boss_damage(&mut w, 1250, Some(1), &mut ev);
        let hud = campaign_hud(&w);
        assert!(hud.boss_active);
        assert!((hud.boss_hp01 - 0.5).abs() < 1e-6);

        let view = boss_view(&w).expect("campaign");
        assert_eq!(view.hp, 1250);
        assert_eq!(view.max_hp, 2500);
        assert_eq!(view.pos[2], w.rules.campaign.boss_base_pos.z as f32);

        // Outside the campaign both readouts are inert.
        let plain = World::new(1, Rules::DEFAULT, Mode::Skirmish, MapKind::Space);
        assert_eq!(campaign_hud(&plain), CampaignHud::default());
        assert!(boss_view(&plain).is_none());
    }

    #[test]
    fn two_identical_missions_produce_identical_state() {
        let run = || {
            let mut w = world_at(3);
            let mut ev = Vec::new();
            activate_boss(&mut w, &mut ev);
            w.ship_mut(1).unwrap().pos = w.rules.campaign.boss_base_pos + v(30.0, 12.0, -260.0);
            for _ in 0..900 {
                update(&mut w, TICK_DT, &mut ev);
            }
            w
        };
        let a = run();
        let b = run();
        assert_eq!(a.campaign, b.campaign);
        assert_eq!(a.bullets, b.bullets);
        assert_eq!(a.ships, b.ships);
        // And the patrol really did move, so the comparison is not vacuous.
        assert_ne!(
            a.campaign.as_ref().unwrap().boss_pos,
            Rules::DEFAULT.campaign.boss_base_pos
        );
        assert!(!a.bullets.is_empty());
    }

    #[test]
    fn init_is_inert_outside_the_campaign() {
        let mut w = World::new(1, Rules::DEFAULT, Mode::Skirmish, MapKind::Space);
        init(&mut w, false);
        assert!(w.campaign.is_none());
        assert!(w.ships.is_empty());
        let mut ev = Vec::new();
        update(&mut w, TICK_DT, &mut ev);
        assert!(ev.is_empty());
        assert_eq!(on_player_death(&mut w, &mut ev), 0);
    }

    #[test]
    fn an_out_of_range_wave_index_is_ignored() {
        let mut w = world_at(1);
        let before = w.ships.len();
        spawn_wave(&mut w, 99);
        assert_eq!(w.ships.len(), before);
        assert_eq!(w.campaign.as_ref().unwrap().wave_index, 0);
    }
}
