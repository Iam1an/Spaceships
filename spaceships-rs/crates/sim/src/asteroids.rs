//! The asteroid field: seeded generation, damage, destruction, and spin.
//!
//! # Why this module is the determinism hot spot
//!
//! Everything else in the simulation can be re-derived from state that is
//! already on the wire. The asteroid field cannot: it is *generated*, and the
//! whole point of generating it is that the server sends a single `u64` seed
//! instead of streaming sixty asteroid records (`server/index.js:767` puts the
//! entire `room.asteroids` array inside the `start` message today). Both sides
//! then rebuild the field locally.
//!
//! That trade is only sound if the rebuild is *byte-identical*. One float of
//! divergence and a rock sits somewhere else on one machine than on another —
//! so players shoot at rocks that are not there, bullets vanish into empty
//! space, and a ship takes 15–29 collision damage from nothing. There is no
//! reconciliation message for "your rock is in the wrong place"; the field is
//! generated once and never corrected.
//!
//! Concretely, this module holds to three rules:
//!
//! - **One draw order, written once.** Every rock consumes its random numbers in
//!   the order tier → size → position → rotation → variant → spin, in both field
//!   shapes. Reordering, adding, or removing a draw regenerates every field from
//!   every seed. `generation_sequence_is_pinned` fails loudly if that happens.
//! - **No transcendental functions.** Positions are built from `+ - * /` and
//!   `sqrt` only, all of which IEEE-754 requires to be correctly rounded, hence
//!   bit-identical on x86-64, aarch64, and wasm32. There is no `sin`, `cos`, or
//!   `powf` anywhere in generation — notably the direction draw is a normalized
//!   cube sample, not a spherical-coordinate one, which is what the JS does and
//!   is also the only version that is portable.
//! - **Fixed iteration order.** Rocks are generated in index order into a `Vec`
//!   and stay there.
//!
//! # One generator, replacing three
//!
//! | JS generator | Site | Shape |
//! |---|---|---|
//! | Client solo / trials | `asteroids.js:110` (`createAsteroidField`) | sphere around the origin |
//! | Multiplayer | `server/index.js:544` (`generateAsteroidField`) | sphere around the origin |
//! | Campaign | `main.js:237` (`genCampaignAsteroids`) | three boxed slabs along the flight path |
//!
//! [`crate::rules`] already unified their constants. What is resolved *here* is
//! the behaviour the three disagreed on:
//!
//! - **Draw order.** The client draws tier → variant → size → position → …; the
//!   server draws tier → size → position → rotation → variant → spin. This
//!   module uses the **server's** order, to match the id convention
//!   ([`FIRST_ASTEROID_ID`]) that was also taken from the server.
//! - **Moon avoidance.** See [`avoid_volumes`] — this is the live bug.
//! - **The fallback placement.** See `resolve_fallback` — this is the other one.
//! - **Spin scaling.** Applied exactly once, from
//!   [`crate::rules::AsteroidTierSpec::spin_scale`]. The campaign generator
//!   applied it twice.
//! - **Initial rotation.** The client draws `[0, π)` per axis
//!   (`asteroids.js:171`); the campaign draws `[0, 2π)` on x and y and hard-zero
//!   on z (`main.js:264`). Neither is preserved: both fields now draw
//!   `[0, `[`ROTATION_RANGE`]`)` on all three axes, which is the only one of the
//!   three that is actually a uniform orientation draw. Rotation is cosmetic
//!   (collision reads [`crate::world::Asteroid::radius`], so a rock is a sphere
//!   however it is drawn), but it must still agree across clients, which is why
//!   it is generated here rather than in the renderer.
//!
//! # What is deliberately absent
//!
//! No meshes, no materials, no `IcosahedronGeometry`, no vertex displacement.
//! [`crate::world::Asteroid::variant`] is an *index* — which of the
//! [`crate::rules::AsteroidFieldRules::variant_count`] deformed icosahedra to
//! draw — and it is simulation state purely because every client must pick the
//! same one. The geometry stays in `asteroids.js`.
//!
//! No audio. `main.js:2189` plays `rockbreak` when a rock dies; this module
//! emits [`SimEvent::Explosion`] with [`ExplosionKind::AsteroidBreak`] and lets
//! the caller decide that a sound is a thing that exists.

use crate::collision::{sphere_overlaps_aabb, Aabb, Sphere};
use crate::math::Vec3;
use crate::rng::Rng;
use crate::rules::{
    asteroid_tier_weights, AsteroidFieldRules, AsteroidTierSpec, AsteroidZone, Rules,
    ASTEROID_TIERS,
};
use crate::world::{Asteroid, AsteroidTier, ExplosionKind, MapKind, Mode, SimEvent, World};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// First asteroid id, for every mode.
///
/// **Zero-based, and id `0` is a real asteroid.** The JS disagrees with itself:
/// `server/index.js:565` allocates `id: i` from 0, while both client generators
/// count from 1 (`asteroids.js:184` uses `list.length + 1`, `main.js:239` starts
/// `let id = 1`). [`crate::world::Asteroid::id`] settled on the server's.
///
/// The consequence is worth stating plainly, because it is the classic way this
/// breaks: **`0` must never be truthiness-tested.** `if (id)` and `if (!id)` are
/// wrong, `Option<NonZeroU32>` is wrong, and a sentinel of `0` for "no asteroid"
/// is wrong. The JS gets this right at its one call site (`main.js:2180` tests
/// `id === undefined || id === null`), which is the correct shape. In Rust the
/// type system carries it: absence is `None`, and `0` is a `u32` like any other.
/// `id_zero_is_an_ordinary_asteroid` pins it.
pub const FIRST_ASTEROID_ID: u32 = 0;

/// Rate at which an asteroid's damage flash fades, per second.
///
/// `asteroids.js:101` and `:192`, both `hitFlash - dt * 4`. Cosmetic, but it is
/// *shared* cosmetics — two clients must agree on how long a rock glows — so it
/// lives in the simulation rather than the renderer.
///
// TODO(rules): this belongs in `AsteroidFieldRules` beside the other field
// constants. It is a free-standing `const` only because `rules.rs` has no field
// for it and is read-only to this module.
pub const HIT_FLASH_DECAY_RATE: f64 = 4.0;

/// Upper bound of the initial rotation draw, per axis, in radians.
///
/// A full turn. See the module docs: neither JS convention (`[0, π)` on the
/// client, `[0, 2π)` on two axes and zero on the third in the campaign) is a
/// uniform orientation, and this is.
///
// TODO(rules): also belongs in `AsteroidFieldRules`.
pub const ROTATION_RANGE: f64 = core::f64::consts::TAU;

/// Slack added when pushing a rock out of an avoidance volume, in world units.
///
/// [`sphere_overlaps_aabb`] counts tangency as overlap, so a rock placed exactly
/// on a boundary still clips. This is the margin that makes the push land
/// strictly outside. One micron against a moon of radius 80 — eight orders of
/// magnitude below anything the simulation or the renderer can distinguish, and
/// a fixed constant rather than a relative tolerance so it is exactly
/// reproducible.
const PLACEMENT_EPSILON: f64 = 1.0e-6;

/// Push-out rounds in `resolve_fallback` before it gives up on axis pushes and
/// escapes radially.
///
/// A push can only be undone by a *different* volume, and the shipped geometry
/// is now a single moon at the origin, so one round always suffices. The budget
/// exists for hypothetical rule sets whose avoidance volumes overlap each
/// other — as it did when the two motherships were also in the list.
const PUSH_ROUNDS: u32 = 8;

/// Radial-escape doublings in `resolve_fallback`, the terminator of last resort.
///
/// Avoidance volumes are bounded, so doubling the distance from the field centre
/// clears all of them in a handful of steps. Eight doublings is a factor of 256
/// on a 400-unit field.
const ESCAPE_DOUBLINGS: u32 = 8;

// ---------------------------------------------------------------------------
// Damage results
// ---------------------------------------------------------------------------

/// What one weapon impact did to an asteroid.
///
/// Mirrors the three-way branch that `damageAsteroidLocal` (`main.js:2179`) and
/// the server's `asteroid-hit` handler (`server/index.js:808`) both open-code.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DamageOutcome {
    /// Nothing happened: there is no asteroid with that id, or it was already at
    /// zero hit points. Both JS sites guard with the same
    /// `if (!a || a.hp <= 0) return;`, and both halves are reachable — a hit
    /// report can arrive a tick after the rock that would have absorbed it was
    /// destroyed.
    Ignored,
    /// The asteroid survived.
    Damaged {
        /// Hit points remaining, always at least 1.
        hp: i32,
    },
    /// The asteroid was destroyed and removed from [`World::asteroids`].
    Destroyed {
        /// Where it died, for the debris burst.
        pos: Vec3,
        /// Its collision radius, which sizes that burst.
        radius: f64,
    },
}

impl DamageOutcome {
    /// Whether this impact removed the asteroid from the world.
    #[must_use]
    pub fn destroyed(self) -> bool {
        matches!(self, DamageOutcome::Destroyed { .. })
    }

    /// Whether this impact changed anything at all.
    #[must_use]
    pub fn landed(self) -> bool {
        !matches!(self, DamageOutcome::Ignored)
    }
}

// ---------------------------------------------------------------------------
// Tier lookup
// ---------------------------------------------------------------------------

/// The size/HP/frequency row for `tier`.
#[must_use]
pub fn tier_spec(tier: AsteroidTier) -> &'static AsteroidTierSpec {
    &ASTEROID_TIERS[tier.index()]
}

/// Starting hit points for `tier`. `asteroids.js:15`–`:18`.
#[must_use]
pub fn tier_hp(tier: AsteroidTier) -> i32 {
    tier_spec(tier).hp
}

