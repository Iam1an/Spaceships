//! Bevy renderer for the Spaceships simulation.
//!
//! A vertical slice: it proves the whole pipeline — `sim` state, fixed
//! timestep, `Frame` handoff, glTF assets, a render backend on two very
//! different targets — end to end, and nothing more. It draws the player's ship
//! and the seeded asteroid field, and lets you fly. Projectiles, bots, the HUD,
//! audio, networking, the campaign, and the first-person cockpit are all out of
//! scope and are marked with `TODO` where they attach.
//!
//! # Layout
//!
//! - [`sim_bridge`] — the only module that knows `spaceships_sim`'s state
//!   types. Owns the `World`, runs the fixed tick, publishes a `Frame`.
//! - [`input`] — keyboard to `sim::world::Input`. Intent only.
//! - [`scene`] — static geometry, and the id-keyed sync from `Frame` to
//!   transforms.
//! - [`camera`] — the chase camera from `public/src/camera.js`.
//! - [`skybox`] — the procedural starfield from `public/src/skybox.js`.
//! - [`terrain`] — the Sierras map: ground, sea, bases, trees, boulders, clouds
//!   and fog, with the ground drawn one triangle per `sim::terrain` lattice face
//!   so what you see is exactly what kills you.
//!
//! The dependency direction is one-way: `scene`, `camera`, and `skybox` read
//! `Frame` and never touch `World`.
//!
//! # Draw calls, and the bottleneck not to reproduce
//!
//! Profiling the JS client found its real cost is one mesh per entity — 477
//! draw calls at p99 — so the scene here is built to share handles from the
//! start rather than to be fixed later:
//!
//! - **Asteroids** are 60 entities sharing **6 mesh handles and 1 material**.
//!   Bevy batches by (mesh, material) and, with GPU preprocessing on, draws
//!   them as a handful of indirect calls rather than 60. This is the case the
//!   JS gets wrong: `asteroids.js` clones a material per rock so it can tint
//!   damage, which breaks batching for all sixty. Keeping one material is why
//!   the damage tint is a `TODO` pointing at a material extension with a
//!   per-instance uniform instead of a clone.
//! - **Ships** are one glTF instance each, which is correct — there are at most
//!   ten and they carry distinct hulls.
//! - **Bullets, missiles, flares, and trails are not here yet, and are the ones
//!   that matter.** `ProjView`/`FlareView` are 32-byte records with a stable
//!   `key`, and there can be hundreds in flight. A mesh per bolt reproduces the
//!   JS bottleneck exactly. The shape that does not: one entity, one quad or
//!   capsule mesh, and a per-instance data buffer — Bevy's material extension
//!   plus a storage buffer indexed by instance, or a single mesh rebuilt each
//!   frame from the `Frame` slices. Trails are the same problem with more
//!   vertices; `trails.js` already keeps a ring buffer per ship, which maps to
//!   one dynamic mesh per ship rather than one per segment.
//!
//! The other profiling finding — O(entities x asteroids) collision scans with
//! no broad phase — is a **simulation** problem, not a render one, and it lives
//! in `crates/sim`. It is visible from here: [`sim_bridge::tick`] calls
//! `ship::resolve_world_collisions`, which walks every asteroid for every ship,
//! and `World::asteroid()` is a linear scan. `sim::collision` already has the
//! swept primitives a broad phase would feed; what is missing is the grid or
//! BVH in front of them. Nothing in this crate can fix that, and nothing in
//! this crate should try.
//!
//! # Running
//!
//! ```text
//! cargo run -p spaceships-client --release        # native (Metal)
//! crates/client/build-wasm.sh                     # web (WebGL2)
//! ```
//!
//! Controls: `W`/`S` throttle, arrows steer, `A`/`D` roll, `Space` brake/drift,
//! `Shift` boost.

mod api;
mod audio;
mod camera;
mod cockpit;
mod hud;
mod input;
mod net;
mod scene;
mod sim_bridge;
mod skybox;
mod terrain;
mod ui;
mod warp;
mod weapons;

use core::time::Duration;

use bevy::asset::AssetPlugin;
use bevy::platform::time::Instant;
use bevy::prelude::*;
use bevy::window::{Window, WindowPlugin};

pub use sim_bridge::LOCAL_ID;

