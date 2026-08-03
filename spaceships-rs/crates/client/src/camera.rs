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
//! | lift / gain / saturation / contrast / grain | [`FilmGrade`] |
//! | `PCFSoftShadowMap` | [`ShadowFilteringMethod::Gaussian`] |
//! | `antialias: false` | [`Msaa::Off`] |
//!
//! Only the JS's chase camera is ported. `_updateFreeLook` needs
//! right-mouse-drag and a mouse-delta accumulator, which is input work.

use bevy::asset::uuid_handle;
use bevy::camera::Hdr;
use bevy::core_pipeline::fullscreen_material::{FullscreenMaterial, FullscreenMaterialPlugin};
use bevy::core_pipeline::tonemapping::{tonemapping, Tonemapping};
use bevy::core_pipeline::Core3dSystems;
use bevy::ecs::schedule::ScheduleConfigs;
use bevy::ecs::system::BoxedSystem;
use bevy::light::ShadowFilteringMethod;
use bevy::post_process::bloom::{Bloom, BloomPrefilter};
use bevy::post_process::effect_stack::{ChromaticAberration, Vignette};
use bevy::prelude::*;
use bevy::render::extract_component::ExtractComponent;
use bevy::render::render_resource::ShaderType;
use bevy::shader::{Shader, ShaderRef};

use crate::scene::ShipRoot;

/// `ThirdPersonCamera.distance`.
const DISTANCE: f32 = 11.0;
/// `ThirdPersonCamera.heightOffset`.
const HEIGHT: f32 = 5.6;
/// How much of the way from the ship's own forward toward "look back at the
/// ship" the view direction sits. `camera.js:53` (`lerp(dirToShip, 0.25)`).
const VIEW_BLEND: f32 = 0.25;

/// The chase camera's resting vertical field of view, in radians.
///
/// Public because [`crate::warp`]'s FOV punch decelerates *into* it and needs a
/// fallback for a camera it cannot read one from. The punch normally captures
/// whatever the projection actually holds when it starts, because `cockpit.rs`
/// swaps this out for the seated profile on `V`.
///
/// `main.js:45` is `new THREE.PerspectiveCamera(75, ...)` — three.js takes
/// **degrees**, vertical. Bevy takes radians, and its default is 45 degrees, so
/// leaving it defaulted made the view a third narrower than the game it is
/// reproducing: everything looked magnified and the field of view read wrong.
pub const BASE_FOV: f32 = 75.0 * std::f32::consts::PI / 180.0;

/// The one camera the player looks through.
///
/// `With<Camera3d>` stopped meaning that when `ui.rs` added the menu's ship
/// preview, which is a second `Camera3d` rendering to an off-screen image. A
/// `single()` over two of them does not pick one, it *fails* — which is how
/// `cockpit.rs` silently stopped seating anyone the day the preview landed:
/// pressing `V` hid the flat HUD and the canopy, and left the camera exactly
/// where the chase camera had it.
///
/// So the question "which camera draws the match" gets an answer that cannot
/// drift. `skybox.rs` and `hud.rs` each infer it — from the render target and
/// from render layer 0 respectively — and both of those still hold; this is for
/// callers that need it as a query *filter*, where neither test can be
/// expressed.
#[derive(Component)]
pub struct FlightCamera;

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
        app.add_plugins(FullscreenMaterialPlugin::<FilmGrade>::default())
            .add_systems(Startup, (install_grade_shader, spawn_camera))
            // The chase target is a transform, so this has to land before
            // transform propagation or the camera trails by a frame.
            //
            // Gated on the camera still being attached to a ship: a replay's
            // free camera flies six degrees of freedom and writes the same
            // `Transform`, and two systems fighting over one camera is a view
            // that snaps back to the ship every frame.
            .add_systems(
                PostUpdate,
                follow
                    .before(TransformSystems::Propagate)
                    .run_if(crate::replay::camera_is_attached),
            )
            .add_systems(Update, advance_grain);
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
        // additive glow idiom are authored to sit just above it — including
        // `warp.rs`'s streaks, which are the same `0xccffff` additive material
        // `warp.js` uses.
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
        // The rest of `GradeShader`: the lift/gain/saturation/contrast trim and
        // the film grain, which have no Bevy component. See [`FilmGrade`].
        FilmGrade::default(),
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
            fov: BASE_FOV,
            near: 0.5,
            far: 20_000.0,
            ..default()
        }),
        ChaseCam { up: Vec3::Y },
        FlightCamera,
        Transform::from_xyz(0.0, HEIGHT, -540.0 - DISTANCE),
    ));
}