// ---------------------------------------------------------------------------
// Avoidance volumes
// ---------------------------------------------------------------------------

/// The volumes a generated asteroid must not clip, for `map`.
///
/// # The bug this fixes
///
/// The client builds `_avoidList` from the two motherships **and the moon**
/// (`main.js:236`, using the `moonAvoid` box from `main.js:185`), and
/// `clipsAvoidance` (`asteroids.js:114`) rejects any placement inside it. The
/// server's `clipsMothership` (`server/index.js:532`) checks the motherships and
/// nothing else — there is no `MOON_AVOID` in `server/index.js` at all.
///
/// Meanwhile `generateAsteroidField` places rocks between `30 + size` and 400
/// units *from the origin* (`server/index.js:556`), and the moon is a sphere of
/// radius 80 at the origin (`main.js:181`). So in every multiplayer space match,
/// the rocks drawn between 30 and 80 units out are generated **inside the
/// moon**, where they are invisible and effectively immortal: bullets are
/// consumed by the buried rock before the moon obstacle test runs
/// (`bullets.js:96` precedes `:122`), missiles detonate on entry
/// (`missiles.js:355`), and a player who clips one eats both the 15–29 collision
/// damage (`main.js:2215`) and the moon's instant kill (`main.js:2244`).
///
/// It is worth noting how thoroughly dead the server's check is even on its own
/// terms: the motherships sat at `z = ±600` with a half-depth of 35, and the
/// field's outer radius is 400, so a candidate could never reach one.
/// `clipsMothership` has never rejected a single placement in production. The
/// only avoidance volume that ever mattered is the one the server does not have.
///
/// # Why there are no mothership volumes here at all now
///
/// The hulls are gone from the Rust world entirely
/// ([`crate::rules::WorldRules`]), so the two boxes went with them. What that
/// changes, exactly:
///
/// - **The radial field — deathmatch, trials, tutorial — is bit-identical.**
///   Its outer radius is 400 and the nearest hull face was 565, so the filter
///   could not fire and the accept/reject stream is unchanged.
///   `the_removed_mothership_volumes_were_outside_the_radial_field` pins the
///   arithmetic.
/// - **The campaign field changes for some seeds.** Its third slab runs to
///   `z = 540` ([`crate::rules::CAMPAIGN_ASTEROID_ZONES`]) and a *huge* rock
///   there carries 61 units of clearance, which did reach past 565 near the
///   centreline. Those placements were rejected and re-rolled; they are
///   accepted now. This is the correct outcome — there is no longer anything at
///   `z = 600` for a rock to be inside of — but it does mean campaign asteroid
///   layouts differ from the previous build at the same seed.
///
/// The campaign generator is worse still — `genCampaignAsteroids`
/// (`main.js:237`) performs no avoidance test whatsoever, and its middle zone
/// spans `x ∈ [-130, 130]`, `y ∈ [-65, 65]`, `z ∈ [-180, 200]`, which is centred
/// on the moon.
///
/// [`crate::rules::AsteroidFieldRules::avoid_moon`] is the unified rule and
/// [`Rules::validate`] refuses to let it be switched off. This function honours
/// it for **every** field shape, campaign included.
///
/// # Shape
///
/// The moon is avoided as the *box*
/// [`crate::rules::WorldRules::moon_avoid_half`] — a cube circumscribing the
/// sphere — because that is what `moonAvoid` is and what `clipsAvoidance` tests.
/// It is strictly more conservative than the sphere (it contains it), so
/// clearing the box also clears the moon itself, which is what
/// `no_generated_asteroid_is_inside_the_moon` asserts against the true
/// sphere.
///
/// The test itself is [`sphere_overlaps_aabb`] rather than the per-axis
/// comparison the JS uses, because the JS version rejects a *box*-shaped region
/// around each volume instead of the true rounded one — it over-rejects at the
/// corners. Over-rejection is safe here; it costs attempts, not correctness.
///
/// Returns an empty list for [`MapKind::Terrain`], which has no asteroid field
/// at all (`main.js:273` builds it from an empty array).
#[must_use]
pub fn avoid_volumes(rules: &Rules, map: MapKind) -> Vec<Aabb> {
    let mut out = Vec::new();
    if !matches!(map, MapKind::Space) {
        return out;
    }
    // The moon is now the whole list — see the section above on the two
    // mothership volumes that used to follow it and never rejected anything.
    if rules.world.asteroid_field.avoid_moon {
        out.push(Aabb::new(rules.world.moon_pos, rules.world.moon_avoid_half));
    }
    out
}

/// Whether a rock with clearance `clearance` centred at `pos` clips any
/// avoidance volume.
///
/// `clearance` is the rock's mesh `size` plus
/// [`crate::rules::AsteroidFieldRules::avoid_margin`], matching
/// `asteroids.js:116` (`const margin = asteroidRadius + 6`, whose argument at
/// `:163` is the mesh size, not the 0.95-scaled collision radius).
#[must_use]
pub fn clips_avoidance(pos: Vec3, clearance: f64, avoid: &[Aabb]) -> bool {
    avoid
        .iter()
        .any(|v| sphere_overlaps_aabb(Sphere::new(pos, clearance), *v))
}

// ---------------------------------------------------------------------------
// Placement
// ---------------------------------------------------------------------------

/// Draws a random direction, flattened on `y` so the field reads as a disc.
///
/// `asteroids.js:155` and `server/index.js:551`: three `[-1, 1)` draws with the
/// `y` one scaled by [`crate::rules::AsteroidFieldRules::y_flatten`], then
/// normalized. Note that this is a *cube* sample, so the direction is not
/// uniform on the sphere — it is denser toward the corners. That is a fidelity
/// choice, not an oversight: matching the JS matters more than a better
/// distribution, and the alternative needs `sin`/`cos`, which are not
/// bit-portable.
///
/// A degenerate all-zero draw normalizes to zero (see [`Vec3::normalize`]),
/// putting the candidate at the field centre — inside the moon — where the
/// caller's avoidance test rejects it like any other bad candidate. The JS
/// reaches the same outcome via its `|| 1` guard.
fn draw_direction(rng: &mut Rng, y_flatten: f64) -> Vec3 {
    // Field order is evaluation order in Rust, so x, y, z draw in that sequence,
    // exactly as the JS does.
    Vec3::new(
        rng.next_f64_signed(),
        rng.next_f64_signed() * y_flatten,
        rng.next_f64_signed(),
    )
    .normalize()
}

/// Places one rock in a sphere around the field centre.
///
/// Attempt loop as `asteroids.js:154` and `server/index.js:550`: draw a
/// direction and a distance in `[min_dist_base + size, radius)`, accept if it
/// clears every avoidance volume, give up after
/// [`crate::rules::AsteroidFieldRules::place_attempts`] tries. Each attempt
/// consumes exactly four draws whether or not it succeeds, so the stream
/// position after a rock depends only on how many attempts it took.
fn place_radial(
    rng: &mut Rng,
    field: &AsteroidFieldRules,
    radius: f64,
    size: f64,
    avoid: &[Aabb],
) -> Vec3 {
    let clearance = size + field.avoid_margin;
    let min_dist = field.min_dist_base + size;
    for _ in 0..field.place_attempts {
        let dir = draw_direction(rng, field.y_flatten);
        // `range_f64` is `lo + next_f64() * (hi - lo)`, the JS expression
        // character for character. It clamps to `lo` on an empty range, where
        // the JS would produce a distance *below* `min_dist`; `Rules::validate`
        // rejects the rule sets that can reach that.
        let dist = rng.range_f64(min_dist, radius);
        let cand = dir * dist;
        if !clips_avoidance(cand, clearance, avoid) {
            return cand;
        }
    }
    // Out of attempts. One more direction draw, so the fallback is still
    // seed-driven rather than a fixed point, then make it actually clear.
    let dir = draw_direction(rng, field.y_flatten);
    resolve_fallback(dir * radius, clearance, avoid)
}

/// Places one rock inside a campaign slab.
///
/// `main.js:259`–`:262`: independent uniform draws on each axis, `x` and `y`
/// symmetric about zero and `z` spanning the slab.
///
/// The JS performs **no** avoidance test here at all, which is why campaign
/// rocks sit inside the moon — the middle slab is centred on it. This adds the
/// same attempt loop the radial generator uses, so the unified `avoid_moon` rule
/// applies to every field shape rather than only to the two that already had a
/// filter.
fn place_in_zone(
    rng: &mut Rng,
    zone: &AsteroidZone,
    field: &AsteroidFieldRules,
    size: f64,
    avoid: &[Aabb],
) -> Vec3 {
    let clearance = size + field.avoid_margin;
    let mut last = Vec3::ZERO;
    for _ in 0..field.place_attempts {
        let cand = Vec3::new(
            (rng.next_f64() - 0.5) * 2.0 * zone.x_range,
            (rng.next_f64() - 0.5) * 2.0 * zone.y_range,
            rng.range_f64(zone.z_min, zone.z_max),
        );
        if !clips_avoidance(cand, clearance, avoid) {
            return cand;
        }
        last = cand;
    }
    // A slab is a corridor the mission flies down, so escaping *along* it would
    // move the rock out of its zone entirely. Push the last candidate out
    // instead, which keeps it as near to where it was drawn as clearing the
    // volume allows.
    resolve_fallback(last, clearance, avoid)
}

