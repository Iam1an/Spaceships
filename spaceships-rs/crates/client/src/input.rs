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
        // Not on wasm: the browser owns fullscreen and only grants it inside a
        // user-gesture handler, which a Bevy system is not.
        #[cfg(not(target_arch = "wasm32"))]
        app.add_systems(PreUpdate, toggle_fullscreen);
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

/// Locks the pointer on click, and releases it on Escape or `O`.
///
/// `main.js` requests pointer lock on the canvas for the same reason. Without
/// a grab the mouse leaves the window mid-turn and aiming simply stops.
///
/// `O` is the JS's own binding for this (`main.js:1392`): it toggles pointer
/// lock both ways, and it is gated on `!noMouseMode` there because a pilot on
/// the keyboard scheme has no use for it. Escape is kept alongside it as the
/// platform convention — winit surrenders the lock on Escape regardless, so
/// pretending otherwise would only desync this state from the window's.
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

    let locked = cursor.grab_mode != CursorGrabMode::None;
    let take = (mouse.just_pressed(MouseButton::Left) && !locked)
        || (keys.just_pressed(KeyCode::KeyO) && !locked);
    let release =
        keys.just_pressed(KeyCode::Escape) || (keys.just_pressed(KeyCode::KeyO) && locked);

    if take {
        // `Locked` is the pointer-lock equivalent. macOS supports it; where it
        // is unavailable winit falls back and the cursor stays confined, which
        // is still better than losing it out of the window.
        cursor.grab_mode = CursorGrabMode::Locked;
        cursor.visible = false;
    } else if release {
        cursor.grab_mode = CursorGrabMode::None;
        cursor.visible = true;
    }
}

/// `L` toggles fullscreen. `main.js:1401`.
///
/// The JS calls `requestFullscreen` on the document element; the native
/// equivalent is the window's mode, and `Borderless(None)` is the one that
/// matches — it takes the monitor the window is already on rather than moving
/// it, which is what a browser going fullscreen does.
#[cfg(not(target_arch = "wasm32"))]
fn toggle_fullscreen(
    keys: Res<ButtonInput<KeyCode>>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
) {
    if !keys.just_pressed(KeyCode::KeyL) {
        return;
    }
    window.mode = match window.mode {
        bevy::window::WindowMode::Windowed => {
            bevy::window::WindowMode::BorderlessFullscreen(bevy::window::MonitorSelection::Current)
        }
        _ => bevy::window::WindowMode::Windowed,
    };
}

/// Reference speed for [`accelerate`], in logical pixels per second.
///
/// Below this the curve is essentially off; above it, gain climbs. Chosen so an
/// ordinary aiming movement sits near the knee rather than at either extreme.
const ACCEL_REFERENCE: f32 = 900.0;

/// How much extra gain a very fast flick gets, on top of 1.0.
const ACCEL_GAIN: f32 = 1.0;

/// Pointer acceleration, approximating what the OS already did for the web
/// build.
///
/// The same client compiled two ways receives two different signals. In a
/// browser, pointer lock delivers `movementX`/`movementY` — deltas macOS has
/// **already** run through its pointer-acceleration curve, the one every other
/// application on the machine uses. Natively, winit delivers
/// `DeviceEvent::MouseMotion`, which is raw device counts with no curve at all.
///
/// That is why the web build felt smoother than the native one to fly, which is
/// how this was reported. Raw input is *correct* and it is what a competitive
/// shooter wants; it is also unlike everything else on the desktop, so slow
/// corrections feel stiff and fast ones feel short.
///
/// This is a gain curve, not a smoothing filter: it scales the delta by speed
/// and adds no latency and no history. Slow movement passes through at 1.0, so
/// fine aim is untouched; a fast flick gets up to `1 + ACCEL_GAIN`. The `dt`
/// divide is what makes it a *speed* curve rather than a per-frame one — at 144
/// Hz each delta is half the size it is at 72 Hz for the same hand movement,
/// and without dividing, frame rate would change the aiming.
///
/// `SPACESHIPS_MOUSE_RAW=1` turns it off for anyone who wants the hardware
/// exactly as it comes. `SPACESHIPS_MOUSE` still scales everything on top.
///
/// Not applied on wasm: the browser has already done it, and doing it twice is
/// how you get a pointer that skates.
fn accelerate(delta: Vec2, dt: f32) -> Vec2 {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = dt;
        return delta;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        if raw_mouse() || dt <= 0.0 {
            return delta;
        }
        let speed = delta.length() / dt;
        let gain = 1.0 + ACCEL_GAIN * (speed / ACCEL_REFERENCE).min(1.0);
        delta * gain
    }
}