/// Where Bevy looks for `spaceship.glb`, `moon Texture.jpg` and
/// `sounds/asteroid.jpg`.
///
/// The assets live in the repo's `public/`, shared with the Three.js client,
/// and are deliberately not duplicated into a `crates/client/assets/`.
///
/// Web: a URL path relative to the page. `build-wasm.sh` copies the assets it
/// needs into `web/assets/`.
#[cfg(target_arch = "wasm32")]
const ASSET_ROOT: &str = "assets";

/// The repo checkout this binary was compiled from, as an absolute path.
///
/// The development answer, and it used to be the *only* answer. Bevy resolves a
/// relative `file_path` against `CARGO_MANIFEST_DIR` under `cargo run` but
/// against the executable's directory otherwise, and those are different
/// places; baking an absolute path removes the whole class of "why is the model
/// invisible" failure while you are working.
///
/// It also makes the binary non-relocatable, which this comment used to note
/// was "the first thing to change if this ever ships". See [`asset_root`].
#[cfg(not(target_arch = "wasm32"))]
const DEV_ASSET_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../public");

/// Where to load assets from, decided at startup rather than at compile time.
///
/// A packaged build has to find its own assets wherever the user dropped the
/// app, so the baked path is now the *last* resort rather than the only one.
/// In order:
///
/// 1. `SPACESHIPS_ASSETS`, for running a packaged binary against a working
///    copy — or for anyone who wants to swap the ship models out.
/// 2. `../Resources/assets` relative to the executable, which is where a macOS
///    `.app` bundle puts them: the binary lives in `Contents/MacOS/`.
/// 3. `assets` beside the executable, the plain-directory layout — a zip
///    someone unpacked, or a Windows build.
/// 4. [`DEV_ASSET_ROOT`], so `cargo run` from a checkout keeps working exactly
///    as it did.
///
/// Each candidate is probed with `is_dir` rather than assumed, so a bundle
/// missing its `Resources` falls through to something that works instead of
/// starting up with every texture silently absent.
#[cfg(not(target_arch = "wasm32"))]
fn asset_root() -> String {
    if let Some(explicit) = std::env::var_os("SPACESHIPS_ASSETS") {
        return explicit.to_string_lossy().into_owned();
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for candidate in ["../Resources/assets", "assets"] {
                let path = dir.join(candidate);
                if path.is_dir() {
                    return path.to_string_lossy().into_owned();
                }
            }
        }
    }
    DEV_ASSET_ROOT.to_owned()
}

/// The web has one answer and no filesystem to probe.
#[cfg(target_arch = "wasm32")]
fn asset_root() -> String {
    ASSET_ROOT.to_owned()
}

fn main() {
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(AssetPlugin {
                file_path: asset_root(),
                ..default()
            })
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "Spaceships".into(),
                    // Both canvas fields are no-ops off the web. The
                    // selector must match `web/index.html`.
                    canvas: Some("#game".into()),
                    fit_canvas_to_parent: true,
                    // Let the browser keep F5, devtools, and tab switching;
                    // the game only reads WASD/arrows/space/shift.
                    prevent_default_event_handling: false,
                    // Vsync pins the frame time to the display's refresh
                    // interval, which makes `report_frame_time` a readout
                    // of the monitor rather than of the renderer. Set
                    // `SPACESHIPS_NO_VSYNC=1` to measure how much headroom
                    // there actually is.
                    // Caveat found the hard way: on macOS/Metal wgpu has no
                    // immediate present mode, so `AutoNoVsync` falls back
                    // to Fifo and this does nothing. Frame times there
                    // quantise to multiples of the refresh interval, which
                    // tells you which vsync bucket you are in but not what
                    // the frame actually costs. `SPACESHIPS_RES` is the
                    // way round it: shrink the target until the frame
                    // drops a bucket.
                    present_mode: if std::env::var_os("SPACESHIPS_NO_VSYNC").is_some() {
                        bevy::window::PresentMode::AutoNoVsync
                    } else {
                        bevy::window::PresentMode::AutoVsync
                    },
                    resolution: window_resolution(),

                    // A grey title bar over a space game looks like a document
                    // window. These three make it disappear into the scene:
                    // `fullsize_content_view` extends the render target up
                    // under the bar so there is something behind it,
                    // `titlebar_transparent` stops it painting grey, and the
                    // title text goes since it would sit over the sky.
                    //
                    // The traffic lights stay — without them the window cannot
                    // be closed or minimised by mouse, and they float over the
                    // scene, which is the intended look. Note this means the
                    // top-left ~80x30 px of the viewport sits under them, so
                    // nothing important should be drawn there.
                    //
                    // macOS-only in effect; the fields exist on every platform
                    // and are ignored elsewhere, so no `cfg` is needed.
                    fullsize_content_view: true,
                    titlebar_transparent: true,
                    titlebar_show_title: false,

                    ..default()
                }),
                ..default()
            }),
    )
    .add_plugins((
        sim_bridge::SimPlugin,
        input::InputPlugin,
        scene::ScenePlugin,
        camera::FollowCameraPlugin,
        skybox::SkyboxPlugin,
        terrain::TerrainPlugin,
        audio::AudioPlugin,
        hud::HudPlugin,
        net::NetPlugin,
        api::ApiPlugin,
        weapons::WeaponsPlugin,
        cockpit::CockpitPlugin,
        ui::UiPlugin,
        warp::WarpPlugin,
    ))
    .init_resource::<FrameCost>()
    // `First` opens the window and `Last` closes it, so `busy` is the main
    // world's own work and nothing else. The readout goes in `Last` too, so it
    // reports the window it just closed rather than the previous one.
    .add_systems(First, open_frame)
    .add_systems(Last, report_frame_time);

    #[cfg(not(target_arch = "wasm32"))]
    app.add_systems(Update, screenshot_on_f12);

    app.run();
}