// ---------------------------------------------------------------------------
// The grade pass
// ---------------------------------------------------------------------------

/// The tail of `graphics.js`'s `GradeShader`, as a fullscreen pass.
///
/// Bevy owns four of the six things that shader does — ACES, chromatic
/// aberration, vignette, and the sRGB encode — so what is left, and what this
/// carries, is the part that changes the *character* of the image rather than
/// its shape: a lift and gain that cool the shadows and warm nothing, a
/// saturation push, a contrast curve, and animated grain. Ultra without it is
/// the same scene rendered flat and slightly grey: the contrast curve alone
/// takes everything under 0.024 linear to black, which is the difference
/// between the JS's tight blue nebula on a dead-black sky and a milky wash over
/// the whole frame.
///
/// **Scheduled `after(tonemapping)`, which is where the JS grades.** The default
/// for a [`FullscreenMaterial`] is *before* it, and that would be wrong twice
/// over: contrast about 0.5 and a saturation mix are display-referred
/// operations, meaningless on an HDR buffer whose highlights run to 20; and the
/// grain would be tone-mapped after being added, so its amplitude would depend
/// on scene brightness. `Core3dSystems::PostProcess` still runs before
/// `upscaling`, so this is the last thing to touch the image.
///
/// # It also carries the warp lens
///
/// `BACKLOG.md` §9 asks for the warp-in's radial bend to live "in that same node
/// rather than a second pass", and this is that node: it is already the last
/// thing to touch the image, it already has the screen texture bound, and a
/// second fullscreen pass would be a second full-resolution round trip through
/// memory for one frame in fifty. [`WarpLens`] is what [`crate::warp`] writes
/// into it; at rest every lane is zero and the displacement is exactly the
/// identity, so the idle cost is a handful of ALU on a pass that is already
/// bandwidth-bound.
///
/// # Four uniform members, not eleven
///
/// WebGL2 requires every uniform struct member to be 16-byte aligned, so the
/// scalars ride in the `w` lanes of the vectors they belong with rather than
/// as their own fields — the same packing, and the same reason, as `scene.rs`'s
/// `DamageFlash`. It also keeps the buffer at 64 bytes with no padding to get
/// wrong.
///
/// # Not ported
///
/// `uExposure: 1.10`, which the JS applies to the HDR colour *before* ACES.
/// There is nowhere for it here: this pass runs after tone mapping, and Bevy's
/// pre-tonemap exposure is [`bevy::camera::Exposure`], in EV100 against
/// physical light units, while `install_space_lights` already anchors its key
/// light by eye because three.js's intensities are unitless. Folding a 10%
/// linear lift into a scale that is neither linear nor calibrated to the same
/// zero would be a number that looks like a port and is not one. The exposure
/// that matters is in the lights.
#[derive(Component, ExtractComponent, Clone, Copy, ShaderType)]
pub struct FilmGrade {
    /// `rgb`: `uLift`. `a`: `uSaturation`.
    lift: Vec4,
    /// `rgb`: `uGain`. `a`: `uContrast`.
    gain: Vec4,
    /// `x`: `uGrain`. `y`: `uTime`, in seconds. `zw`: unused padding.
    grain: Vec4,
    /// [`WarpLens`], lane for lane. Not in `GradeShader` — see the type docs.
    warp: Vec4,
}

