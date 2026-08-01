//! Keyboard to [`sim::world::Input`].
//!
//! `Input` is *intent only*: which keys are down, nothing derived. The
//! deadzone, the steering response curve, and the arrow-key ramp are all
//! simulation rules applied inside `ship::integrate`, so this module must not
//! smooth, scale, or filter anything — two clients that pre-processed
//! differently would fly differently.
//!
//! Bindings follow `main.js`.

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseButton};
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use spaceships_sim as sim;

use crate::sim_bridge::{PlayerInput, LOCAL_ID};

/// Collects keyboard and mouse state into [`PlayerInput`] once per rendered
/// frame.
pub struct InputPlugin;

impl Plugin for InputPlugin {
    fn build(&self, app: &mut App) {
        // `PreUpdate` so the input is already current when the fixed loop runs
        // later in the same frame: Bevy's `RunFixedMainLoop` sits between
        // `PreUpdate` and `Update`.
        app.init_resource::<VirtualCursor>()
            .add_systems(PreUpdate, (grab_cursor, gather_input).chain());
    }
}

/// Aim sensitivity multiplier, on top of the DPI correction.
///
/// The browser hands the JS an OS-accelerated delta; winit hands us a raw one,
/// and no constant reproduces an acceleration curve. 1.0 is the literal port —
/// `SPACESHIPS_MOUSE=1.4` and so on for taste.
fn mouse_sensitivity() -> f32 {
    #[cfg(not(target_arch = "wasm32"))]
    if let Ok(v) = std::env::var("SPACESHIPS_MOUSE") {
        if let Ok(f) = v.parse::<f32>() {
            if f.is_finite() && f > 0.0 {
                return f;
            }
        }
    }
    1.0
}

/// The aiming cursor, in pixels from screen centre.
///
/// `input.js:65`–`:73` does **not** treat the mouse as a rate control. It keeps
/// a *virtual cursor position*, accumulating `movementX`/`movementY` and
/// clamping to half the window height, then divides by that half-height to get
/// `steer_x`/`steer_y` in `-1..1`. Steering is therefore proportional to how far
/// the cursor sits from centre, and holding it off-centre holds a turn.
///
/// That is exactly why pointer lock matters: without it the real cursor stops
/// at the screen edge and the turn stops with it.
#[derive(Resource, Default)]
struct VirtualCursor {
    x: f32,
    y: f32,
}

/// Locks the pointer on click and releases it on Escape.
///
/// `main.js` requests pointer lock on the canvas for the same reason. Without
/// a grab the mouse leaves the window mid-turn and aiming simply stops.
fn grab_cursor(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    lobby: Option<Res<crate::ui::LobbyOpen>>,
    mut cursor: Single<&mut CursorOptions, With<PrimaryWindow>>,
) {
    // The lobby navigates by pointer, so it needs a real cursor. Taking the
    // lock on the first click there would hide it and pin it to the centre,
    // which is the opposite of what a menu wants.
    if lobby.is_some_and(|l| l.0) {
        if cursor.grab_mode != CursorGrabMode::None {
            cursor.grab_mode = CursorGrabMode::None;
            cursor.visible = true;
        }
        return;
    }

    if mouse.just_pressed(MouseButton::Left) && cursor.grab_mode == CursorGrabMode::None {
        // `Locked` is the pointer-lock equivalent. macOS supports it; where it
        // is unavailable winit falls back and the cursor stays confined, which
        // is still better than losing it out of the window.
        cursor.grab_mode = CursorGrabMode::Locked;
        cursor.visible = false;
    } else if keys.just_pressed(KeyCode::Escape) {
        cursor.grab_mode = CursorGrabMode::None;
        cursor.visible = true;
    }
}

