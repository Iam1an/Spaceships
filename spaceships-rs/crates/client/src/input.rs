//! Keyboard to [`sim::world::Input`].
//!
//! `Input` is *intent only*: which keys are down, nothing derived. The
//! deadzone, the steering response curve, and the arrow-key ramp are all
//! simulation rules applied inside `ship::integrate`, so this module must not
//! smooth, scale, or filter anything — two clients that pre-processed
//! differently would fly differently.
//!
//! Bindings follow `main.js`.

use bevy::input::mouse::MouseButton;
use bevy::prelude::*;
use spaceships_sim as sim;

use crate::sim_bridge::{PlayerInput, LOCAL_ID};

/// Collects keyboard state into [`PlayerInput`] once per rendered frame.
pub struct InputPlugin;

impl Plugin for InputPlugin {
    fn build(&self, app: &mut App) {
        // `PreUpdate` so the input is already current when the fixed loop runs
        // later in the same frame: Bevy's `RunFixedMainLoop` sits between
        // `PreUpdate` and `Update`.
        app.add_systems(PreUpdate, gather_input);
    }
}

fn gather_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut out: ResMut<PlayerInput>,
) {
    let axis = |neg: KeyCode, pos: KeyCode| -> f64 {
        f64::from(keys.pressed(pos)) - f64::from(keys.pressed(neg))
    };

    let braking = keys.pressed(KeyCode::Space);

    out.0 = sim::world::Input {
        id: LOCAL_ID,

        // Steering. `steer_x`/`steer_y` are the *mouse* axes; the keyboard path
        // is `arrow_x`/`arrow_y`, which `ship::integrate` ramps rather than
        // applying directly, so arrow keys are not strictly worse than a mouse
        // (`main.js:992`). Negative `arrow_y` is nose-up, so Up must be -1 —
        // hence Up in the `neg` slot.
        arrow_x: axis(KeyCode::ArrowLeft, KeyCode::ArrowRight),
        arrow_y: axis(KeyCode::ArrowUp, KeyCode::ArrowDown),
        // Q is fine-aim *and* the flare key, and in the JS it is genuinely
        // both at once rather than one winning: `main.js:1240` reads it held
        // for the slower arrow ramp, `:1108` reads its rising edge for the
        // flare. Left-control is kept as a second fine-aim binding for anyone
        // who would rather not slow their turn every time they pop a decoy.
        arrow_fine: keys.pressed(KeyCode::KeyQ) || keys.pressed(KeyCode::ControlLeft),

        // Roll. `main.js:1249`.
        roll: axis(KeyCode::KeyD, KeyCode::KeyA),

        // Throttle. W/S is the continuous axis; the mouse wheel's discrete
        // notches and the touch HUD's absolute override are the other two
        // sources, and neither exists here.
        throttle_axis: axis(KeyCode::KeyS, KeyCode::KeyW),

        braking,
        // Brake *and* S together is the hard stop, which swaps `drift_drag`
        // for `drift_brake` (`main.js:1284`).
        hard_brake: braking && keys.pressed(KeyCode::KeyS),
        boost: keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight),
        free_look: keys.pressed(KeyCode::AltLeft),

        // Weapons.
        //
        // `fire` is held, not edged: the gun's rate of fire is a *rule*
        // (`weapons.gun_cooldown`), applied by `sim::bullets::fire_gun`, so
        // the client's job is to report the trigger down and nothing more.
        // `main.js:1490` is the same test — `input.lmb || keys.has('KeyF') ||
        // gp.fire` — and left mouse is the primary binding, F the fallback for
        // anyone flying on the keyboard scheme.
        //
        // The rest are edge-triggered, and `just_pressed` is exactly the
        // `prevKeyE`/`prevKeyQ`/`prevKeyP` debounce the JS keeps by hand
        // (`main.js:1415`, `:1108`, `:1380`).
        //
        // Note that these still go nowhere in this build: `sim_bridge::tick`
        // does not call `sim::bullets` or `sim::missiles`, so nothing consumes
        // them and `Frame`'s projectile lists stay empty. The bindings are
        // complete here so that the day the projectile phases land in the
        // bridge, no input work is left to do — and so the whole binding table
        // is legible in one place rather than half-here and half-pending.
        fire: keys.pressed(KeyCode::KeyF) || mouse.pressed(MouseButton::Left),
        fire_missile: keys.just_pressed(KeyCode::KeyE),
        deploy_flare: keys.just_pressed(KeyCode::KeyQ),
        toggle_gun: keys.just_pressed(KeyCode::KeyP),
        // C, not T: `main.js:1385` (`prevKeyC`). T is not bound to anything in
        // the JS at all.
        toggle_aim_assist: keys.just_pressed(KeyCode::KeyC),

        // Unset, and named here so the list above is visibly exhaustive:
        // `steer_x`/`steer_y` (mouse), `throttle_notches` (wheel),
        // `throttle_override` (touch HUD).
        ..Default::default()
    };
}