/// Turns a placement that sampling could not find into one that is actually
/// clear.
///
/// # The bug this fixes
///
/// Both JS generators fall back to the same expression after ten failed attempts
/// (`asteroids.js:169`, `server/index.js:562`):
///
/// ```text
/// pos = [(Math.random() - 0.5) * radius, 0, (Math.random() - 0.5) * radius];
/// ```
///
/// It is unconditional — nothing checks it against the avoidance volumes the
/// loop just spent ten attempts avoiding. With the shipped numbers it draws `x`
/// and `z` from `[-200, 200)` and pins `y` to exactly zero, which is the moon's
/// equatorial plane. The moon's avoidance box has 80 units of half-extent, so
/// roughly **21 %** of fallback placements land inside the moon, and the
/// distribution's mode — `x = z = 0` — is the moon's dead centre. A rock can be
/// generated at the literal origin.
///
/// Worse, the fallback runs most often for exactly the rocks that can least
/// afford it: a huge rock needs 61 units of clearance, so it is the one that
/// exhausts its attempts, and it is the one whose fallback covers the largest
/// slice of the moon.
///
/// # What this does instead
///
/// Two stages, neither of which draws a random number — the fallback's
/// randomness was already spent on the candidate handed in, so the stream
/// position does not depend on how the fallback resolves:
///
/// 1. **Axis push-out.** For each volume the candidate clips, slide it out along
///    the axis it is shallowest in, to just past the face ([`PLACEMENT_EPSILON`]
///    past, since [`sphere_overlaps_aabb`] counts tangency as contact).
///    Shallowest-axis is the minimum-translation exit, so the rock ends up as
///    near to where it was drawn as clearing the volume permits — which matters,
///    because the drawn position is the one that respects the field's shape.
///    Repeated up to [`PUSH_ROUNDS`] times, since pushing out of one volume
///    could in principle push into another.
/// 2. **Radial escape**, only if the push-out did not converge — which requires
///    avoidance volumes that overlap each other, and the shipped geometry has
///    none. Double the distance from the field centre until clear, up to
///    [`ESCAPE_DOUBLINGS`] times. Volumes are bounded, so this terminates.
///
/// The result can sit outside the nominal field radius. That is the deliberate
/// trade: a rock a little further out than intended is a cosmetic difference,
/// and a rock inside the moon is an invisible bullet sponge that also kills
/// whoever touches it.
fn resolve_fallback(pos: Vec3, clearance: f64, avoid: &[Aabb]) -> Vec3 {
    let mut p = pos;
    for _ in 0..PUSH_ROUNDS {
        if !clips_avoidance(p, clearance, avoid) {
            return p;
        }
        for v in avoid {
            // Expanding by the epsilon as well as the clearance means landing on
            // *this* box's face leaves the original box by a strict margin.
            let e = v.expanded(clearance + PLACEMENT_EPSILON);
            if e.contains_point(p) {
                p = push_out_of(p, e);
            }
        }
    }
    if !clips_avoidance(p, clearance, avoid) {
        return p;
    }
    // Push-out did not converge. Escape outward along whatever bearing we have;
    // a zero-length candidate has no bearing, so borrow `+z`.
    let dir = p.try_normalize().unwrap_or(Vec3::Z);
    let mut dist = p.length();
    if dist <= 0.0 {
        dist = clearance;
    }
    for _ in 0..ESCAPE_DOUBLINGS {
        dist *= 2.0;
        let cand = dir * dist;
        if !clips_avoidance(cand, clearance, avoid) {
            return cand;
        }
    }
    dir * dist
}

/// Slides `p` out of `e` along the axis it is least deep in.
///
/// `e` is expected to contain `p`; the caller checks. Ties between axes resolve
/// to the earliest of x, y, z, and a point exactly at the centre exits along
/// `+x`, so the choice never depends on anything but the inputs.
fn push_out_of(p: Vec3, e: Aabb) -> Vec3 {
    let d = p - e.center;
    // Penetration depth on each axis: how far inside the face `p` sits.
    let pen_x = e.half_extents.x - d.x.abs();
    let pen_y = e.half_extents.y - d.y.abs();
    let pen_z = e.half_extents.z - d.z.abs();
    // `signum` would carry the sign of a negative zero; an explicit test keeps a
    // dead-centre point exiting in the positive direction.
    let face = |offset: f64, half: f64| if offset < 0.0 { -half } else { half };
    if pen_x <= pen_y && pen_x <= pen_z {
        Vec3::new(e.center.x + face(d.x, e.half_extents.x), p.y, p.z)
    } else if pen_y <= pen_z {
        Vec3::new(p.x, e.center.y + face(d.y, e.half_extents.y), p.z)
    } else {
        Vec3::new(p.x, p.y, e.center.z + face(d.z, e.half_extents.z))
    }
}

// ---------------------------------------------------------------------------
// Generation
// ---------------------------------------------------------------------------

/// Draws a tier from the weighted table.
///
/// [`Rng::weighted_index`] is the deterministic replacement for `pickTier`
/// (`asteroids.js:20`), `pickAsteroidTier` (`server/index.js:522`), and the
/// inline accumulate-and-break at `main.js:254` — three copies of one loop.
fn draw_tier(rng: &mut Rng) -> AsteroidTier {
    AsteroidTier::from_index(rng.weighted_index(&asteroid_tier_weights()))
}

/// Draws a mesh size within `tier`'s band. `asteroids.js:151`.
fn draw_size(rng: &mut Rng, tier: AsteroidTier) -> f64 {
    let spec = tier_spec(tier);
    // Written the way the JS writes it (`min + rand * (max - min)`) rather than
    // as `range_f64`, which is the same arithmetic; spelled out because the size
    // band is the one place a reader will want to check the endpoints.
    spec.min_size + rng.next_f64() * (spec.max_size - spec.min_size)
}

/// The draws every rock makes once its position is settled: rotation, variant,
/// spin — in that order, for both field shapes.
///
/// Keeping the tail in one function is what stops the two generators drifting
/// apart in draw order the way the three JS ones did.
///
/// # Spin
///
/// `(rand - 0.5) * spin_scale` per axis, matching `asteroids.js:177`, with the
/// tier's scale applied **exactly once**.
///
/// That "once" is the fix. `createAsteroidFieldFromData` (`asteroids.js:90`)
/// multiplies whatever spin it receives by `spinScaleFor(tier)`, so a generator
/// feeding it must send the *raw* draw. The server does — `server/index.js:572`
/// sends a bare `random() - 0.5` — but the campaign generator sends an
/// already-narrowed `±0.2` on x and y and `±0.1` on z (`main.js:265`), which is
/// then scaled again, leaving campaign rocks turning at 20 % of everyone else's
/// rate on x and y and 40 % on z. There is no reading under which the campaign's
/// asymmetric z was intentional, so all three axes now use `spin_scale`.
///
/// # Variant
///
/// [`Rng::bounded_u32`] rather than `floor(random() * 6)`
/// (`server/index.js:571`). It is unbiased where the JS is not, and the
/// difference in draw *consumption* is the thing to know about: it reads 32-bit
/// words rather than 53-bit floats, and it re-draws on a short final block. With
/// `variant_count = 6` that rejection fires with probability 4/2³², i.e. never
/// in practice — and when it does fire it fires identically on every machine, so
/// the stream stays in step.
fn finish_rock(
    rng: &mut Rng,
    field: &AsteroidFieldRules,
    id: u32,
    tier: AsteroidTier,
    pos: Vec3,
    size: f64,
) -> Asteroid {
    let rot = Vec3::new(
        rng.next_f64() * ROTATION_RANGE,
        rng.next_f64() * ROTATION_RANGE,
        rng.next_f64() * ROTATION_RANGE,
    );
    let variant = rng.bounded_u32(field.variant_count);
    let scale = tier_spec(tier).spin_scale;
    let spin = Vec3::new(
        (rng.next_f64() - 0.5) * scale,
        (rng.next_f64() - 0.5) * scale,
        (rng.next_f64() - 0.5) * scale,
    );
    Asteroid {
        id,
        pos,
        size,
        radius: size * field.collision_radius_scale,
        hp: tier_hp(tier),
        tier,
        // `variant_count` is a handful, so the cast cannot lose information for
        // any sane rule set; saturating rather than wrapping keeps a silly one
        // in range instead of aliasing variant 256 onto variant 0.
        variant: u8::try_from(variant).unwrap_or(u8::MAX),
        rot,
        spin,
        hit_flash: 0.0,
    }
}

/// Generates a spherical field around the origin.
///
/// This is `createAsteroidField` (`asteroids.js:110`) and
/// `generateAsteroidField` (`server/index.js:544`), unified. Ids run from
/// `first_id` upward in generation order.
///
/// `count` and `radius` are arguments rather than reads from `field` because the
/// trials modes reuse the same rules with a different rock count; keeping both
/// dimensions open means one function serves every caller.
#[must_use]
pub fn generate_radial_field(
    rng: &mut Rng,
    field: &AsteroidFieldRules,
    count: u32,
    radius: f64,
    avoid: &[Aabb],
    first_id: u32,
) -> Vec<Asteroid> {
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let tier = draw_tier(rng);
        let size = draw_size(rng, tier);
        let pos = place_radial(rng, field, radius, size, avoid);
        out.push(finish_rock(rng, field, first_id + i, tier, pos, size));
    }
    out
}

/// Generates a campaign field: three boxed slabs along the flight path.
///
/// This is `genCampaignAsteroids` (`main.js:237`), with the moon avoidance it
/// never had and without the double spin scaling it did have. Zones
/// are filled in [`crate::rules::CampaignRules::asteroid_zones`] order and ids
/// run from `first_id` upward across all three.
#[must_use]
pub fn generate_campaign_field(
    rng: &mut Rng,
    rules: &Rules,
    avoid: &[Aabb],
    first_id: u32,
) -> Vec<Asteroid> {
    let field = &rules.world.asteroid_field;
    let zones = &rules.campaign.asteroid_zones;
    let total: u32 = zones.iter().map(|z| z.count).sum();
    let mut out = Vec::with_capacity(total as usize);
    let mut id = first_id;
    for zone in zones {
        for _ in 0..zone.count {
            let tier = draw_tier(rng);
            let size = draw_size(rng, tier);
            let pos = place_in_zone(rng, zone, field, size, avoid);
            out.push(finish_rock(rng, field, id, tier, pos, size));
            id += 1;
        }
    }
    out
}

