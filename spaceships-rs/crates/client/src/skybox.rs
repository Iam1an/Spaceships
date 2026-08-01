//! Procedural nebula cubemap, ported from `createUltraSkybox` in
//! `public/src/graphics.js`.
//!
//! This is the **Ultra** skybox, not the default one in `skybox.js`. The
//! difference is the whole point: the stock sky is 500 flat stars on near-black,
//! while this layers fBm dust, four coloured nebula lobes, a dense
//! temperature-varied star field, and a handful of bright cores authored to sit
//! *above* the bloom threshold so the post pass turns them into real glow.
//!
//! Bevy's [`Skybox`] wants the six faces as one `Image` with
//! `depth_or_array_layers: 6` and a cube texture view, in the same
//! +X, -X, +Y, -Y, +Z, -Z order Three.js uses, so the port is direct.
//!
//! The same cubemap is also fed to [`GeneratedEnvironmentMapLight`], which is
//! Bevy's equivalent of the JS's `PMREMGenerator` + `scene.environment` — it
//! prefilters at runtime, so every `StandardMaterial` in the scene picks up
//! reflections off the nebula.
//!
//! Generating it beats shipping it: six 1024² PNGs would be a megabyte of
//! payload on a build whose entire problem is payload.

use bevy::asset::RenderAssetUsages;
use bevy::camera::RenderTarget;
use bevy::image::Image;
use bevy::light::{GeneratedEnvironmentMapLight, Skybox};
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};

use spaceships_sim::rng::Rng;

/// Face size. The JS uses 1024; 512 is 6 MB of RGBA instead of 24 MB, which
/// matters far more in a wasm build than the extra sharpness does. The dust and
/// nebula lobes are smooth by construction and the stars are 1–2 px either way.
const FACE: u32 = 512;

/// Linear resolution divisor for the fBm field. `graphics.js`'s `FIELD_DIV`,
/// and for the same reason: the field is smooth, so evaluating it at a quarter
/// rate and interpolating is indistinguishable and 16x less work. Stars are
/// drawn at full resolution afterwards.
const FIELD_DIV: u32 = 4;

/// Fixed, so the sky is the same sky on every client and every run.
const SKY_SEED: u64 = 0x5C1_5EED;

/// A nebula lobe: a direction, a colour, a half-angle, and a gain.
///
/// Defined in *world* space rather than per face, which is what makes the
/// clouds line up across cube seams. `spread` well under 1 leaves most of the
/// sky genuinely black.
struct Nebula {
    dir: [f32; 3],
    col: [f32; 3],
    spread: f32,
    gain: f32,
}

/// `graphics.js`'s `NEBULAE`, verbatim.
const NEBULAE: [Nebula; 4] = [
    Nebula {
        dir: [0.62, 0.28, -0.73],
        col: [0.30, 0.52, 1.00],
        spread: 0.85,
        gain: 0.80,
    },
    Nebula {
        dir: [-0.78, -0.12, -0.61],
        col: [0.95, 0.30, 0.55],
        spread: 0.68,
        gain: 0.52,
    },
    Nebula {
        dir: [-0.30, 0.70, 0.65],
        col: [0.35, 0.85, 0.95],
        spread: 0.60,
        gain: 0.38,
    },
    Nebula {
        dir: [0.20, -0.75, 0.63],
        col: [0.55, 0.40, 1.00],
        spread: 0.55,
        gain: 0.34,
    },
];

pub struct SkyboxPlugin;

impl Plugin for SkyboxPlugin {
    fn build(&self, app: &mut App) {
        // Ultra's deep-space base is a very dark blue rather than pure black,
        // "so the hull has something to reflect". The skybox covers every
        // pixel, so this only shows if it fails to load.
        app.insert_resource(ClearColor(Color::srgb(0.012, 0.016, 0.032)))
            // Both the skybox and the environment map are components on the
            // camera, so the camera has to exist first.
            .add_systems(Startup, attach_sky.after(crate::camera::spawn_camera));
    }
}