/// When [`screenshot_on_f12`] opens the shutter, in seconds since launch.
///
/// Three by default: long enough for the glTF and both textures to have loaded
/// and for the camera damping to have settled.
///
/// `SPACESHIPS_SHOT_AT` moves it, which is what makes a *timed* effect
/// photographable at all. The EMP is the case that needed it — the pulse fires,
/// four seconds of blackout run, and then the cockpit reboots over another
/// six-tenths, so "detonation", "blind" and "recovery" are three different
/// moments of the same run and no fixed shutter can reach all three. Paired with
/// `SPACESHIPS_EMP=<seconds>`, which decides when the trigger is pulled, any
/// frame of the effect is one command line away.
#[cfg(not(target_arch = "wasm32"))]
fn screenshot_at() -> f32 {
    std::env::var("SPACESHIPS_SHOT_AT")
        .ok()
        .and_then(|s| s.trim().parse::<f32>().ok())
        .filter(|s| s.is_finite() && *s >= 0.0)
        .unwrap_or(3.0)
}

/// Writes the exact render target to a PNG, on `F12` or on a timer.
///
/// Native only — it writes to disk. Worth the dozen lines: a hand-cropped grab
/// of the window tells you nothing reliable about where the frame's *centre*
/// is, which is precisely the question you have when a camera looks subtly
/// wrong. `SPACESHIPS_SCREENSHOT=path.png` captures once the assets have had
/// time to land and then quits, which is the form a visual regression check
/// would use; `SPACESHIPS_SHOT_AT` decides when.
#[cfg(not(target_arch = "wasm32"))]
fn screenshot_on_f12(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    mut exit: MessageWriter<AppExit>,
    mut n: Local<u32>,
    mut auto_done: Local<bool>,
) {
    let auto = std::env::var("SPACESHIPS_SCREENSHOT").ok();

    if let Some(path) = auto.as_ref() {
        let at = screenshot_at();
        if !*auto_done && time.elapsed_secs() > at {
            *auto_done = true;
            info!("writing {path}");
            commands
                .spawn(bevy::render::view::screenshot::Screenshot::primary_window())
                .observe(bevy::render::view::screenshot::save_to_disk(path.clone()));
        }
        // The capture is asynchronous — it round-trips through the render
        // world — so quitting has to wait a beat.
        if *auto_done && time.elapsed_secs() > at + 2.0 {
            // 0.17 renamed buffered events to messages: `EventWriter::send` is
            // `MessageWriter::write`.
            exit.write(AppExit::Success);
        }
    }

    if keys.just_pressed(KeyCode::F12) {
        let path = format!("screenshot-{}.png", *n);
        *n += 1;
        info!("writing {path}");
        commands
            .spawn(bevy::render::view::screenshot::Screenshot::primary_window())
            .observe(bevy::render::view::screenshot::save_to_disk(path));
    }
}

