//! The seam between the simulation and Bevy.
//!
//! Everything that knows about `spaceships_sim` types lives here or in
//! [`crate::input`]; the rendering modules read only [`SimFrame`], which is the
//! flat `f32` view the simulation was already designed to hand a renderer.
//!
//! # Where the tick comes from
//!
//! `sim::world` defines [`sim::world::TickFn`] as a *type alias* and says
//! plainly that the composition root does not exist yet — the behaviour lives
//! in `ship`, `bullets`, `missiles`, `asteroids`, `bot`, and `campaign`, and
//! nothing calls them in order. [`tick`] below is that call order for the
//! subset this vertical slice renders. It is deliberately in the client and
//! deliberately partial.
//!
//! **This function belongs in `sim::world`, not here.** It is here only because
//! the brief forbids touching `crates/sim`, and a renderer that cannot advance
//! the world cannot be demonstrated at all. When `sim::world::tick` lands, this
//! module should shrink to `let frame = sim::world::tick(..)` and every `TODO`
//! below disappears with it.

use bevy::prelude::*;

use sim::math::Vec3 as SimVec3;
use sim::world::{
    Frame, Input as SimInput, MapKind, Mode, Quat as SimQuat, RockView, ShipFlags, ShipKind,
    ShipView, World as SimWorldState, TICK_DT, TICK_HZ,
};
use spaceships_sim as sim;

/// Match seed. Fixed so every run produces the same asteroid field — the whole
/// point of `sim`'s seeded RNG, and what makes a visual regression obvious.
const SEED: u64 = 0xC0FFEE;

/// The id given to the ship the player flies.
pub const LOCAL_ID: sim::world::EntityId = 1;

/// The authoritative simulation state. Never read by the rendering systems.
#[derive(Resource)]
pub struct SimWorld(pub SimWorldState);

/// The most recent [`Frame`]. This is the *only* thing the renderer reads.
///
/// Kept in a resource and refilled in place rather than returned by value, so
/// that after the first few seconds no tick allocates — see [`Frame::clear`].
///
/// # This is the last *tick*, not the last frame
///
/// It changes at [`TICK_HZ`], which is not the display's rate. Anything that
/// reads a position or an orientation straight out of here and puts it on
/// screen will hold still for two or three frames and then jump — that is
/// exactly the judder [`crate::scene`]'s interpolation exists to remove, and it
/// re-appears in whatever reads around it.
///
/// The rule of thumb: **discrete state** (hit points, ammo, flags, events,
/// scores) is correct to read here, because it has no meaningful in-between
/// value. **Continuous state** — anything a camera, a trail, a nameplate, or a
/// lock-on marker positions itself from — wants the interpolated pose that
/// `scene` writes onto the entity's `Transform`, not `ShipView::pos`.
///
/// > `camera.rs` currently reads `ShipView::pos`/`quat` from here. Its
/// > exponential damping filters most of the resulting staircase out, but not
/// > all of it: measured against this tick rate at 144 Hz, a boosting ship
/// > wobbles about 11 px at 720p against a camera that chases the raw value,
/// > and 0 px against one that chases the interpolated pose. Fixing it means
/// > following the `ShipRoot` entity's `Transform` instead, which is a change
/// > in `camera.rs`.
#[derive(Resource, Default)]
pub struct SimFrame(pub Frame);

/// This frame's player intent, filled by [`crate::input`] and consumed by the
/// fixed tick.
#[derive(Resource, Default)]
pub struct PlayerInput(pub SimInput);

/// System set for the fixed-timestep simulation step, so rendering can order
/// itself after it.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct SimSet;

/// Wires the simulation into the app: one world, one fixed tick at
/// [`TICK_HZ`], one frame buffer.
pub struct SimPlugin;