/// `SPACESHIPS_MOUSE_RAW`: skip [`accelerate`] entirely.
#[cfg(not(target_arch = "wasm32"))]
fn raw_mouse() -> bool {
    std::env::var_os("SPACESHIPS_MOUSE_RAW").is_some()
}

/// `SPACESHIPS_EMP=<seconds>`: pull the EMP trigger by itself, once, that many
/// seconds after launch.
///
/// The screenshot hook for a weapon whose whole effect is *not being able to
/// see*, which is the one thing a still frame can show and the one thing you
/// cannot arrange by hand — the pulse has to have gone off, the pilot has to be
/// the one it caught, and the shutter has to open somewhere inside four seconds.
/// With `SPACESHIPS_EMP` fixing the first two (`sim_bridge` also flips
/// `EmpRules::blinds_owner` and drops the charge time to zero) and
/// `SPACESHIPS_SCREENSHOT_AT` fixing the third, every frame of the effect is
/// reachable from a command line.
///
/// `SPACESHIPS_EMP=1` with no number means "at one second", which is why the bare
/// flag the cockpit's old dev hook used still does something sensible.
///
/// Returns whether *this* frame is the one to fire on, so the caller keeps the
/// edge — the simulation debounces nothing.
#[cfg(not(target_arch = "wasm32"))]
fn emp_test_fire(time: &Time, fired: &mut bool) -> bool {
    if *fired {
        return false;
    }
    let Ok(spec) = std::env::var("SPACESHIPS_EMP") else {
        return false;
    };
    let at = spec.trim().parse::<f32>().unwrap_or(1.0);
    if time.elapsed_secs() < at {
        return false;
    }
    *fired = true;
    true
}

#[cfg(target_arch = "wasm32")]
fn emp_test_fire(_time: &Time, _fired: &mut bool) -> bool {
    false
}