// Only the camera that draws the world gets a sky.
//
// `ui.rs` runs a second `Camera3d` on its own render layer to draw the spinning
// ship into the CRT's off-screen image. An unfiltered `With<Camera3d>` query
// attaches the nebula to that one too — which happens to look good behind the
// preview, but is accidental and depends on system order. Filtering on a
// window target makes the intent explicit and stops a third camera inheriting
// it by surprise.
fn attach_sky(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    cameras: Query<(Entity, Option<&RenderTarget>), With<Camera3d>>,
) {
    let handle = images.add(nebula_cubemap());
    for (cam, target) in &cameras {
        // `RenderTarget` is a separate component in 0.19, and absent means the
        // default — the primary window. Only an explicit `Image` target is the
        // off-screen preview camera.
        if matches!(target, Some(RenderTarget::Image(_))) {
            continue;
        }
        commands.entity(cam).insert((
            Skybox {
                // 0.19: this is `Option<Handle<Image>>`, not a bare handle —
                // `None` means "draw nothing", not "draw the default".
                image: Some(handle.clone()),
                // A scale factor into cd/m². The texture is authored in the
                // JS's 0..1 display space, where the dust sits around 0.05, so
                // it needs a real exposure to survive ACES next to a 9000 lux
                // key light. Scaling the whole cubemap preserves the
                // dust/star/core relationship the texture already encodes.
                brightness: 3000.0,
                ..default()
            },
            // `applyEnvironment`: PMREM the cubemap and hang it on
            // `scene.environment` so every standard material picks up real
            // reflections. `envMapIntensity` in the JS is 0.45 generic / 0.9 on
            // ships; Bevy's intensity is per-probe rather than per-material, so
            // this is the single value both end up sharing.
            //
            // TODO(env intensity): recovering the per-material split needs a
            // second `LightProbe` volume or a material extension. Worth doing —
            // the ship reading brighter than the rocks is a deliberate part of
            // the Ultra look.
            GeneratedEnvironmentMapLight {
                environment_map: handle.clone(),
                intensity: 900.0,
                ..default()
            },
        ));
    }
}

/// Builds the six-layer RGBA image [`Skybox`] wants.
fn nebula_cubemap() -> Image {
    let mut rng = Rng::new(SKY_SEED);
    let px = (FACE * FACE) as usize;
    let mut data = Vec::with_capacity(px * 6 * 4);

    for face in 0..6u32 {
        let mut buf = vec![0u8; px * 4];
        draw_field(&mut buf, face);
        draw_stars(&mut buf, &mut rng);
        draw_bright_cores(&mut buf, &mut rng);
        data.extend_from_slice(&buf);
    }

    Image {
        // Without this the six layers are a 2D array, not a cube, and the
        // skybox pipeline fails at bind-group creation with a wgpu validation
        // error rather than anything that mentions skyboxes.
        texture_view_descriptor: Some(TextureViewDescriptor {
            dimension: Some(TextureViewDimension::Cube),
            ..default()
        }),
        ..Image::new(
            Extent3d {
                width: FACE,
                height: FACE,
                depth_or_array_layers: 6,
            },
            TextureDimension::D2,
            data,
            TextureFormat::Rgba8UnormSrgb,
            // The CPU copy is dead weight after upload — 6 MB of it on wasm.
            RenderAssetUsages::RENDER_WORLD,
        )
    }
}

// ---------------------------------------------------------------------------
// Layers
// ---------------------------------------------------------------------------