impl Plugin for SimPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(SimWorld(new_world()))
            .init_resource::<SimFrame>()
            .init_resource::<PlayerInput>()
            // The simulation is fixed-step by contract: variable frame time
            // must never reach it (`sim::world::TickFn`). Bevy's `FixedUpdate`
            // accumulator is exactly the "accumulate real time and run a whole
            // number of ticks" the contract asks for, so the two agree without
            // any bookkeeping of our own.
            //
            // The *remainder* that accumulator carries — `Time<Fixed>`'s
            // `overstep_fraction()` — is what `scene.rs` blends this tick and
            // the last one by. That is the only thing outside this module that
            // depends on the rate, and it depends on it in the one direction
            // that is safe: reading how far through a tick the display is,
            // never feeding anything back.
            .insert_resource(Time::<Fixed>::from_hz(TICK_HZ))
            .add_systems(FixedUpdate, fixed_tick.in_set(SimSet));
    }
}

/// Builds the match this slice renders: a solo skirmish on the space map with a
/// seeded asteroid field and one player ship.
fn new_world() -> SimWorldState {
    let rules = sim::rules::Rules::DEFAULT;

    // `Mode::Skirmish` rather than `Multiplayer` so `authority` is `Local` and
    // the simulation is free to resolve its own damage — there is no server.
    let mut world = SimWorldState::new(SEED, rules, Mode::Skirmish, MapKind::Space);

    // The moon and both motherships came with `World::new`; the field does not.
    // It draws from the dedicated `field` RNG stream, so the layout is stable
    // even as other subsystems are added.
    sim::asteroids::populate(&mut world);

    // Team 0 spawns at -z facing +z, which points the nose at the moon and the
    // field beyond it. `rules.spawn.space_z` is the same 540 the JS uses.
    let spawn = SimVec3::new(0.0, rules.spawn.space_y, -rules.spawn.space_z);
    let mut me =
        sim::world::Ship::spawn(LOCAL_ID, ShipKind::Local, spawn, SimQuat::IDENTITY, &rules);
    me.team = Some(sim::world::Team::Zero);
    world.ships.push(me);
    world.local_id = Some(LOCAL_ID);

    // TODO(bots): `sim::bot` drives `ShipKind::Bot` ships; spawning skirmish
    // allies/enemies here is what turns this into a match. Out of scope for the
    // render slice — nothing about the pipeline changes, the ship list just
    // gets longer.
    world
}

/// Runs one fixed simulation step and refills [`SimFrame`].
fn fixed_tick(mut world: ResMut<SimWorld>, input: Res<PlayerInput>, mut frame: ResMut<SimFrame>) {
    let inputs = [input.0];
    tick(&mut world.0, &inputs, TICK_DT, &mut frame.0);
}

