//! Third-person chase camera, and the post-processing stack.
//!
//! The camera motion is ported from `public/src/camera.js`; the render settings
//! target **Ultra Graphics** (`public/src/graphics.js`), which is the look worth
//! preserving — not the default forward path, and emphatically not the default
//! path's 1/3-resolution PSX pixel filter.
//!
//! # Ultra, mapped onto Bevy 0.19
//!
//! | `graphics.js` | here |
//! |---|---|
//! | HalfFloat composer target | [`Hdr`] |
//! | ACES in the grade shader | [`Tonemapping::AcesFitted`] |
//! | `UnrealBloomPass(0.58, 0.62, 0.92)` | [`Bloom`] |
//! | `uAberration: 0.0014` | [`ChromaticAberration`] |
//! | `uVignette: 0.34` | [`Vignette`] |
//! | `PCFSoftShadowMap` | [`ShadowFilteringMethod::Gaussian`] |
//! | `antialias: false` | [`Msaa::Off`] |
//!
//! Only the JS's chase camera is ported. `_updateFreeLook` needs
//! right-mouse-drag and a mouse-delta accumulator, which is input work.

use bevy::camera::Hdr;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::light::ShadowFilteringMethod;
use bevy::post_process::bloom::{Bloom, BloomPrefilter};
use bevy::post_process::effect_stack::{ChromaticAberration, Vignette};
use bevy::prelude::*;

use crate::scene::ShipRoot;
use crate::LOCAL_ID;

/// `ThirdPersonCamera.distance`.
const DISTANCE: f32 = 11.0;
/// `ThirdPersonCamera.heightOffset`.
const HEIGHT: f32 = 5.6;
/// How much of the way from the ship's own forward toward "look back at the
/// ship" the view direction sits. `camera.js:53` (`lerp(dirToShip, 0.25)`).
const VIEW_BLEND: f32 = 0.25;

/// The camera's smoothed up vector. In Three.js this is `camera.up`, a mutable
/// field on the camera object; Bevy's `Transform::look_at` takes up as an
/// argument, so it has to be stored.
#[derive(Component)]
struct ChaseCam {
    up: Vec3,
}

pub struct FollowCameraPlugin;

impl Plugin for FollowCameraPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_camera)
            // The chase target is a transform, so this has to land before
            // transform propagation or the camera trails by a frame.
            .add_systems(PostUpdate, follow.before(TransformSystems::Propagate));
    }
}

pub fn spawn_camera(mut commands: Commands) {
    commands.spawn((
        Camera3d::default(),
        // Ultra renders into a HalfFloat target so bloom has real headroom
        // above 1.0. `Hdr` is the equivalent switch, and without it the bloom
        // prefilter has nothing to find. Moved from `bevy::render` to
        // `bevy::camera` in 0.19.
        Hdr,
        // `graphics.js` sets `renderer.toneMapping = NoToneMapping` and then
        // applies ACES by hand in the final pass so it cannot be applied
        // twice. Bevy owns tonemapping, so the operator goes here instead —
        // same curve, one application.
        Tonemapping::AcesFitted,
        // `UnrealBloomPass(strength 0.58, radius 0.62, threshold 0.92)`.
        // Bevy's `intensity` is not on the same scale as three's `strength`,
        // so this is tuned by eye from `NATURAL`; the threshold is the part
        // that has to be faithful, because Ultra's bright star cores and
        // additive glow idiom are authored to sit just above it.
        Bloom {
            intensity: 0.22,
            low_frequency_boost: 0.7,
            prefilter: BloomPrefilter {
                threshold: 0.9,
                threshold_softness: 0.4,
            },
            ..Bloom::NATURAL
        },
        // The combined grade pass, minus the parts Bevy has no component for.
        // `uAberration: 0.0014` scales with r², which is what
        // `ChromaticAberration` does natively.
        ChromaticAberration {
            intensity: 0.006,
            ..default()
        },
        // `uVignette: 0.34`, `smoothstep(0.15, 0.85, r2 * 1.9)`.
        Vignette {
            intensity: 0.34,
            radius: 0.55,
            smoothness: 0.55,
            ..default()
        },
        // TODO(grade): film grain, and the lift/gain/saturation/contrast trim
        // in `GradeShader`, have no Bevy component. They are a small custom
        // post-process node — one fullscreen shader — and are the only part of
        // the Ultra stack that does not map onto something built in.
        //
        // `PCFSoftShadowMap`.
        ShadowFilteringMethod::Gaussian,
        // Ultra sets `antialias: false` on purpose and leans on a high pixel
        // ratio instead; MSAA there comes from the composer's own multisampled
        // target only on low-density displays. Off is the faithful default,
        // and it is also what lets the HDR target stay cheap.
        Msaa::Off,
        // The moon is 160 units across and the field reaches 400; the default
        // far plane of 1000 clips the far half of the map.
        Projection::Perspective(PerspectiveProjection {
            fov: std::f32::consts::FRAC_PI_4,
            near: 0.5,
            far: 20_000.0,
            ..default()
        }),
        ChaseCam { up: Vec3::Y },
        Transform::from_xyz(0.0, HEIGHT, -540.0 - DISTANCE),
    ));
}