/// Dust and nebula lobes: `makeNebulaFace`'s field pass, evaluated at
/// `FACE / FIELD_DIV` and bilinearly upsampled.
fn draw_field(buf: &mut [u8], face: u32) {
    let n = (FACE / FIELD_DIV).max(4);
    let mut field = vec![[0.0f32; 3]; (n * n) as usize];

    for py in 0..n {
        let t = (py as f32 + 0.5) / n as f32 * 2.0 - 1.0;
        for px in 0..n {
            let s = (px as f32 + 0.5) / n as f32 * 2.0 - 1.0;
            let d = cube_dir(face, s, t);
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            let (nx, ny, nz) = (d[0] / len, d[1] / len, d[2] / len);

            // Deep space base — very dark blue, not pure black.
            let mut c = [0.012f32, 0.016, 0.032];

            // Direction-driven fBm, so the dust is continuous across seams.
            let warp = fbm(nx * 2.4 + 8.0, ny * 2.4 + nz * 1.3, 3, 4);
            let dust = fbm(
                nx * 3.1 + warp * 0.9,
                ny * 3.1 + nz * 2.0 + warp * 0.9,
                91,
                6,
            );

            for neb in &NEBULAE {
                let dot = nx * neb.dir[0] + ny * neb.dir[1] + nz * neb.dir[2];
                let mut m = ((dot - (1.0 - neb.spread)) / neb.spread).max(0.0);
                m = m.powf(2.6);
                if m <= 0.0005 {
                    continue;
                }
                let cloud = (((dust - 0.34).max(0.0)) / 0.66).powf(1.9);
                let a = m * cloud * neb.gain;
                for (ch, col) in c.iter_mut().zip(neb.col) {
                    *ch += col * a * 0.85;
                }
            }

            // Faint cold haze everywhere, so empty sky is not dead flat.
            let haze = (((dust - 0.62).max(0.0)) / 0.38).powf(2.4) * 0.10;
            c[0] += haze * 0.30;
            c[1] += haze * 0.45;
            c[2] += haze * 0.85;

            field[(py * n + px) as usize] = c;
        }
    }

    // `ctx.drawImage` with `imageSmoothingQuality: 'high'` — bilinear.
    for y in 0..FACE {
        let fy = ((y as f32 + 0.5) / FACE as f32) * n as f32 - 0.5;
        let (y0, ty) = split(fy, n);
        for x in 0..FACE {
            let fx = ((x as f32 + 0.5) / FACE as f32) * n as f32 - 0.5;
            let (x0, tx) = split(fx, n);
            let x1 = (x0 + 1).min(n - 1);
            let y1 = (y0 + 1).min(n - 1);

            let i = ((y * FACE + x) * 4) as usize;
            for ch in 0..3 {
                let a = field[(y0 * n + x0) as usize][ch];
                let b = field[(y0 * n + x1) as usize][ch];
                let c = field[(y1 * n + x0) as usize][ch];
                let d = field[(y1 * n + x1) as usize][ch];
                let top = a + (b - a) * tx;
                let bot = c + (d - c) * tx;
                buf[i + ch] = ((top + (bot - top) * ty) * 255.0).min(255.0) as u8;
            }
            buf[i + 3] = 255;
        }
    }
}

/// Splits a sample coordinate into a clamped integer index and a fraction.
#[inline]
fn split(f: f32, n: u32) -> (u32, f32) {
    let i = f.floor();
    let t = f - i;
    (i.clamp(0.0, (n - 1) as f32) as u32, t.clamp(0.0, 1.0))
}

/// The dense star field. `starCount = size² / 620` — about 420 stars per face
/// at 512, weighted hard toward dim by `pow(random, 2.6)`.
fn draw_stars(buf: &mut [u8], rng: &mut Rng) {
    let count = (FACE * FACE) / 620;
    for _ in 0..count {
        let x = rng.next_f64() as f32 * FACE as f32;
        let y = rng.next_f64() as f32 * FACE as f32;
        let a = (rng.next_f64() as f32).powf(2.6);
        let rad = a * 1.5 + 0.25;
        let alpha = 0.25 + a * 0.75;

        // Blue-white through amber, weighted toward white.
        let temp = rng.next_f64() as f32;
        let col = if temp < 0.7 {
            [255.0, 250.0, 255.0]
        } else {
            [255.0, 225.0 - temp * 40.0, 190.0 - temp * 60.0]
        };
        disc(buf, x, y, rad, col, alpha);
    }
}

/// Ten bright cores per face with a halo. These are the ones authored to push
/// past the bloom threshold and bleed — the reason `camera.rs` keeps the
/// prefilter threshold near Ultra's 0.92 instead of running thresholdless
/// bloom.
fn draw_bright_cores(buf: &mut [u8], rng: &mut Rng) {
    for _ in 0..10 {
        let x = rng.next_f64() as f32 * FACE as f32;
        let y = rng.next_f64() as f32 * FACE as f32;
        let hue = 190.0 + rng.next_f64() as f32 * 90.0;
        let radius = 9.0 + rng.next_f64() as f32 * 14.0;

        // `createRadialGradient` with stops at 0 / 0.35 / 1.
        let inner = hsl_to_rgb(hue, 0.90, 0.92);
        let mid = hsl_to_rgb(hue, 0.90, 0.70);
        let x0 = (x - radius).max(0.0) as u32;
        let x1 = ((x + radius) as u32).min(FACE - 1);
        let y0 = (y - radius).max(0.0) as u32;
        let y1 = ((y + radius) as u32).min(FACE - 1);
        for py in y0..=y1 {
            for pxx in x0..=x1 {
                let d = ((pxx as f32 - x).powi(2) + (py as f32 - y).powi(2)).sqrt() / radius;
                if d >= 1.0 {
                    continue;
                }
                let (col, alpha) = if d < 0.35 {
                    let t = d / 0.35;
                    (lerp3(inner, mid, t), 0.85 + (0.22 - 0.85) * t)
                } else {
                    let t = (d - 0.35) / 0.65;
                    (mid, 0.22 * (1.0 - t))
                };
                blend(buf, pxx, py, col, alpha);
            }
        }

        // The white core itself.
        disc(
            buf,
            x,
            y,
            1.3 + rng.next_f64() as f32,
            [255.0, 255.0, 255.0],
            1.0,
        );
    }
}