/// One simulation step: the call order `sim::world::tick` will eventually own.
///
/// Ports the subset of the JS frame this slice renders. What is deliberately
/// missing, in the order it would be added back:
///
/// - `sim::bullets` / `sim::missiles` — projectile integration and hit
///   resolution, and the `SimEvent`s they emit.
/// - `sim::bot` — AI-driven ships. Bots synthesize an `Input` and would join
///   the same loop below.
/// - `sim::campaign` — waves, the capital ship, and `Frame::boss`.
/// - Aim assist, which `ship::integrate` documents as composing by
///   premultiplying `Ship::quat` after it returns.
/// - Respawn. `tick_timers` reports `respawn_due`; choosing *where* is
///   mode-dependent and is the caller's job.
pub fn tick(w: &mut SimWorldState, inputs: &[SimInput], dt: f64, out: &mut Frame) {
    // `Rules` and the mode discriminants are `Copy`, so lifting them out of `w`
    // here keeps the disjoint-field borrows below legal.
    let rules = w.rules;
    let mode = w.mode;
    let map = w.map;
    let local_id = w.local_id;

    out.clear();

    {
        // Disjoint-field borrow: the ship list, the geometry lists, and the RNG
        // are separate fields of `World`, which is what lets a `&mut Ship` and
        // `&[Asteroid]` coexist. `ship::WorldGeometry`'s docs call this out as
        // the intended access pattern.
        let SimWorldState {
            ships,
            asteroids,
            obstacles,
            boxes,
            rng,
            ..
        } = &mut *w;

        for s in ships.iter_mut() {
            // Remote ships are interpolated from network state and boss
            // hitboxes are slaved to the capital ship; neither uses the player
            // flight model. See `ship::integrate`.
            if !matches!(s.kind, ShipKind::Local | ShipKind::Bot) {
                continue;
            }

            // A ship with no input coasts on its last state rather than
            // resetting to neutral — the `TickFn` contract is explicit about
            // this, and `Input::default()` happens to be that neutral, so the
            // `unwrap_or_default` is only ever hit by a ship nobody is driving.
            let input = inputs
                .iter()
                .copied()
                .find(|i| i.id == s.id)
                .unwrap_or_default();

            let step = sim::ship::integrate(s, &input, &rules, mode, dt);

            let report = sim::ship::resolve_world_collisions(
                s,
                step.prev_pos,
                sim::ship::WorldGeometry {
                    asteroids,
                    obstacles,
                    boxes,
                    map,
                },
                &rules,
                mode,
                &mut rng.combat,
            );

            // Both self-damage sources are *returned* rather than applied, so
            // that the invulnerability gate and the death/respawn rules stay in
            // one place. Route them through `apply_damage`, which is that
            // place.
            let self_damage = step.self_damage + report.self_damage;
            if self_damage > 0 {
                sim::ship::apply_damage(s, self_damage, &rules, mode);
            }

            sim::ship::tick_timers(s, &rules, dt);

            out.ships.push(ship_view(s, &step, local_id));
        }

        // Cosmetic but shared: spin and hit-flash decay are integrated in the
        // simulation so two clients never see different rocks.
        sim::asteroids::integrate(asteroids, dt);

        for a in asteroids.iter() {
            out.asteroids.push(RockView {
                id: a.id,
                hp: a.hp,
                pos: v3(a.pos),
                rot: v3(a.rot),
                size: a.size as f32,
                hit_flash: a.hit_flash as f32,
            });
        }
    }

    w.time += dt;
    w.tick += 1;
    out.tick = w.tick;
    out.time = w.time;

    // TODO(hud): nothing draws this yet, but filling it costs a handful of
    // divides and proves the HUD's data path is already complete.
    if let Some(me) = w.local_ship() {
        out.hud = sim::world::HudState {
            throttle01: (me.throttle / rules.ship.max_throttle) as f32,
            speed: me.vel.length() as f32,
            hp: me.hp,
            hp01: me.hp as f32 / rules.ship.max_hp as f32,
            ammo01: (me.ammo / rules.weapons.max_ammo) as f32,
            boost01: (me.boost_meter / rules.ship.max_boost) as f32,
            charge01: me.brake_charge as f32,
            missiles: me.missiles_left,
            flares: me.flares_left,
            gun_mode: me.gun_mode,
            invuln: me.invuln_timer > 0.0,
            assist_target: -1,
            match_timer: w.match_state.timer as f32,
            team_kills: w.match_state.team_kills,
            ..Default::default()
        };
    }
}

/// Narrows one `Ship` to the renderer's view of it.
fn ship_view(s: &sim::world::Ship, step: &sim::ship::FlightStep, local: Option<i32>) -> ShipView {
    ShipView {
        id: s.id,
        team: s.team.map_or(-1, |t| t.index() as i32),
        hp: s.hp,
        flags: ShipFlags::NONE
            .with_if(s.alive, ShipFlags::ALIVE)
            .with_if(step.boosting, ShipFlags::BOOSTING)
            .with_if(step.braking, ShipFlags::BRAKING)
            .with_if(s.invuln_timer > 0.0, ShipFlags::INVULN)
            .with_if(matches!(s.kind, ShipKind::Bot), ShipFlags::BOT)
            .with_if(Some(s.id) == local, ShipFlags::LOCAL)
            .with_if(
                matches!(s.kind, ShipKind::BossHitbox),
                ShipFlags::BOSS_HITBOX,
            ),
        pos: v3(s.pos),
        quat: [
            s.quat.x as f32,
            s.quat.y as f32,
            s.quat.z as f32,
            s.quat.w as f32,
        ],
        vel: v3(s.vel),
        hit_flash: s.hit_flash as f32,
    }
}