/// `SPACESHIPS_RES=640x360` overrides the window size.
///
/// Exists to answer "is this fill-rate bound or CPU bound", which the frame
/// counter alone cannot on a vsync'd display: if halving the pixel count moves
/// the frame into a lower vsync bucket, the cost is in the post stack.
fn window_resolution() -> bevy::window::WindowResolution {
    let default = bevy::window::WindowResolution::default();
    let Some(spec) = std::env::var("SPACESHIPS_RES").ok() else {
        return default;
    };
    let Some((w, h)) = spec.split_once(['x', 'X']) else {
        return default;
    };
    match (w.trim().parse::<u32>(), h.trim().parse::<u32>()) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => bevy::window::WindowResolution::new(w, h),
        _ => default,
    }
}

/// Where a frame's time went, split into the part this client controls and the
/// part the monitor does.
///
/// Two numbers, because on a vsync'd surface they answer different questions
/// and only one of them is about the game.
///
/// `Time::delta` is the *interval between frames*, and that is a property of
/// the display. `WindowPlugin` above explains why `SPACESHIPS_NO_VSYNC` cannot
/// change it here — wgpu has no immediate present mode on macOS/Metal, so
/// `AutoNoVsync` falls back to Fifo — and the consequence is that the interval
/// quantises to the refresh period. A ProMotion panel stepping from 110 Hz to
/// 60 Hz reads as `9.09 ms` becoming `16.67 ms`: the client apparently losing
/// nearly half its frame rate while doing exactly the same work. Reported once
/// a second over a long match, that is indistinguishable from something
/// accumulating — which is what it was mistaken for. Note that 16.67 is not
/// twice 9.09: a frame that genuinely overran its budget would land on a
/// *multiple* of the old interval, and one that lands on a different interval
/// entirely is the panel changing rate.
///
/// `busy` is the wall clock spent in the main world, `First` to `Last`, and
/// nothing quantises it. It is the number to watch when the question is
/// "is the client getting slower": when the reported fps falls and `busy` does
/// not, the game did not get slower.
///
/// Neither number covers the render world, which runs on its own thread. For
/// that, `SPACESHIPS_DRAWCALLS=1` (see `weapons.rs`) counts batched draw calls
/// once a second, and `SPACESHIPS_RES` shrinks the target to answer whether a
/// cost is fill-rate bound.
#[derive(Resource)]
struct FrameCost {
    /// When this frame's main-world work began.
    ///
    /// `bevy::platform`'s `Instant` rather than `std`'s: `std::time::Instant`
    /// panics on `wasm32-unknown-unknown`, which is a target this crate builds
    /// for.
    opened: Instant,
    /// Main-world time over the reporting window, and the worst single frame
    /// in it.
    busy: Duration,
    worst_busy: Duration,
    /// Wall clock over the window, the frames in it, and the worst interval —
    /// the display's side of the same second.
    elapsed: f32,
    frames: u32,
    worst: f32,
}

impl Default for FrameCost {
    fn default() -> FrameCost {
        FrameCost {
            opened: Instant::now(),
            busy: Duration::ZERO,
            worst_busy: Duration::ZERO,
            elapsed: 0.0,
            frames: 0,
            worst: 0.0,
        }
    }
}

fn open_frame(mut cost: ResMut<FrameCost>) {
    cost.opened = Instant::now();
}

/// Rolling frame-time readout, once a second.
///
/// Hand-rolled rather than `FrameTimeDiagnosticsPlugin`, which would pull in
/// `bevy_diagnostic` and its dependents for a number this prints once a second.
fn report_frame_time(time: Res<Time>, mut cost: ResMut<FrameCost>) {
    let busy = cost.opened.elapsed();
    cost.busy += busy;
    cost.worst_busy = cost.worst_busy.max(busy);

    let dt = time.delta_secs();
    cost.elapsed += dt;
    cost.frames += 1;
    cost.worst = cost.worst.max(dt);

    if cost.elapsed < 1.0 {
        return;
    }

    let frames = cost.frames.max(1) as f32;
    info!(
        "{:.2} ms/frame avg ({:.0} fps), {:.2} ms worst; cpu {:.2} ms avg, {:.2} ms worst",
        cost.elapsed * 1000.0 / frames,
        frames / cost.elapsed,
        cost.worst * 1000.0,
        cost.busy.as_secs_f32() * 1000.0 / frames,
        cost.worst_busy.as_secs_f32() * 1000.0,
    );
    // `opened` is not reset: `open_frame` writes it at the top of every frame.
    cost.busy = Duration::ZERO;
    cost.worst_busy = Duration::ZERO;
    cost.elapsed = 0.0;
    cost.frames = 0;
    cost.worst = 0.0;
}