/// Rocks in a standard field for `mode`.
///
/// Trials use a denser field as the course number rises
/// ([`crate::rules::AsteroidFieldRules::trials_counts`], `main.js:232`); the
/// campaign's count is the sum of its three slabs; everything else uses
/// [`crate::rules::AsteroidFieldRules::count`].
#[must_use]
pub fn field_count(rules: &Rules, mode: Mode) -> u32 {
    let field = &rules.world.asteroid_field;
    match mode {
        Mode::Trials(n) => {
            // `main.js:232` is a ternary chain over the mode string whose
            // trials-but-unrecognised fallback is 120, i.e. trial 1's count.
            field.trials_counts[usize::from(n.clamp(1, 4)) - 1]
        }
        Mode::Campaign(_) => rules.campaign.asteroid_zones.iter().map(|z| z.count).sum(),
        _ => field.count,
    }
}

/// Generates the field for a mode and map.
///
/// The single entry point: it picks the field shape, the rock count, and the
/// avoidance list, so no caller needs to know that the campaign is a corridor or
/// that the terrain map has no rocks.
#[must_use]
pub fn generate(rng: &mut Rng, rules: &Rules, mode: Mode, map: MapKind) -> Vec<Asteroid> {
    // `main.js:272`: the terrain map builds its field from an empty array.
    if !matches!(map, MapKind::Space) {
        return Vec::new();
    }
    let avoid = avoid_volumes(rules, map);
    let field = &rules.world.asteroid_field;
    match mode {
        Mode::Campaign(_) => generate_campaign_field(rng, rules, &avoid, FIRST_ASTEROID_ID),
        _ => generate_radial_field(
            rng,
            field,
            field_count(rules, mode),
            field.radius,
            &avoid,
            FIRST_ASTEROID_ID,
        ),
    }
}

/// Fills [`World::asteroids`] from the world's own
/// [`crate::world::WorldRng::field`] stream.
///
/// This is the call match setup makes. Because the field draws from a dedicated
/// stream, adding or removing a random draw in bot AI or spawn jitter cannot
/// move a single rock.
///
/// Replaces the whole field and leaves [`World::next_asteroid_id`] pointing past
/// the last id used.
pub fn populate(world: &mut World) {
    let rules = world.rules;
    let mode = world.mode;
    let map = world.map;
    let field = generate(&mut world.rng.field, &rules, mode, map);
    world.next_asteroid_id = field
        .iter()
        .map(|a| a.id + 1)
        .max()
        .unwrap_or(FIRST_ASTEROID_ID);
    world.asteroids = field;
}

// ---------------------------------------------------------------------------
// Per-step integration
// ---------------------------------------------------------------------------

/// Advances rotation and fades the damage flash.
///
/// `asteroids.js:95`–`:105`. Both quantities are cosmetic — collision reads
/// [`crate::world::Asteroid::radius`], so a rock is a sphere no matter how it is
/// drawn — but both are *shared* cosmetics two clients must agree on, which is
/// why they are integrated here and not in the renderer.
///
/// Rotation is deliberately not wrapped into `[0, 2π)`. The JS lets
/// `mesh.rotation` grow without bound, and it stays small: the fastest spin is
/// 0.25 rad/s, so a 300-second match accumulates 75 radians and loses nothing.
/// Wrapping would mean a `rem_euclid` per axis per rock per tick, trading exact,
/// obviously-portable addition for an operation with a less obvious portability
/// story, in exchange for precision no one can see.
pub fn integrate(asteroids: &mut [Asteroid], dt: f64) {
    for a in asteroids.iter_mut() {
        a.rot = a.rot.add_scaled(a.spin, dt);
        if a.hit_flash > 0.0 {
            a.hit_flash = (a.hit_flash - dt * HIT_FLASH_DECAY_RATE).max(0.0);
        }
    }
}

/// [`integrate`] over a whole world.
pub fn step(world: &mut World, dt: f64) {
    integrate(&mut world.asteroids, dt);
}

/// Collision spheres for the whole field, in field order.
///
/// The shape [`crate::collision::sweep_first_hit`] wants for the
/// bullet-versus-sixty-rocks inner loop. Reuses `out`'s allocation, so a caller
/// that keeps one buffer never allocates after the first tick.
///
/// The index a sweep returns is an index into `asteroids`, **not** an
/// [`crate::world::Asteroid::id`]; the two coincide only until the first rock is
/// destroyed.
pub fn collision_spheres(asteroids: &[Asteroid], out: &mut Vec<Sphere>) {
    out.clear();
    out.extend(asteroids.iter().map(|a| Sphere::new(a.pos, a.radius)));
}

// ---------------------------------------------------------------------------
// Damage
// ---------------------------------------------------------------------------

/// Applies one weapon impact to the asteroid with this id.
///
/// One hit removes [`crate::rules::CombatRules::asteroid_damage_per_hit`] hit
/// points **regardless of weapon** — a 50-damage missile and a 10-damage bullet
/// chip a rock identically (`main.js:2183`, `server/index.js:813`). That is the
/// shipped rule, preserved.
///
/// Emits [`SimEvent::AsteroidDamaged`] on a survivable hit. On a killing hit it
/// removes the rock from [`World::asteroids`] and emits two events, in this
/// order:
///
/// 1. [`SimEvent::AsteroidDestroyed`] — the rock is gone; drop its mesh.
/// 2. [`SimEvent::Explosion`] with [`ExplosionKind::AsteroidBreak`] — the debris
///    burst and the `rockbreak` cue.
///
/// Two events rather than one because the JS has two consequences
/// (`main.js:2186`–`:2189`: `asteroids.destroy(id)` removes the mesh, then
/// `bullets.spawnExplosion` and `audio.play('rockbreak')` are separate effects),
/// and a caller that only wants to stop drawing the rock should not have to
/// parse an explosion to learn that. **This module does not know what a sound
/// is** — it reports that a rock broke and where; the audio layer decides the
/// rest.
///
/// # Indices, ids, and the one that survives
///
/// Destruction removes an element from the middle of `world.asteroids`, so every
/// index at or after it shifts. Ids do not move. Anything holding a position in
/// the field across this call — a swept-collision result, a
/// [`collision_spheres`] buffer — must convert to an id first, or re-derive
/// afterwards.
pub fn apply_damage(world: &mut World, id: u32, events: &mut Vec<SimEvent>) -> DamageOutcome {
    let per_hit = world.rules.combat.asteroid_damage_per_hit;
    let Some(idx) = world.asteroids.iter().position(|a| a.id == id) else {
        return DamageOutcome::Ignored;
    };
    let rock = &mut world.asteroids[idx];
    // `main.js:2182` and `server/index.js:812`, both `if (!a || a.hp <= 0)
    // return;`. Reachable: a hit report can arrive a tick after the rock died.
    if rock.hp <= 0 {
        return DamageOutcome::Ignored;
    }
    rock.hp = (rock.hp - per_hit).max(0);
    rock.hit_flash = 1.0;
    let hp = rock.hp;
    if hp > 0 {
        events.push(SimEvent::AsteroidDamaged { id, hp });
        return DamageOutcome::Damaged { hp };
    }
    let pos = rock.pos;
    let radius = rock.radius;
    world.asteroids.remove(idx);
    events.push(SimEvent::AsteroidDestroyed { id, pos, radius });
    events.push(SimEvent::Explosion {
        pos,
        scale: radius,
        kind: ExplosionKind::AsteroidBreak,
    });
    DamageOutcome::Destroyed { pos, radius }
}

/// Overwrites an asteroid's hit points from an authoritative source.
///
/// The [`crate::world::Authority::Server`] reconciliation path: the JS handles
/// `asteroid-hp` by calling `setHp` (`main.js:966` into `asteroids.js:69`),
/// which also lights the damage flash — the flash is how a *remote* player's hit
/// becomes visible locally, so it is part of the message's meaning and not a
/// side effect of the local hit path.
///
/// Returns whether the id was found. Does **not** destroy a rock set to zero:
/// the server sends a separate `asteroid-destroyed` for that
/// (`server/index.js:814`), and inferring destruction here would double-fire the
/// break when both messages arrive.
pub fn set_hp(world: &mut World, id: u32, hp: i32) -> bool {
    match world.asteroid_mut(id) {
        Some(rock) => {
            rock.hp = hp;
            rock.hit_flash = 1.0;
            true
        }
        None => false,
    }
}