/// The warp-in's screen-space distortion, as the grade pass sees it.
///
/// Two displacements over one radial direction from the centre of frame, which
/// is where the local player's warp axis points by construction — the arrival
/// streams down the camera's own forward, so the vanishing point is the middle
/// of the screen and needs no uniform of its own.
///
/// Distances are in *aspect-corrected* screen radii: 0.5 is half the frame
/// height, and the corner of a 16:9 frame is at about 0.98. That keeps the bend
/// circular on a wide window instead of an ellipse.
#[derive(Clone, Copy, Default)]
pub struct WarpLens {
    /// The lens itself: how far, as a fraction of frame height, the sample
    /// point moves at the very edge. Grows with r², so the centre of frame is
    /// untouched and only the periphery bows — a lens, not a zoom.
    ///
    /// **Negative magnifies.** A positive value samples *further* from the
    /// centre than the pixel being shaded, which contracts the image toward the
    /// middle and pulls in texels from beyond the edge of the screen texture —
    /// where there are none, and the clamp smears the border. Negative samples
    /// inward, magnifying the periphery, which is both the fisheye direction
    /// and the one that cannot reveal anything undefined.
    pub bend: f32,
    /// Where the shockwave ring is, as a screen radius.
    pub ring_radius: f32,
    /// How wide the ring's influence is. Zero disables it.
    pub ring_width: f32,
    /// How hard the ring drags what it passes over. `BACKLOG.md` §9 asks for
    /// "the ring itself distorting what it passes over", and this is that: the
    /// displacement straddles the ring, outward ahead of it and inward behind,
    /// which is what makes it read as a wave rather than as a painted circle.
    pub ring_gain: f32,
}

impl WarpLens {
    /// The identity. Every lane zero, and the shader's displacement is then
    /// exactly zero rather than nearly zero.
    pub const NONE: WarpLens = WarpLens {
        bend: 0.0,
        ring_radius: 0.0,
        ring_width: 0.0,
        ring_gain: 0.0,
    };
}

impl FilmGrade {
    /// Points the grade pass's lens. See [`WarpLens`].
    pub fn set_lens(&mut self, lens: WarpLens) {
        self.warp = Vec4::new(lens.bend, lens.ring_radius, lens.ring_width, lens.ring_gain);
    }
}

impl Default for FilmGrade {
    /// `GradeShader`'s uniform defaults (`graphics.js:487`), verbatim.
    ///
    /// The lift is not neutral grey: `(0.004, 0.006, 0.016)` puts four times as
    /// much blue as red into the toe, so black reads as deep space rather than
    /// as a dead pixel. The gain answers it at the top with `(1.00, 0.995,
    /// 1.025)`, a quarter-stop of blue in the highlights. Together they are the
    /// cool cast the JS image has and a straight ACES render does not.
    fn default() -> Self {
        Self {
            lift: Vec4::new(0.004, 0.006, 0.016, 1.14),
            gain: Vec4::new(1.00, 0.995, 1.025, 1.05),
            grain: Vec4::new(0.009, 0.0, 0.0, 0.0),
            warp: Vec4::ZERO,
        }
    }
}

impl FullscreenMaterial for FilmGrade {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(GRADE_SHADER)
    }

    fn schedule_configs(system: ScheduleConfigs<BoxedSystem>) -> ScheduleConfigs<BoxedSystem> {
        system.in_set(Core3dSystems::PostProcess).after(tonemapping)
    }
}

/// Compiled in rather than loaded from the asset root, for the reasons
/// `scene.rs`'s `DAMAGE_FLASH_SHADER` gives: the asset root is `public/`, shared
/// with the Three.js client, and `build-wasm.sh` copies a named list out of it.
const GRADE_SHADER: Handle<Shader> = uuid_handle!("0d3f2a41-6c8e-4b52-9f17-2d4a86b0c913");

/// The whole of the grade, as a fragment shader.
///
/// `FullscreenVertexOutput` is declared here rather than imported from
/// `bevy_core_pipeline::fullscreen_vertex_shader` on purpose. That module also
/// holds the `@vertex` entry point, and [`FullscreenMaterialPlugin`] builds its
/// pipeline with `entry_point: None` — it relies on the fragment module having
/// exactly one entry point to pick. Declaring the struct locally keeps that true
/// no matter how the composer treats an imported module's entry points; the
/// layout is the contract, and it is four lines.
///
/// Everything here runs on **display-referred linear** colour: ACES has already
/// clamped to 0..1, and the sRGB encode happens in the blit to the swapchain.
/// That is the same space `GradeShader` grades in — its own `pow(1/2.4)` encode
/// is the last thing it does, after all of this.
///
/// The aspect ratio comes from `textureDimensions` rather than from a uniform:
/// it is `textureSize` in ESSL 3.0 and therefore fine on WebGL2, and a uniform
/// would be one more thing that can be stale by a frame on a window resize.
///
/// The lens samples with a displaced UV, and that UV is clamped: the
/// displacement reaches past the edge of the screen texture by design, and the
/// fullscreen pass's sampler is not guaranteed to clamp for us. Note the
/// `textureSample` stays outside every branch — WGSL requires uniform control
/// flow around it, and naga will reject a sample inside an `if`.
const GRADE_WGSL: &str = r#"
struct FullscreenVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