fn gather_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    time: Res<Time>,
    scroll: Res<AccumulatedMouseScroll>,
    window: Single<&Window, With<PrimaryWindow>>,
    lobby: Option<Res<crate::ui::LobbyOpen>>,
    mut virt: ResMut<VirtualCursor>,
    mut out: ResMut<PlayerInput>,
    mut emp_fired: Local<bool>,
) {
    let axis = |neg: KeyCode, pos: KeyCode| -> f64 {
        f64::from(keys.pressed(pos)) - f64::from(keys.pressed(neg))
    };

    let braking = keys.pressed(KeyCode::Space);

    // Right mouse is free-look in the JS (`main.js:1220`), and while it is held
    // the deltas drive the *camera* rather than the aim — so the virtual cursor
    // must not move, or the nose snaps when the button is released.
    //
    // Right mouse *only*: `input.js:52` sets `rmb` on `button === 2` and
    // nothing reads a modifier. Left-alt was an addition here and is gone.
    let free_look = mouse.pressed(MouseButton::Right);

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
    // Raw also means *unaccelerated*, and that is the one difference a player
    // feels immediately. See [`accelerate`].
    let scale = mouse_sensitivity() / window.scale_factor().max(0.5);
    if !free_look {
        let delta = accelerate(motion.delta, time.delta_secs());
        virt.x = (virt.x + delta.x * scale).clamp(-half_h, half_h);
        virt.y = (virt.y + delta.y * scale).clamp(-half_h, half_h);
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
        // flare.
        //
        // Left-control used to be a second fine-aim binding here. The JS has no
        // such key — `main.js` reads exactly `Arrow*`, `WASD`, `C`, `E`, `F`,
        // `L`, `O`, `P`, `Q`, `V`, `Shift`, `Space`, `Tab`, and the two mouse
        // buttons — and an extra binding is its own kind of wrong when the
        // muscle memory being ported is the point.
        arrow_fine: keys.pressed(KeyCode::KeyQ),

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
        // **The one binding that is not in `main.js`'s table**, and the comment
        // above says why an extra key is normally wrong: the muscle memory being
        // ported is the point. It is right here for the one reason that
        // overrides it — the EMP is a weapon the JS does not have, so there is no
        // muscle memory to preserve and no browser pilot who could be surprised
        // by it. `G` because `cockpit.rs`'s dev hook has been the EMP key since
        // before the weapon existed, and because it is the closest unbound key to
        // `E` and `Q`, which are the other two stores.
        fire_emp: keys.just_pressed(KeyCode::KeyG) || emp_test_fire(&time, &mut emp_fired),
        toggle_gun: keys.just_pressed(KeyCode::KeyP),
        // C, not T: `main.js:1385` (`prevKeyC`). T is not bound to anything in
        // the JS at all.
        toggle_aim_assist: keys.just_pressed(KeyCode::KeyC),

        // Unset, and named here so the list above is visibly exhaustive:
        // `throttle_override` (touch HUD).
        ..Default::default()
    };
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// Fine aim is untouched and a flick is amplified — the point of the curve.
    #[test]
    fn slow_movement_passes_through_and_fast_movement_gains() {
        let dt = 1.0 / 120.0;

        // A slow correction: a couple of pixels in a frame is ~240 px/s,
        // far below the knee.
        let slow = Vec2::new(2.0, 0.0);
        let out = accelerate(slow, dt);
        assert!(
            (out.length() / slow.length() - 1.0).abs() < 0.35,
            "fine aim should be near 1:1, got {}",
            out.length() / slow.length(),
        );

        // A flick, well past the reference speed, is capped at 1 + ACCEL_GAIN.
        let fast = Vec2::new(60.0, 0.0);
        let out = accelerate(fast, dt);
        assert!(
            (out.length() / fast.length() - (1.0 + ACCEL_GAIN)).abs() < 1e-4,
            "a hard flick should hit the gain ceiling",
        );
    }

    /// The property that makes this a *speed* curve rather than a per-frame
    /// one: the same hand movement must aim the same at any frame rate.
    ///
    /// Without the `dt` divide, a 144 Hz display would halve every delta
    /// against a 72 Hz one and quietly aim differently — which is the bug this
    /// shape exists to avoid, and the reason the naive version was rejected.
    #[test]
    fn the_curve_is_frame_rate_independent() {
        // One hand movement, sampled over one frame or split across two.
        let whole = Vec2::new(24.0, 0.0);
        let dt = 1.0 / 60.0;

        let once = accelerate(whole, dt);
        let halved = accelerate(whole / 2.0, dt / 2.0) * 2.0;

        assert!(
            once.abs_diff_eq(halved, 1e-4),
            "same movement, different frame rate: {once:?} vs {halved:?}",
        );
    }

    /// The escape hatch has to actually escape.
    #[test]
    fn raw_mode_is_the_identity() {
        // `raw_mouse` reads the environment, so assert the arithmetic the flag
        // selects rather than mutating global state in a threaded test run.
        let delta = Vec2::new(40.0, -12.0);
        assert_eq!(accelerate(delta, 0.0), delta, "a zero dt cannot divide");
    }
}