// ---------------------------------------------------------------------------
// Noise
// ---------------------------------------------------------------------------

/// `graphics.js`'s integer bit-mix hash. Kept as an integer mix rather than the
/// usual `sin`-based hash because the field is evaluated a few hundred thousand
/// times at startup, and because a `sin` hash's exact output is one of the
/// portability hazards `sim` warns about.
fn hash(x: i32, y: i32, seed: i32) -> f32 {
    let mut h = (x as i64 * 374_761_393 + y as i64 * 668_265_263 + seed as i64 * 1_442_695_041)
        as i32 as u32;
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    ((h ^ (h >> 16)) as f32) / 4_294_967_296.0
}

fn value_noise(x: f32, y: f32, seed: i32) -> f32 {
    let xi = x.floor();
    let yi = y.floor();
    let xf = x - xi;
    let yf = y - yi;
    let u = xf * xf * (3.0 - 2.0 * xf);
    let v = yf * yf * (3.0 - 2.0 * yf);
    let (xi, yi) = (xi as i32, yi as i32);
    let a = hash(xi, yi, seed);
    let b = hash(xi + 1, yi, seed);
    let c = hash(xi, yi + 1, seed);
    let d = hash(xi + 1, yi + 1, seed);
    a * (1.0 - u) * (1.0 - v) + b * u * (1.0 - v) + c * (1.0 - u) * v + d * u * v
}

fn fbm(x: f32, y: f32, seed: i32, octaves: u32) -> f32 {
    let (mut sum, mut amp, mut freq, mut norm) = (0.0, 0.5, 1.0, 0.0);
    for i in 0..octaves {
        sum += amp * value_noise(x * freq, y * freq, seed + i as i32 * 17);
        norm += amp;
        amp *= 0.5;
        freq *= 2.06;
    }
    sum / norm
}

/// Direction vector for a texel on cube face `face`, in `-1..1` face coords.
/// The face order and axis signs are wgpu's cube convention, which is the same
/// one Three.js uses, so `graphics.js`'s `cubeDir` transfers unchanged.
fn cube_dir(face: u32, s: f32, t: f32) -> [f32; 3] {
    match face {
        0 => [1.0, -t, -s],  // +X
        1 => [-1.0, -t, s],  // -X
        2 => [s, 1.0, t],    // +Y
        3 => [s, -1.0, -t],  // -Y
        4 => [s, -t, 1.0],   // +Z
        _ => [-s, -t, -1.0], // -Z
    }
}

// ---------------------------------------------------------------------------
// Raster helpers
// ---------------------------------------------------------------------------

/// An antialiased filled circle, standing in for `ctx.arc(..); ctx.fill()`.
fn disc(buf: &mut [u8], cx: f32, cy: f32, rad: f32, col: [f32; 3], alpha: f32) {
    let r = rad.max(0.5);
    let x0 = (cx - r - 1.0).max(0.0) as u32;
    let x1 = ((cx + r + 1.0) as u32).min(FACE - 1);
    let y0 = (cy - r - 1.0).max(0.0) as u32;
    let y1 = ((cy + r + 1.0) as u32).min(FACE - 1);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            // One-pixel coverage ramp at the edge: the canvas 2D API
            // antialiases, and a hard-edged star at radius 0.25 would otherwise
            // vanish entirely.
            let cov = (r + 0.5 - d).clamp(0.0, 1.0);
            if cov > 0.0 {
                blend(buf, x, y, col, alpha * cov);
            }
        }
    }
}

/// Alpha-blends a colour into one pixel.
fn blend(buf: &mut [u8], x: u32, y: u32, col: [f32; 3], alpha: f32) {
    let i = ((y * FACE + x) * 4) as usize;
    for c in 0..3 {
        let dst = f32::from(buf[i + c]);
        buf[i + c] = (dst + (col[c] - dst) * alpha).clamp(0.0, 255.0) as u8;
    }
}

#[inline]
fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// CSS `hsl()` to 0–255 RGB. `h` in degrees, `s`/`l` in `0..1`.
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [(r + m) * 255.0, (g + m) * 255.0, (b + m) * 255.0]
}