struct FilmGrade {
    // rgb: lift.       a: saturation.
    lift: vec4<f32>,
    // rgb: gain.       a: contrast.
    gain: vec4<f32>,
    // x: grain amount. y: time.
    grain: vec4<f32>,
    // x: bend.  y: ring radius.  z: ring width.  w: ring gain.
    warp: vec4<f32>,
}

@group(0) @binding(0) var screen: texture_2d<f32>;
@group(0) @binding(1) var screen_sampler: sampler;
@group(0) @binding(2) var<uniform> grade: FilmGrade;

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    // The warp lens. Radial from the centre of frame, in units of frame
    // height, so it stays circular on a wide window.
    let dim = vec2<f32>(textureDimensions(screen));
    let aspect = dim.x / max(dim.y, 1.0);
    var offset = in.uv - vec2<f32>(0.5, 0.5);
    offset.x = offset.x * aspect;
    let r = length(offset);
    let radial = offset / max(r, 1e-5);

    // Space bending: nothing at the centre, growing with r², bowing the edges.
    var push = grade.warp.x * r * r;
    // The shockwave, as the derivative of a Gaussian straddling the ring —
    // outward just outside it, inward just inside, zero everywhere else.
    let band = (r - grade.warp.y) / max(grade.warp.z, 1e-4);
    push = push + grade.warp.w * band * exp(-band * band);

    let bent = clamp(
        in.uv + vec2<f32>(radial.x / aspect, radial.y) * push,
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );

    var col = textureSample(screen, screen_sampler, bent).rgb;

    col = col * grade.gain.rgb + grade.lift.rgb;

    // Rec. 709 luma, and the same value gates the grain below — noise lives in
    // the shadows, where a sensor's does.
    let luma = dot(col, vec3<f32>(0.2126, 0.7152, 0.0722));
    col = mix(vec3<f32>(luma), col, grade.lift.a);
    col = clamp((col - 0.5) * grade.gain.a + 0.5, vec3<f32>(0.0), vec3<f32>(1.0));

    let seed = in.uv * 1024.0 + fract(grade.grain.y) * 91.7;
    let n = fract(sin(dot(seed, vec2<f32>(12.9898, 78.233))) * 43758.5453) - 0.5;
    col += n * grade.grain.x * (1.0 - smoothstep(0.0, 0.7, luma));

    return vec4<f32>(clamp(col, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
"#;

fn install_grade_shader(mut shaders: ResMut<Assets<Shader>>) {
    shaders
        .insert(
            &GRADE_SHADER,
            Shader::from_wgsl(GRADE_WGSL, "spaceships/grade.wgsl"),
        )
        .expect("a uuid handle has no generation to be stale");
}

/// `grade.uniforms.uTime.value += dt` (`graphics.js:603`).
///
/// The shader only ever uses `fract(time)`, so an elapsed count is as good as an
/// accumulator and cannot drift.
fn advance_grain(time: Res<Time>, mut grades: Query<&mut FilmGrade>) {
    for mut grade in &mut grades {
        grade.grain.y = time.elapsed_secs_wrapped();
    }
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
    // Which ship, rather than `LOCAL_ID`. It *is* `LOCAL_ID` in a live match —
    // that is the resource's default — and it is somebody else's aircraft when
    // a replay is riding one. See `replay::ViewTarget`.
    target: Res<crate::replay::ViewTarget>,
    mut cam: Query<(&mut Transform, &mut ChaseCam), Without<ShipRoot>>,
) {
    let Some((_, me)) = ships.iter().find(|(root, _)| root.0 == target.0) else {
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
