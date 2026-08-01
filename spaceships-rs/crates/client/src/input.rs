//! Keyboard to [`sim::world::Input`].
//!
//! `Input` is *intent only*: which keys are down, nothing derived. The
//! deadzone, the steering response curve, and the arrow-key ramp are all
//! simulation rules applied inside `ship::integrate`, so this module must not
//! smooth, scale, or filter anything — two clients that pre-processed
//! differently would fly differently.
//!
//! Bindings follow `main.js`.

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

fn gather_input(keys: Res<ButtonInput<KeyCode>>, mut out: ResMut<PlayerInput>) {
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
        // Q is fine-aim *and* the flare key in the JS; flares win there, so
        // fine-aim gets left-control here.
        arrow_fine: keys.pressed(KeyCode::ControlLeft),

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

        // Edge-triggered actions. `just_pressed` is exactly the debounce the JS
        // input layer does by hand (`main.js:1390`, `:1446`, `:1034`).
        //
        // TODO(weapons): the simulation's `Input` accepts these today, but
        // nothing consumes them yet — `sim::bullets` and `sim::missiles` are
        // not in `sim_bridge::tick`, so these currently go nowhere. They are
        // wired anyway so the binding table is complete in one place.
        fire: keys.pressed(KeyCode::KeyF),
        fire_missile: keys.just_pressed(KeyCode::KeyE),
        deploy_flare: keys.just_pressed(KeyCode::KeyQ),
        toggle_gun: keys.just_pressed(KeyCode::KeyP),
        toggle_aim_assist: keys.just_pressed(KeyCode::KeyT),

        // Unset, and named here so the list above is visibly exhaustive:
        // `steer_x`/`steer_y` (mouse), `throttle_notches` (wheel),
        // `throttle_override` (touch HUD).
        ..Default::default()
    };
}