// Reads the ship's *interpolated* `Transform`, not its pose in `SimFrame`.
//
// `SimFrame` holds the last completed tick, so at 60 Hz sim on a 144 Hz display
// it is a staircase. Chasing it directly leaves a periodic 60 Hz wobble that the
// exponential damping below only partly filters — measured at 0.13 units peak to
// peak for a boosting ship, about 11 px at 720p. `scene::draw_interpolated` has
// already written the smoothed pose to this entity by the time `PostUpdate`
// runs, and reading that instead takes the residual to zero.
//
// The `Without` bounds are load-bearing: both queries touch `Transform`, and
// Bevy cannot prove the archetypes are disjoint without them.
fn follow(
    ships: Query<(&ShipRoot, &Transform), Without<ChaseCam>>,
    time: Res<Time>,
    mut cam: Query<(&mut Transform, &mut ChaseCam), Without<ShipRoot>>,
) {
    let Some((_, me)) = ships.iter().find(|(root, _)| root.0 == LOCAL_ID) else {
        return;
    };
    let Ok((mut tf, mut chase)) = cam.single_mut() else {
        return;
    };

    let dt = time.delta_secs();
    let ship_pos = me.translation;
    let q = me.rotation;

    // The ship's own axes. The nose is local +z, so "back" is local -z.
    let back = q * Vec3::NEG_Z;
    let up = q * Vec3::Y;
    let fwd = q * Vec3::Z;

    let desired = ship_pos + back * DISTANCE + up * HEIGHT;

    // `1 - base^dt` is Three.js's frame-rate-independent damping, and the two
    // bases differ on purpose: position chases hard (0.0001) while the up
    // vector rolls in slowly (0.01), which is what keeps a barrel roll from
    // whipping the horizon around.
    tf.translation = tf.translation.lerp(desired, damp(0.0001, dt));
    chase.up = chase.up.lerp(up, damp(0.01, dt)).normalize_or(Vec3::Y);

    // Look slightly *past* the nose rather than straight down it, so the ship
    // sits low in frame instead of dead centre.
    let to_ship = (ship_pos - tf.translation).normalize_or(fwd);
    let view = fwd.lerp(to_ship, VIEW_BLEND).normalize_or(fwd);
    let target = tf.translation + view * 100.0;

    // `Transform::look_at` points -Z at the target, the same camera convention
    // Three.js uses, so no sign flip is needed.
    let up = chase.up;
    tf.look_at(target, up);
}

/// Three.js's `1 - base^dt` exponential damping factor.
#[inline]
fn damp(base: f32, dt: f32) -> f32 {
    1.0 - base.powf(dt)
}