fn gather_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    window: Single<&Window, With<PrimaryWindow>>,
    lobby: Option<Res<crate::ui::LobbyOpen>>,
    mut virt: ResMut<VirtualCursor>,
    mut out: ResMut<PlayerInput>,
) {
    let axis = |neg: KeyCode, pos: KeyCode| -> f64 {
        f64::from(keys.pressed(pos)) - f64::from(keys.pressed(neg))
    };

    let braking = keys.pressed(KeyCode::Space);

    // Right mouse is free-look in the JS (`main.js:1220`), and while it is held
    // the deltas drive the *camera* rather than the aim — so the virtual cursor
    // must not move, or the nose snaps when the button is released.
    let free_look = mouse.pressed(MouseButton::Right) || keys.pressed(KeyCode::AltLeft);

    // Half the window *height* on both axes, matching `input.js:66` — the
    // horizontal range is deliberately not half the width, so aim sensitivity
    // does not change with aspect ratio.
    let half_h = (window.height() * 0.5).max(1.0);

    // While the lobby is up the pointer is navigating menus, not aiming. The
    // virtual cursor is a *position*, so integrating menu movement into it
    // leaves it pegged at the clamp, and the first frame after launch hands
    // `ship::integrate` a full-deflection stick — the ship snapped into a turn
    // the moment a match started. Recentred rather than merely paused, so a
    // match always begins with the stick neutral.
    if lobby.is_some_and(|l| l.0) {
        virt.x = 0.0;
        virt.y = 0.0;
    }

    // The delta has to be brought into the same units as that clamp, and it is
    // not already there. `AccumulatedMouseMotion` is winit's raw
    // `DeviceEvent::MouseMotion` — **physical** device units — while
    // `Window::height` is logical. On a 2x display that made the mouse twice as
    // sensitive as the browser's `movementX`, which is CSS pixels.
    //
    // Raw also means unaccelerated, where the browser hands the JS a delta the
    // OS has already curved. There is no way to reproduce that exactly, so
    // `SPACESHIPS_MOUSE` scales the result for anyone who wants it heavier or
    // lighter than the 1:1 the JS implies.
    let scale = mouse_sensitivity() / window.scale_factor().max(0.5);
    if !free_look {
        virt.x = (virt.x + motion.delta.x * scale).clamp(-half_h, half_h);
        virt.y = (virt.y + motion.delta.y * scale).clamp(-half_h, half_h);
    }

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
        // Sign matters and was inverted. `main.js:1255`-`:1256` is
        // `KeyD -> roll += RATE`, `KeyA -> roll -= RATE`, so D is positive.
        // `axis(neg, pos)` returns `pos - neg`, which means D belongs in the
        // `pos` slot -- the other way round rolled left when you pressed D.
        roll: axis(KeyCode::KeyA, KeyCode::KeyD),

        // Throttle. W/S is the continuous axis; the mouse wheel's discrete
        // notches and the touch HUD's absolute override are the other two
        // sources, and neither exists here.
        throttle_axis: axis(KeyCode::KeyS, KeyCode::KeyW),

        braking,
        // Brake *and* S together is the hard stop, which swaps `drift_drag`
        // for `drift_brake` (`main.js:1284`).
        hard_brake: braking && keys.pressed(KeyCode::KeyS),
        boost: keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight),
        free_look,

        // Mouse aim. Position of the virtual cursor as a fraction of the
        // half-height, which is `input.js:71`–`:72` exactly. `ship::integrate`
        // applies the deadzone and the `|x|^1.6` response curve, so this stays
        // raw — smoothing it here would make a mouse client fly differently
        // from a keyboard one.
        steer_x: f64::from(virt.x / half_h),
        steer_y: f64::from(virt.y / half_h),

        // One notch per wheel click, sign-only: `input.js:82` is
        // `-Math.sign(e.deltaY)`, so a trackpad's large fractional deltas do
        // not fling the throttle to its stop.
        throttle_notches: f64::from(-scroll.delta.y.signum())
            * f64::from(u8::from(scroll.delta.y != 0.0)),

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
        // `throttle_override` (touch HUD).
        ..Default::default()
    };
}