/// Destroys an asteroid outright, from an authoritative source.
///
/// The `asteroid-destroyed` handler (`main.js:967`), which removes the mesh and
/// then plays the same explosion and `rockbreak` the local path does. Emits the
/// identical event pair a killing [`apply_damage`] does, so a caller cannot tell
/// which side resolved the hit — which is the point.
///
/// Returns whether the id was found.
pub fn destroy(world: &mut World, id: u32, events: &mut Vec<SimEvent>) -> bool {
    let Some(idx) = world.asteroids.iter().position(|a| a.id == id) else {
        return false;
    };
    let rock = world.asteroids.remove(idx);
    events.push(SimEvent::AsteroidDestroyed {
        id,
        pos: rock.pos,
        radius: rock.radius,
    });
    events.push(SimEvent::Explosion {
        pos: rock.pos,
        scale: rock.radius,
        kind: ExplosionKind::AsteroidBreak,
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collision::sphere_overlaps_sphere;
    use crate::rules::{ASTEROID_TIER_COUNT, CAMPAIGN_ASTEROID_ZONES};
    use crate::world::{Authority, RNG_STREAM_FIELD};

    const SEED: u64 = 0xA57E_2011_D000_0001;

    fn rules() -> Rules {
        Rules::DEFAULT
    }

    fn field_rng(seed: u64) -> Rng {
        Rng::with_stream(seed, RNG_STREAM_FIELD)
    }

    fn space_field(seed: u64) -> Vec<Asteroid> {
        generate(
            &mut field_rng(seed),
            &rules(),
            Mode::Skirmish,
            MapKind::Space,
        )
    }

    fn campaign_field(seed: u64) -> Vec<Asteroid> {
        generate(
            &mut field_rng(seed),
            &rules(),
            Mode::Campaign(1),
            MapKind::Space,
        )
    }

    /// FNV-1a over every bit of every rock. Any change at all — a moved rock, a
    /// reordered draw, a different tier — moves this number.
    fn checksum(field: &[Asteroid]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for a in field {
            for word in [
                u64::from(a.id),
                a.pos.x.to_bits(),
                a.pos.y.to_bits(),
                a.pos.z.to_bits(),
                a.size.to_bits(),
                a.radius.to_bits(),
                a.hp as u64,
                a.tier.index() as u64,
                u64::from(a.variant),
                a.rot.x.to_bits(),
                a.rot.y.to_bits(),
                a.rot.z.to_bits(),
                a.spin.x.to_bits(),
                a.spin.y.to_bits(),
                a.spin.z.to_bits(),
                a.hit_flash.to_bits(),
            ] {
                h ^= word;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        h
    }

    fn assert_bit_identical(a: &[Asteroid], b: &[Asteroid]) {
        assert_eq!(a.len(), b.len(), "field lengths differ");
        for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            assert_eq!(x.id, y.id, "rock {i}: id");
            assert_eq!(x.tier, y.tier, "rock {i}: tier");
            assert_eq!(x.hp, y.hp, "rock {i}: hp");
            assert_eq!(x.variant, y.variant, "rock {i}: variant");
            for (label, p, q) in [
                ("pos.x", x.pos.x, y.pos.x),
                ("pos.y", x.pos.y, y.pos.y),
                ("pos.z", x.pos.z, y.pos.z),
                ("size", x.size, y.size),
                ("radius", x.radius, y.radius),
                ("rot.x", x.rot.x, y.rot.x),
                ("rot.y", x.rot.y, y.rot.y),
                ("rot.z", x.rot.z, y.rot.z),
                ("spin.x", x.spin.x, y.spin.x),
                ("spin.y", x.spin.y, y.spin.y),
                ("spin.z", x.spin.z, y.spin.z),
                ("hit_flash", x.hit_flash, y.hit_flash),
            ] {
                assert_eq!(
                    p.to_bits(),
                    q.to_bits(),
                    "rock {i}: {label} differs in bits ({p} vs {q})"
                );
            }
        }
    }

    // -----------------------------------------------------------------
    // The property the whole module exists for.
    // -----------------------------------------------------------------

    #[test]
    fn same_seed_regenerates_a_byte_identical_field() {
        // This is the trade the seed makes possible: the server sends 8 bytes
        // instead of 60 asteroid records, and every client rebuilds the field
        // locally. If this ever fails, that trade is unsound and players are
        // shooting at rocks their opponents cannot see.
        for seed in [0, 1, SEED, u64::MAX, 0x5EED_0000_0000_0001] {
            let a = space_field(seed);
            let b = space_field(seed);
            assert_bit_identical(&a, &b);
            assert_eq!(checksum(&a), checksum(&b));
        }
    }

    #[test]
    fn same_seed_regenerates_every_field_shape_identically() {
        for mode in [
            Mode::Multiplayer,
            Mode::Skirmish,
            Mode::Trials(1),
            Mode::Trials(4),
            Mode::Campaign(1),
            Mode::Campaign(3),
        ] {
            let a = generate(&mut field_rng(SEED), &rules(), mode, MapKind::Space);
            let b = generate(&mut field_rng(SEED), &rules(), mode, MapKind::Space);
            assert_bit_identical(&a, &b);
            assert!(!a.is_empty(), "{mode:?} generated nothing");
        }
    }

    #[test]
    fn two_worlds_built_from_one_seed_agree() {
        // The realistic path: two machines each construct a World from the seed
        // the server broadcast, and populate it independently.
        let mut server = World::new(SEED, rules(), Mode::Multiplayer, MapKind::Space);
        let mut client = World::new(SEED, rules(), Mode::Multiplayer, MapKind::Space);
        populate(&mut server);
        populate(&mut client);
        assert_bit_identical(&server.asteroids, &client.asteroids);
        assert_eq!(server.next_asteroid_id, client.next_asteroid_id);
        assert_eq!(server.next_asteroid_id, 60);
        // The authority split must not touch the geometry.
        assert_eq!(server.authority, Authority::Server);
    }

    #[test]
    fn different_seeds_produce_different_fields() {
        let a = space_field(1);
        let b = space_field(2);
        assert_eq!(a.len(), b.len());
        assert_ne!(checksum(&a), checksum(&b));
    }

    /// **Do not update this number to make a failing test pass.**
    ///
    /// It pins the generated field for one seed, which pins the draw order, the
    /// draw count, the placement algorithm, the tier table, and the RNG all at
    /// once. If it fails, a client and a server built at different times will
    /// disagree about where the rocks are. Either revert the change, or treat it
    /// as a protocol break and version the seed.
    ///
    /// Moved once, knowingly:
    /// [`crate::rules::AsteroidFieldRules::collision_radius_scale`] stopped
    /// being the JS's literal 0.95 and became the drawn mesh's mean radius, so
    /// every rock's `radius` — which this hashes — moved with it. Rocks are in
    /// the same places, at the same sizes, in the same order; only the sphere
    /// standing in for each one is smaller. Like any rule change it needs client
    /// and server rebuilt together, which the seed does not version.
    const GOLDEN_FIELD_CHECKSUM: u64 = 0x4A7C_1A8F_DC9C_7ECF;

    #[test]
    fn generation_sequence_is_pinned() {
        let field = space_field(SEED);
        assert_eq!(field.len(), 60);
        assert_eq!(
            checksum(&field),
            GOLDEN_FIELD_CHECKSUM,
            "the asteroid field generator changed \
             — see the comment above GOLDEN_FIELD_CHECKSUM"
        );
    }

    #[test]
    fn generation_does_not_disturb_the_other_rng_streams() {
        // Five streams exist so a change in one subsystem cannot move another's
        // numbers. Generating a field must leave the rest untouched.
        let mut world = World::new(SEED, rules(), Mode::Skirmish, MapKind::Space);
        let before = world.rng.clone();
        populate(&mut world);
        assert_ne!(
            world.rng.field, before.field,
            "the field stream must advance"
        );
        assert_eq!(world.rng.spawn, before.spawn);
        assert_eq!(world.rng.combat, before.combat);
        assert_eq!(world.rng.bots, before.bots);
        assert_eq!(world.rng.effects, before.effects);
    }

    // -----------------------------------------------------------------
    // Placement rejection — the moon bug.
    // -----------------------------------------------------------------

    #[test]
    fn no_generated_asteroid_is_inside_the_moon() {
        // The server's `clipsMothership` checked only the motherships, so
        // multiplayer rocks generate inside the moon, where they eat bullets
        // invisibly. Every field shape, over many seeds, must now clear it.
        let rules = rules();
        let moon = Sphere::new(rules.world.moon_pos, rules.world.moon_radius);

        let mut checked = 0usize;
        for seed in 0..40u64 {
            for mode in [
                Mode::Multiplayer,
                Mode::Trials(4),
                Mode::Campaign(2),
                Mode::Tutorial,
            ] {
                let field = generate(&mut field_rng(seed), &rules, mode, MapKind::Space);
                assert!(!field.is_empty());
                for a in &field {
                    // Tested against the rock's true collision sphere, not the
                    // padded clearance the generator rejects with — this asserts
                    // the outcome that matters, not the filter that produced it.
                    let body = Sphere::new(a.pos, a.radius);
                    assert!(
                        !sphere_overlaps_sphere(body, moon),
                        "{mode:?} seed {seed}: rock {} at {:?} (r {}) is inside the moon",
                        a.id,
                        a.pos,
                        a.radius
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 24_000, "only checked {checked} rocks");
    }

    #[test]
    fn no_generated_asteroid_sits_at_the_field_origin() {
        // The JS fallback's modal outcome is (0, 0, 0) — the dead centre of the
        // moon. Nothing may ever land there.
        for seed in 0..40u64 {
            for mode in [Mode::Multiplayer, Mode::Trials(4), Mode::Campaign(2)] {
                for a in generate(&mut field_rng(seed), &rules(), mode, MapKind::Space) {
                    assert_ne!(
                        a.pos,
                        Vec3::ZERO,
                        "{mode:?} seed {seed}: rock at the origin"
                    );
                }
            }
        }
    }

    #[test]
    fn the_fallback_path_still_clears_every_volume() {
        // Force the attempt loop to fail constantly: one attempt per rock, and a
        // moon avoidance box so large that most of the field is inside it. Every
        // rock then resolves through `resolve_fallback`, which is the path the
        // JS leaves unchecked.
        let mut hostile = rules();
        hostile.world.moon_avoid_half = Vec3::splat(300.0);
        hostile.world.asteroid_field.place_attempts = 1;

        let avoid = avoid_volumes(&hostile, MapKind::Space);
        let mut fell_back = 0usize;
        for seed in 0..20u64 {
            let field = generate(
                &mut field_rng(seed),
                &hostile,
                Mode::Multiplayer,
                MapKind::Space,
            );
            for a in &field {
                let clearance = a.size + hostile.world.asteroid_field.avoid_margin;
                assert!(
                    !clips_avoidance(a.pos, clearance, &avoid),
                    "seed {seed}: fallback rock {} at {:?} still clips",
                    a.id,
                    a.pos
                );
                assert_ne!(a.pos, Vec3::ZERO);
                // Anything pushed clear of the 300-unit box ends up beyond it;
                // count those to prove the path really ran.
                if a.pos.x.abs() > 300.0 || a.pos.y.abs() > 300.0 || a.pos.z.abs() > 300.0 {
                    fell_back += 1;
                }
            }
        }
        assert!(fell_back > 0, "the fallback path was never exercised");
    }

    #[test]
    fn the_fallback_pushes_a_dead_centre_candidate_out_of_the_moon() {
        // The exact JS failure: a candidate at the origin. It must come out, and
        // by the shortest route rather than to somewhere arbitrary.
        let rules = rules();
        let avoid = avoid_volumes(&rules, MapKind::Space);
        for size in [5.0, 15.0, 30.0, 55.0] {
            let clearance = size + rules.world.asteroid_field.avoid_margin;
            let out = resolve_fallback(Vec3::ZERO, clearance, &avoid);
            assert!(
                !clips_avoidance(out, clearance, &avoid),
                "size {size}: {out:?}"
            );
            assert_ne!(out, Vec3::ZERO);
            let half = rules.world.moon_avoid_half.x + clearance;
            assert!(
                (out.x.abs() - half).abs() < 1.0e-3,
                "expected an x-axis exit at {half}, got {out:?}"
            );
            assert_eq!(out.y, 0.0);
            assert_eq!(out.z, 0.0);
        }
    }

    #[test]
    fn the_fallback_is_deterministic_and_draws_nothing() {
        // The fallback must not consume randomness: two calls with the same
        // candidate produce the same answer, bit for bit.
        let avoid = avoid_volumes(&rules(), MapKind::Space);
        for p in [
            Vec3::ZERO,
            Vec3::new(1.0, -2.0, 3.0),
            Vec3::new(-40.0, 70.0, 12.0),
            Vec3::new(0.0, 0.0, 600.0),
        ] {
            let a = resolve_fallback(p, 11.0, &avoid);
            let b = resolve_fallback(p, 11.0, &avoid);
            assert_eq!(a.x.to_bits(), b.x.to_bits());
            assert_eq!(a.y.to_bits(), b.y.to_bits());
            assert_eq!(a.z.to_bits(), b.z.to_bits());
            assert!(!clips_avoidance(a, 11.0, &avoid), "{p:?} -> {a:?}");
        }
        // A candidate that was already clear is returned untouched.
        let clear = Vec3::new(0.0, 0.0, 300.0);
        assert_eq!(resolve_fallback(clear, 11.0, &avoid), clear);
    }

    #[test]
    fn avoid_volumes_follow_the_map_and_the_rule() {
        let rules = rules();
        let space = avoid_volumes(&rules, MapKind::Space);
        assert_eq!(space.len(), 1, "the moon, and nothing else");
        assert_eq!(space[0].half_extents, Vec3::splat(80.0));
        assert!(avoid_volumes(&rules, MapKind::Terrain).is_empty());

        // The bug state, reachable only by hand: `Rules::validate` rejects it.
        let mut no_moon = rules;
        no_moon.world.asteroid_field.avoid_moon = false;
        assert!(avoid_volumes(&no_moon, MapKind::Space).is_empty());
        assert!(no_moon.validate().is_err());
    }

    #[test]
    fn the_removed_mothership_volumes_were_outside_the_radial_field() {
        // Why deleting the motherships costs the radial field nothing, and why
        // the server's `clipsMothership` was dead code before that: the hulls
        // sat at |z| = 600 with a half-depth of 35 — the JS numbers, written out
        // here because `WorldRules` no longer carries them — so the nearest face
        // was at 565, and no candidate drawn inside a 400-unit sphere can reach
        // it. Nothing was ever rejected, so nothing about the accept/reject
        // stream moves when the volumes go.
        //
        // The campaign field is the exception and is *not* covered here: its
        // third slab runs to z = 540 and a huge rock's clearance did cross 565.
        // See `avoid_volumes`.
        let rules = rules();
        let nearest_face = 600.0 - 35.0;
        assert!(
            rules.world.asteroid_field.radius < nearest_face,
            "field radius {} vs nearest mothership face {nearest_face}",
            rules.world.asteroid_field.radius
        );
    }

    // -----------------------------------------------------------------
    // Field shape.
    // -----------------------------------------------------------------

    #[test]
    fn field_counts_follow_the_mode() {
        let rules = rules();
        assert_eq!(field_count(&rules, Mode::Multiplayer), 60);
        assert_eq!(field_count(&rules, Mode::Skirmish), 60);
        assert_eq!(field_count(&rules, Mode::Trials(1)), 120);
        assert_eq!(field_count(&rules, Mode::Trials(2)), 150);
        assert_eq!(field_count(&rules, Mode::Trials(3)), 180);
        assert_eq!(field_count(&rules, Mode::Trials(4)), 210);
        // Unknown trial numbers clamp, like the JS ternary chain's fallback.
        assert_eq!(field_count(&rules, Mode::Trials(0)), 120);
        assert_eq!(field_count(&rules, Mode::Trials(99)), 210);
        assert_eq!(field_count(&rules, Mode::Campaign(1)), 280);

        for mode in [Mode::Multiplayer, Mode::Trials(3), Mode::Campaign(2)] {
            let n = generate(&mut field_rng(SEED), &rules, mode, MapKind::Space).len();
            assert_eq!(n as u32, field_count(&rules, mode), "{mode:?}");
        }
    }

    #[test]
    fn the_terrain_map_has_no_asteroids() {
        // `main.js:272`: the terrain map builds its field from an empty array.
        let mut world = World::new(SEED, rules(), Mode::Multiplayer, MapKind::Terrain);
        let before = world.rng.field.clone();
        populate(&mut world);
        assert!(world.asteroids.is_empty());
        assert_eq!(world.next_asteroid_id, FIRST_ASTEROID_ID);
        assert_eq!(world.rng.field, before, "an empty field must draw nothing");
    }

    #[test]
    fn ids_are_zero_based_and_dense() {
        for mode in [Mode::Multiplayer, Mode::Trials(2), Mode::Campaign(3)] {
            let field = generate(&mut field_rng(SEED), &rules(), mode, MapKind::Space);
            for (i, a) in field.iter().enumerate() {
                assert_eq!(a.id, i as u32, "{mode:?}: ids must be dense from zero");
            }
            assert_eq!(field[0].id, FIRST_ASTEROID_ID);
        }
        assert_eq!(FIRST_ASTEROID_ID, 0);
    }

    #[test]
    fn sizes_and_hp_stay_inside_their_tier() {
        let mut seen = [0usize; ASTEROID_TIER_COUNT];
        for seed in 0..30u64 {
            for a in space_field(seed) {
                let spec = tier_spec(a.tier);
                assert!(
                    a.size >= spec.min_size && a.size < spec.max_size,
                    "{:?}: size {} outside [{}, {})",
                    a.tier,
                    a.size,
                    spec.min_size,
                    spec.max_size
                );
                assert_eq!(a.hp, spec.hp);
                assert_eq!(
                    a.radius,
                    a.size * rules().world.asteroid_field.collision_radius_scale
                );
                assert!(a.variant < 6);
                assert_eq!(a.hit_flash, 0.0);
                for r in [a.rot.x, a.rot.y, a.rot.z] {
                    assert!((0.0..ROTATION_RANGE).contains(&r), "rotation {r}");
                }
                seen[a.tier.index()] += 1;
            }
        }
        for (i, n) in seen.iter().enumerate() {
            assert!(*n > 0, "tier {i} never appeared");
        }
    }

    #[test]
    fn the_collision_sphere_is_the_drawn_rocks_mean_radius() {
        // "Their hitboxes are weird and not accurate", measured.
        //
        // A rock is drawn as a unit icosphere with every vertex displaced to
        // `lobe * bump` of the nominal radius, each octave an independent
        // per-vertex hash in [-1, 1) — see `AsteroidMeshRules`. Integrating over
        // that square is integrating over the drawn surface, because the hash is
        // what the vertex radii are drawn from.
        //
        // The JS collided against `size * 0.95`. This is what that sphere was
        // standing in front of.
        let mesh = rules().world.asteroid_field.mesh;
        let scale = rules().world.asteroid_field.collision_radius_scale;
        const JS_SCALE: f64 = 0.95;
        const STEPS: i32 = 401;

        let mut sum = 0.0f64;
        let mut js_gap = 0.0f64;
        let mut outside_js = 0u32;
        let mut outside_ours = 0u32;
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        let mut samples = 0u32;
        for i in 0..STEPS {
            let n1 = -1.0 + 2.0 * f64::from(i) / f64::from(STEPS - 1);
            for j in 0..STEPS {
                let n2 = -1.0 + 2.0 * f64::from(j) / f64::from(STEPS - 1);
                let drawn =
                    (mesh.lobe_base + mesh.lobe_amp * n1) * (mesh.bump_base + mesh.bump_amp * n2);
                sum += drawn;
                js_gap += JS_SCALE - drawn;
                outside_js += u32::from(drawn > JS_SCALE);
                outside_ours += u32::from(drawn > scale);
                lo = lo.min(drawn);
                hi = hi.max(drawn);
                samples += 1;
            }
        }
        let n = f64::from(samples);
        let mean = sum / n;

        // The description's mean is exactly the product of the two octaves'
        // means, because each noise term is symmetric about zero.
        assert!((mean - mesh.mean_radius_scale()).abs() < 1e-12, "{mean}");
        assert_eq!(scale, mesh.mean_radius_scale());
        assert!((scale - 0.7332).abs() < 1e-12, "{scale}");

        // The envelope the description can reach, and the sphere sitting inside
        // it rather than around it.
        assert!((lo - mesh.min_radius_scale()).abs() < 1e-12, "{lo}");
        assert!((hi - mesh.max_radius_scale()).abs() < 1e-12, "{hi}");
        assert!(lo < scale && scale < hi);

        // What was wrong: the JS sphere stood 0.217 of a `size` clear of the
        // surface it was standing in for — 23 % of its own radius, 2.2 times the
        // volume, and 11.9 units of invisible wall on the largest rock tier.
        let js_mean_gap = js_gap / n;
        assert!((js_mean_gap - 0.2168).abs() < 1e-4, "{js_mean_gap}");
        let biggest = ASTEROID_TIERS[ASTEROID_TIER_COUNT - 1].max_size;
        assert!((js_mean_gap * biggest - 11.9).abs() < 0.1);
        assert!(((JS_SCALE / scale).powi(3) - 2.18).abs() < 0.01);

        // And the sphere that replaces it splits the surface down the middle,
        // where the JS's had 84 % of the rock inside it.
        let ours = f64::from(outside_ours) / n;
        let js = f64::from(outside_js) / n;
        assert!((ours - 0.5).abs() < 0.02, "{ours} of the surface pokes out");
        assert!(js < 0.16, "{js}");
    }

    #[test]
    fn every_rocks_hitbox_is_derived_from_the_mesh_and_not_from_a_literal() {
        // The property that stops the two drifting apart again: nothing writes
        // a collision radius down, it comes off the same description the
        // renderer builds the mesh from.
        let field = rules().world.asteroid_field;
        for a in space_field(SEED) {
            assert_eq!(a.radius, a.size * field.mesh.mean_radius_scale());
            assert!(a.radius > a.size * field.mesh.min_radius_scale());
            assert!(a.radius < a.size * field.mesh.max_radius_scale());
        }
        // A rule set whose sphere is not somewhere on the rock is not a rule set.
        let mut absurd = rules();
        absurd.world.asteroid_field.collision_radius_scale = 2.0;
        assert!(absurd.validate().is_err());
    }

    #[test]
    fn generated_rocks_overlap_each_other_because_nothing_separates_them() {
        // Pinned because it is load-bearing elsewhere: `ship::depenetrate`
        // iterates precisely because a ship can end a step inside two rocks at
        // once, and this is why that is not a hypothetical. Placement is tested
        // against the moon (`avoid_volumes`) and never against another rock —
        // in all three JS generators, and here.
        //
        // Not a bug to be fixed by separating them: a field of touching rocks is
        // what an asteroid field looks like, and it is what the campaign's three
        // slabs are *for*. The fix belongs in the resolver, which is where it is.
        let knotted = |field: &[Asteroid]| {
            let mut count = 0;
            for (i, a) in field.iter().enumerate() {
                let body = Sphere::new(a.pos, a.radius);
                if field.iter().enumerate().any(|(j, b)| {
                    i != j && sphere_overlaps_sphere(body, Sphere::new(b.pos, b.radius))
                }) {
                    count += 1;
                }
            }
            count
        };

        // The campaign packs 280 rocks into three slabs, and it shows: 118 of
        // them — 42 % — are touching another rock. That is the field the mission
        // flies down.
        let campaign = campaign_field(SEED);
        let campaign_knotted = knotted(&campaign);
        assert!(
            campaign_knotted > campaign.len() / 4,
            "{campaign_knotted} of {} campaign rocks are in a knot",
            campaign.len()
        );

        // A deathmatch field is far sparser — 60 rocks in a 400-unit sphere —
        // but not empty of knots either, over enough seeds.
        let mut space_knotted = 0;
        for seed in 0..20u64 {
            space_knotted += knotted(&space_field(seed));
        }
        assert!(space_knotted > 0, "no knots in twenty deathmatch fields");
    }

    #[test]
    fn tier_frequencies_track_the_weights() {
        let mut counts = [0usize; ASTEROID_TIER_COUNT];
        let mut total = 0usize;
        for seed in 0..400u64 {
            for a in space_field(seed) {
                counts[a.tier.index()] += 1;
                total += 1;
            }
        }
        for (i, spec) in ASTEROID_TIERS.iter().enumerate() {
            let observed = counts[i] as f64 / total as f64;
            assert!(
                (observed - spec.weight).abs() < 0.01,
                "tier {i}: expected ~{}, observed {observed}",
                spec.weight
            );
        }
    }

    #[test]
    fn the_field_is_a_flattened_disc_inside_its_radius() {
        // `y_flatten` squashes the direction draw *before* it is normalized, so
        // it biases the field flat rather than capping its height — a draw of
        // (0.01, 0.4, 0.01) still normalizes to almost straight up, and that
        // rock is legitimately 400 units above the plane. The property to assert
        // is therefore about the average shape, not the extreme.
        let radius = rules().world.asteroid_field.radius;
        let mut sum_y = 0.0f64;
        let mut sum_xz = 0.0f64;
        let mut n = 0.0f64;
        for seed in 0..20u64 {
            for a in space_field(seed) {
                sum_y += a.pos.y.abs();
                sum_xz += a.pos.x.hypot(a.pos.z);
                n += 1.0;
                assert!(
                    a.pos.length() <= radius + 1.0e-9,
                    "rock at {:?} is outside the field radius",
                    a.pos
                );
            }
        }
        let (mean_y, mean_xz) = (sum_y / n, sum_xz / n);
        assert!(
            mean_xz > 2.0 * mean_y,
            "field is not flattened: mean |y| {mean_y}, mean horizontal {mean_xz}"
        );
    }

    #[test]
    fn campaign_rocks_stay_in_their_slabs() {
        // Placement may be nudged by the fallback, so the bound is the slab plus
        // the largest clearance a push can add, not the slab exactly.
        let rules = rules();
        let slack = ASTEROID_TIERS[ASTEROID_TIER_COUNT - 1].max_size
            + rules.world.asteroid_field.avoid_margin
            + 1.0;
        let z_min = CAMPAIGN_ASTEROID_ZONES
            .iter()
            .map(|z| z.z_min)
            .fold(f64::INFINITY, f64::min);
        let z_max = CAMPAIGN_ASTEROID_ZONES
            .iter()
            .map(|z| z.z_max)
            .fold(f64::NEG_INFINITY, f64::max);
        for seed in 0..20u64 {
            let field = campaign_field(seed);
            assert_eq!(field.len(), 280);
            for a in &field {
                assert!(
                    a.pos.z >= z_min - slack && a.pos.z <= z_max + slack,
                    "campaign rock at z {}",
                    a.pos.z
                );
                assert!(
                    a.pos.x.abs() <= 130.0 + slack,
                    "campaign rock at x {}",
                    a.pos.x
                );
            }
        }
    }

    // -----------------------------------------------------------------
    // Spin — applied exactly once.
    // -----------------------------------------------------------------

    #[test]
    fn spin_is_scaled_exactly_once() {
        // The campaign generator narrowed its own draw and then let
        // `createAsteroidFieldFromData` narrow it again, landing at 20–40 % of
        // everyone else's rate. Each axis must span the full
        // [-spin_scale/2, spin_scale/2) band.
        let mut max_seen = [0.0f64; ASTEROID_TIER_COUNT];
        let mut field: Vec<Asteroid> = Vec::new();
        for seed in 0..60u64 {
            field.extend(space_field(seed));
            field.extend(campaign_field(seed));
        }
        for a in &field {
            let half = tier_spec(a.tier).spin_scale * 0.5;
            for c in [a.spin.x, a.spin.y, a.spin.z] {
                assert!(
                    c >= -half && c < half,
                    "{:?}: spin {c} outside ±{half} — scaled more than once?",
                    a.tier
                );
                max_seen[a.tier.index()] = max_seen[a.tier.index()].max(c.abs());
            }
        }
        for (i, spec) in ASTEROID_TIERS.iter().enumerate() {
            let half = spec.spin_scale * 0.5;
            // A double-scaled field would top out near `half * spin_scale`,
            // which for every tier is far under 90 % of `half`.
            assert!(
                max_seen[i] > half * 0.9,
                "tier {i}: fastest spin {} is far below the {half} the tier allows \
                 — the scale is being applied more than once",
                max_seen[i]
            );
        }
    }

    #[test]
    fn campaign_and_radial_fields_spin_at_the_same_rate() {
        // The unification, as an assertion: a campaign rock of a given tier
        // turns exactly as fast as a skirmish rock of that tier. Under the JS
        // this ratio was 0.2 on x and y.
        let mut radial = [0.0f64; ASTEROID_TIER_COUNT];
        let mut campaign = [0.0f64; ASTEROID_TIER_COUNT];
        for seed in 0..60u64 {
            for a in space_field(seed) {
                let m = &mut radial[a.tier.index()];
                *m = m.max(a.spin.x.abs().max(a.spin.z.abs()));
            }
            for a in campaign_field(seed) {
                let m = &mut campaign[a.tier.index()];
                *m = m.max(a.spin.x.abs().max(a.spin.z.abs()));
            }
        }
        for i in 0..ASTEROID_TIER_COUNT {
            let ratio = campaign[i] / radial[i];
            assert!(
                (0.9..1.1).contains(&ratio),
                "tier {i}: campaign spins at {ratio}x the radial field",
            );
        }
    }

    // -----------------------------------------------------------------
    // Integration.
    // -----------------------------------------------------------------

    #[test]
    fn rotation_advances_by_spin_times_dt() {
        let mut field = space_field(SEED);
        let before: Vec<(Vec3, Vec3)> = field.iter().map(|a| (a.rot, a.spin)).collect();
        let dt = 1.0 / 60.0;
        integrate(&mut field, dt);
        for (a, (rot0, spin)) in field.iter().zip(before) {
            assert_eq!(a.rot.x.to_bits(), (rot0.x + spin.x * dt).to_bits());
            assert_eq!(a.rot.y.to_bits(), (rot0.y + spin.y * dt).to_bits());
            assert_eq!(a.rot.z.to_bits(), (rot0.z + spin.z * dt).to_bits());
            assert_eq!(a.spin, spin, "spin must not change");
        }
    }

    #[test]
    fn integration_is_reproducible_across_worlds() {
        let mut a = World::new(SEED, rules(), Mode::Skirmish, MapKind::Space);
        let mut b = World::new(SEED, rules(), Mode::Skirmish, MapKind::Space);
        populate(&mut a);
        populate(&mut b);
        for _ in 0..600 {
            step(&mut a, 1.0 / 60.0);
            step(&mut b, 1.0 / 60.0);
        }
        assert_bit_identical(&a.asteroids, &b.asteroids);
    }

    #[test]
    fn hit_flash_decays_at_four_per_second_and_stops_at_zero() {
        let mut field = space_field(SEED);
        field[0].hit_flash = 1.0;
        integrate(&mut field, 0.1);
        assert!((field[0].hit_flash - 0.6).abs() < 1.0e-12);
        for _ in 0..100 {
            integrate(&mut field, 0.1);
        }
        assert_eq!(field[0].hit_flash, 0.0, "the flash must not go negative");
        assert_eq!(field[1].hit_flash, 0.0, "untouched rocks stay dark");
    }

    #[test]
    fn collision_spheres_mirror_the_field_and_reuse_the_buffer() {
        let field = space_field(SEED);
        let mut buf = Vec::new();
        collision_spheres(&field, &mut buf);
        assert_eq!(buf.len(), field.len());
        for (s, a) in buf.iter().zip(&field) {
            assert_eq!(s.center, a.pos);
            assert_eq!(s.radius, a.radius);
        }
        let cap = buf.capacity();
        collision_spheres(&field[..10], &mut buf);
        assert_eq!(buf.len(), 10);
        assert_eq!(buf.capacity(), cap, "the buffer should be reused");
    }

    // -----------------------------------------------------------------
    // Damage and destruction.
    // -----------------------------------------------------------------

    fn damage_world() -> World {
        let mut world = World::new(SEED, rules(), Mode::Skirmish, MapKind::Space);
        populate(&mut world);
        world
    }

    #[test]
    fn a_rock_takes_one_point_per_hit_and_dies_at_zero() {
        let mut world = damage_world();
        let id = world.asteroids[3].id;
        let hp0 = world.asteroids[3].hp;
        let mut events = Vec::new();

        for expected in (1..hp0).rev() {
            let out = apply_damage(&mut world, id, &mut events);
            assert_eq!(out, DamageOutcome::Damaged { hp: expected });
            assert!(out.landed() && !out.destroyed());
        }
        assert_eq!(world.asteroid(id).unwrap().hp, 1);
        assert_eq!(world.asteroid(id).unwrap().hit_flash, 1.0);

        assert!(apply_damage(&mut world, id, &mut events).destroyed());
        assert!(
            world.asteroid(id).is_none(),
            "a dead rock must leave the field"
        );

        // Further hits on a rock that is gone are ignored, not a panic.
        assert_eq!(
            apply_damage(&mut world, id, &mut events),
            DamageOutcome::Ignored
        );
        assert_eq!(
            apply_damage(&mut world, 999_999, &mut events),
            DamageOutcome::Ignored
        );
    }

    #[test]
    fn every_weapon_chips_a_rock_identically() {
        // `main.js:2183` and `server/index.js:813` both subtract exactly 1, so a
        // 50-damage missile and a 10-damage bullet are the same to a rock. Pin
        // it: this is the shipped rule, not an oversight in the port.
        assert_eq!(rules().combat.asteroid_damage_per_hit, 1);
    }

    #[test]
    fn hit_counts_to_destruction_match_the_tier_table() {
        let mut world = damage_world();
        let mut events = Vec::new();
        for tier in [
            AsteroidTier::Small,
            AsteroidTier::Medium,
            AsteroidTier::Big,
            AsteroidTier::Huge,
        ] {
            let rock = world
                .asteroids
                .iter()
                .find(|a| a.tier == tier)
                .unwrap_or_else(|| panic!("no {tier:?} rock in the field"));
            let (id, expected) = (rock.id, tier_hp(tier));
            let mut hits = 0;
            loop {
                hits += 1;
                if apply_damage(&mut world, id, &mut events).destroyed() {
                    break;
                }
                assert!(hits <= expected, "{tier:?} outlived its hit points");
            }
            assert_eq!(hits, expected, "{tier:?}");
        }
    }

    #[test]
    fn destruction_reports_a_break_the_renderer_and_the_audio_can_both_use() {
        let mut world = damage_world();
        let rock = world.asteroids[0];
        let mut events = Vec::new();
        for _ in 0..rock.hp {
            apply_damage(&mut world, rock.id, &mut events);
        }
        // Every hit before the last reports damage only.
        let damaged = events
            .iter()
            .filter(|e| matches!(e, SimEvent::AsteroidDamaged { .. }))
            .count();
        assert_eq!(damaged as i32, rock.hp - 1);

        assert_eq!(
            events[events.len() - 2],
            SimEvent::AsteroidDestroyed {
                id: rock.id,
                pos: rock.pos,
                radius: rock.radius,
            },
            "the destroyed event must come first, so a caller can drop the mesh"
        );
        assert_eq!(
            events[events.len() - 1],
            SimEvent::Explosion {
                pos: rock.pos,
                scale: rock.radius,
                kind: ExplosionKind::AsteroidBreak,
            },
            "the break cue — `rockbreak` at main.js:2189 — is an event, not a sound"
        );
    }

    #[test]
    fn id_zero_is_an_ordinary_asteroid() {
        // Ids are 0-based, so `0` is a real rock. Any truthiness test, sentinel,
        // or `NonZeroU32` would silently make it immortal. Damage it exactly
        // like any other and require identical behaviour.
        let mut world = damage_world();
        assert_eq!(world.asteroids[0].id, 0);
        let zero = world.asteroids[0];
        let one = world.asteroids[1];

        let mut events = Vec::new();
        assert_eq!(
            apply_damage(&mut world, 0, &mut events),
            DamageOutcome::Damaged { hp: zero.hp - 1 }
        );
        assert_eq!(world.asteroid(0).unwrap().hit_flash, 1.0);

        for _ in 1..zero.hp {
            apply_damage(&mut world, 0, &mut events);
        }
        assert!(world.asteroid(0).is_none(), "rock 0 must be destructible");
        // Removing index 0 shifts the rest; ids do not move.
        assert_eq!(world.asteroids[0].id, one.id);

        // And the authoritative paths agree.
        let mut world = damage_world();
        assert!(set_hp(&mut world, 0, 2));
        assert_eq!(world.asteroid(0).unwrap().hp, 2);
        assert!(destroy(&mut world, 0, &mut events));
        assert!(world.asteroid(0).is_none());
        assert!(!destroy(&mut world, 0, &mut events));
    }

    #[test]
    fn the_server_reconciliation_paths_match_the_local_one() {
        // `asteroid-hp` sets hit points and lights the flash (asteroids.js:69),
        // and `asteroid-destroyed` produces the same break as a local kill.
        let mut world = damage_world();
        let rock = world.asteroids[5];
        assert!(set_hp(&mut world, rock.id, 3));
        assert_eq!(world.asteroid(rock.id).unwrap().hp, 3);
        assert_eq!(world.asteroid(rock.id).unwrap().hit_flash, 1.0);
        assert!(!set_hp(&mut world, 999_999, 3));

        // Setting zero does not destroy: the server sends a separate message,
        // and inferring it here would double-fire the break.
        assert!(set_hp(&mut world, rock.id, 0));
        assert!(world.asteroid(rock.id).is_some());

        let mut from_server = Vec::new();
        assert!(destroy(&mut world, rock.id, &mut from_server));

        let mut world2 = damage_world();
        let mut from_local = Vec::new();
        for _ in 0..rock.hp {
            apply_damage(&mut world2, rock.id, &mut from_local);
        }
        assert_eq!(from_server, from_local[from_local.len() - 2..]);
    }

    #[test]
    fn damage_leaves_the_rest_of_the_field_untouched() {
        let mut world = damage_world();
        let before = world.asteroids.clone();
        let victim = before[7];
        let mut events = Vec::new();
        for _ in 0..victim.hp {
            apply_damage(&mut world, victim.id, &mut events);
        }
        assert_eq!(world.asteroids.len(), before.len() - 1);
        let survivors: Vec<Asteroid> = before.into_iter().filter(|a| a.id != victim.id).collect();
        assert_bit_identical(&world.asteroids, &survivors);
    }

    #[test]
    fn tier_lookup_matches_the_table() {
        for (i, spec) in ASTEROID_TIERS.iter().enumerate() {
            let tier = AsteroidTier::from_index(i);
            assert_eq!(tier_spec(tier), spec);
            assert_eq!(tier_hp(tier), spec.hp);
        }
        assert_eq!(tier_hp(AsteroidTier::Small), 5);
        assert_eq!(tier_hp(AsteroidTier::Huge), 50);
    }
}
