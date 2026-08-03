//! Renders the Sierras heightfield to a top-down PNG.
//!
//! ```text
//! cargo run -p spaceships-sim --example heightmap -- /tmp/sierras.png
//! ```
//!
//! A dev tool, not part of the simulation. It exists because tuning a map by
//! launching the game, spawning, and flying somewhere costs a minute a look,
//! while this costs a second — and because a plan view shows layout mistakes
//! (a river that runs into a mesa, a ravine that does not reach the wall it is
//! meant to cut) that a first-person screenshot hides completely.
//!
//! The PNG writer below is deliberately dependency-free, using stored — that
//! is, uncompressed — deflate blocks, because `crates/sim` takes no
//! dependencies and an example that pulled in `png` would be the first crack in
//! that. The files are a few megabytes and nothing keeps them.

use spaceships_sim::rules::WorldRules;
use spaceships_sim::terrain;

/// Pixels per side of the output.
const SIZE: u32 = 1024;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "sierras.png".to_string());
    let w = WorldRules::DEFAULT;
    let half = w.terrain_size * 0.5;

    let mut rgb = vec![0u8; (SIZE * SIZE * 3) as usize];
    let mut peak = f64::NEG_INFINITY;
    let mut deepest = f64::INFINITY;

    for py in 0..SIZE {
        for px in 0..SIZE {
            // +z runs down the image, so north (−z) is at the top.
            let x = (f64::from(px) / f64::from(SIZE - 1)) * w.terrain_size - half;
            let z = (f64::from(py) / f64::from(SIZE - 1)) * w.terrain_size - half;
            let h = terrain::ground_height(x, z, &w);
            peak = peak.max(h);
            deepest = deepest.min(h);

            // A cheap hillshade from the north-west, which is what makes ridge
            // lines and channel walls legible in a plan view.
            let step = terrain::lattice_step(&w);
            let dx = terrain::ground_height(x + step, z, &w) - h;
            let dz = terrain::ground_height(x, z + step, &w) - h;
            let shade = (1.0 - (dx + dz) / (step * 1.4)).clamp(0.45, 1.55);

            let (r, g, b) = shade_of(h, &w);
            let i = ((py * SIZE + px) * 3) as usize;
            rgb[i] = clamp8(f64::from(r) * shade);
            rgb[i + 1] = clamp8(f64::from(g) * shade);
            rgb[i + 2] = clamp8(f64::from(b) * shade);
        }
    }

    // Landmark overlay: the two landing pads, in white.
    for cz in [-w.airfield_z, w.airfield_z] {
        outline(
            &mut rgb,
            &w,
            -w.airfield_half.x,
            cz - w.airfield_half.z,
            w.airfield_half.x,
            cz + w.airfield_half.z,
            [255, 255, 255],
        );
    }

    std::fs::write(&path, encode_png(SIZE, SIZE, &rgb)).expect("write png");
    println!("wrote {path}  ({SIZE}x{SIZE})");
    println!("peak {peak:.1}   deepest {deepest:.1}   water {}", w.water_level);
}

/// The palette the plan view uses. Not the game's — this one is picked to make
/// *elevation bands* readable, which is a different job from looking good.
fn shade_of(h: f64, w: &WorldRules) -> (u8, u8, u8) {
    let d = h - w.water_level;
    if d < -40.0 {
        (14, 42, 92)
    } else if d < -8.0 {
        (26, 74, 140)
    } else if d < 0.0 {
        (52, 116, 178)
    } else if d < 8.0 {
        (196, 184, 132)
    } else if d < 60.0 {
        (96, 142, 76)
    } else if d < 150.0 {
        (74, 118, 58)
    } else if d < 260.0 {
        (122, 112, 82)
    } else if d < 420.0 {
        (138, 130, 118)
    } else if d < 560.0 {
        (176, 172, 166)
    } else {
        (238, 240, 245)
    }
}

fn clamp8(v: f64) -> u8 {
    v.clamp(0.0, 255.0) as u8
}

/// Draws a world-space rectangle onto the image, as an outline.
fn outline(rgb: &mut [u8], w: &WorldRules, x0: f64, z0: f64, x1: f64, z1: f64, c: [u8; 3]) {
    let half = w.terrain_size * 0.5;
    let to_px = |v: f64| (((v + half) / w.terrain_size) * f64::from(SIZE - 1)) as i64;
    let (px0, pz0, px1, pz1) = (to_px(x0), to_px(z0), to_px(x1), to_px(z1));
    let mut put = |x: i64, y: i64| {
        if (0..i64::from(SIZE)).contains(&x) && (0..i64::from(SIZE)).contains(&y) {
            let i = ((y * i64::from(SIZE) + x) * 3) as usize;
            rgb[i..i + 3].copy_from_slice(&c);
        }
    };
    for x in px0..=px1 {
        put(x, pz0);
        put(x, pz1);
    }
    for z in pz0..=pz1 {
        put(px0, z);
        put(px1, z);
    }
}

// ---------------------------------------------------------------------------
// A minimal PNG encoder
// ---------------------------------------------------------------------------

/// 8-bit RGB, one stored deflate block per 65,535 bytes. Valid PNG, no
/// compression, no dependencies.
fn encode_png(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(rgb.len() + height as usize);
    for y in 0..height as usize {
        raw.push(0); // filter type 0: none
        let row = y * width as usize * 3;
        raw.extend_from_slice(&rgb[row..row + width as usize * 3]);
    }

    let mut out = Vec::new();
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, truecolour
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01]; // deflate, 32K window, no preset dict
    let mut i = 0;
    while i < data.len() {
        let n = (data.len() - i).min(0xFFFF);
        let last = u8::from(i + n >= data.len());
        out.push(last);
        out.extend_from_slice(&(n as u16).to_le_bytes());
        out.extend_from_slice(&(!(n as u16)).to_le_bytes());
        out.extend_from_slice(&data[i..i + n]);
        i += n;
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}