/// The one-way `f64` -> `f32` narrowing at the render boundary. `Frame` values
/// are never read back into `World`, so the lost precision cannot feed the
/// simulation.
fn v3(v: SimVec3) -> [f32; 3] {
    [v.x as f32, v.y as f32, v.z as f32]
}

// ---------------------------------------------------------------------------
// Frame -> Bevy conversions
// ---------------------------------------------------------------------------

/// `Frame` position to a Bevy translation.
///
/// No axis remapping: `sim` inherited Three.js's right-handed Y-up convention
/// with the ship nose along local `+z`, and Bevy uses the same handedness and
/// the same up axis. The only place the two differ is the glTF model's own
/// resting orientation, which `scene.rs` corrects exactly where `ship.js` does.
#[inline]
pub fn pos(p: [f32; 3]) -> Vec3 {
    Vec3::new(p[0], p[1], p[2])
}

/// `Frame` orientation to a Bevy rotation. Both are `(x, y, z, w)`.
#[inline]
pub fn rot(q: [f32; 4]) -> Quat {
    Quat::from_xyzw(q[0], q[1], q[2], q[3])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One second of ticks with `input` applied.
    fn run(w: &mut SimWorldState, input: SimInput, secs: f64) -> Frame {
        let mut frame = Frame::new();
        for _ in 0..(secs * TICK_HZ) as u32 {
            tick(w, &[input], TICK_DT, &mut frame);
        }
        frame
    }

    fn throttle_up() -> SimInput {
        SimInput {
            id: LOCAL_ID,
            throttle_axis: 1.0,
            ..Default::default()
        }
    }

    #[test]
    fn the_world_starts_with_a_ship_and_a_field() {
        let w = new_world();
        assert_eq!(w.ships.len(), 1);
        assert_eq!(w.local_id, Some(LOCAL_ID));
        assert_eq!(
            w.asteroids.len(),
            w.rules.world.asteroid_field.count as usize
        );
        // `World::new` owns this, not the field generator.
        assert_eq!(w.obstacles.len(), 1, "the moon");
    }

    /// The requirement this whole module exists for: a key press reaches the
    /// flight model and moves the ship.
    #[test]
    fn throttle_input_moves_the_ship_forward() {
        let mut w = new_world();
        let start = w.local_ship().expect("local ship").pos;

        let frame = run(&mut w, throttle_up(), 2.0);

        let end = w.local_ship().unwrap().pos;
        assert!(
            end.distance(start) > 1.0,
            "two seconds of throttle should move the ship, went {:?} -> {:?}",
            start,
            end
        );
        // Team 0 spawns at -z facing +z, so "forward" is +z.
        assert!(end.z > start.z, "the nose is local +z");

        // And the renderer sees it.
        assert_eq!(frame.ships.len(), 1);
        assert_eq!(frame.ships[0].pos[2], end.z as f32);
        assert!(frame.hud.speed > 0.0);
    }

    #[test]
    fn no_input_leaves_the_ship_at_rest() {
        let mut w = new_world();
        let start = w.local_ship().unwrap().pos;
        run(&mut w, SimInput::default(), 1.0);
        assert_eq!(w.local_ship().unwrap().pos, start);
    }

    /// The frame is rebuilt from scratch each tick and must not accumulate.
    #[test]
    fn the_frame_does_not_grow() {
        let mut w = new_world();
        let mut frame = Frame::new();
        tick(&mut w, &[throttle_up()], TICK_DT, &mut frame);
        let first = frame.entity_count();
        for _ in 0..120 {
            tick(&mut w, &[throttle_up()], TICK_DT, &mut frame);
        }
        assert_eq!(frame.entity_count(), first);
        assert_eq!(frame.tick, 121);
    }

    /// The seed is fixed so the field is reproducible — that is the entire
    /// point of `sim`'s per-stream RNG, and a visual regression is only
    /// meaningful if the scene is the same scene.
    #[test]
    fn the_field_is_deterministic() {
        let a = new_world();
        let b = new_world();
        assert_eq!(a.asteroids, b.asteroids);
    }
}
