//! The lobby, as the aircraft's multi-function display — drawn on a tube.
//!
//! `BACKLOG.md` section 8: the menu is the last surface that still looks like a
//! generic space game — dark-blue glassmorphism, `blur(16px)`, cyan accents —
//! while everything around it has become a fighter sim. This is the redesign,
//! not a port of `public/index.html`'s `#lobby-*` screens. The pages make the
//! same *decisions* (which mode, which map, which room, what to buy); the
//! presentation is a page on a CRT.
//!
//! # Two halves
//!
//! **The layout** ([`build_menu`], [`build_pages`]) is deliberately sparse: one
//! primary element per page, no panel borders, no boxes inside boxes, four
//! hairline rules in the whole design, and nothing labelled that reads without
//! a label. Density is the enemy — the tube does the atmospheric work, so the
//! page under it can be almost empty and still look like an instrument.
//!
//! **The tube** ([`Crt`], [`CRT_WGSL`]) is a real post-process, not styling.
//! The whole menu renders to an off-screen image through its own camera, and a
//! single full-screen node draws that image back through one fragment shader
//! doing barrel curvature, phosphor bleed, shadow-mask fringing, scanlines,
//! vignette and mains flicker in one pass. Faking any of that with per-element
//! decoration — a repeating background for scanlines, a transform for
//! curvature — fights the layout and does not look right.
//!
//! It is kept inside this module on purpose. `camera.rs` owns the one 3D camera
//! and the whole Ultra stack hanging off it; a camera-level post-processing
//! node would have to go there, and would also curve the *game*. A UI-space
//! render target keeps the effect self-contained and leaves the 3D scene
//! untouched, which is the right split anyway: the aircraft is seen through a
//! canopy, the menu is seen on a screen.
//!
//! Nothing in the shader needs a storage buffer, a compute pass or a texture
//! array, and the target is `Rgba8UnormSrgb` rather than the `Bgra` a swapchain
//! prefers, so it runs on WebGL2 as written.
//!
//! # It is the same aircraft as `cockpit.rs`
//!
//! Every colour in [`palette`] is lifted from `cockpit.rs`'s `Palette::new` and
//! `Swatch` rather than re-picked, and a test pins them. The idioms come from
//! there too: annunciator lamps that are always present and only change colour,
//! a scope built from rings and a fixed pool of blips with a sweep arm that is
//! one rotated node, a gauge that is a well plus a fill, and captions that are
//! short, upper case and letter-spaced because that is all `caption_mesh`'s 3x5
//! font can say.
//!
//! # The performance rule this module inherits
//!
//! `hud.rs`'s module docs describe the bug the Bevy client exists to escape:
//! the DOM HUD forced one layout and style recalculation **every frame**,
//! 35–40% of the JS frame callback. A menu is a far bigger tree than a HUD and
//! would reintroduce it at a larger scale. So, the same shape exactly:
//!
//! - The tree is built **once**, in [`build_menu`]. All thirteen pages exist
//!   from startup and switching is two writes to [`Node::display`].
//! - [`drive_menu`] reduces everything visible to a [`MenuModel`] — `Copy`,
//!   `Eq`, integers only, with the one continuous quantity (the sweep)
//!   quantised — compares it whole, and **returns before acquiring a single
//!   `Mut`** when it matches.
//! - **While the menu is closed the model is a constant**, because `sweep`,
//!   `clock` and `caution` are forced to zero when `open` is false. In flight
//!   this module costs one comparison of twelve integers per frame, the
//!   off-screen camera is switched off, and the tube node is `Display::None`.
//! - The tube itself never costs a CPU write. Its one animated term reads
//!   `globals.time` **in the shader**; driving it from Rust would mean mutating
//!   the material asset every frame and re-uploading its uniform buffer, which
//!   is the same bug wearing a different hat.
//!
//! `Display::None` rather than `Visibility::Hidden` for the twelve hidden
//! pages, which is the one place this departs from `hud.rs`. There, hiding
//! `#chargebar` with `Visibility` keeps it in layout so a brake forces no
//! relayout; here the opposite is wanted — twelve full page trees that taffy
//! must measure on every dirty pass cost far more than the single relayout a
//! page change causes, and a page change is an event, not a frame.
//!
//! # The pointer has to be resolved by hand
//!
//! `bevy_ui`'s [`ui_focus_system`] only resolves a cursor for cameras whose
//! render target is a **window**, and this menu's camera targets an *image* so
//! the tube can warp it. So [`Interaction`] never fires on anything in this
//! tree, and a hover path built on it is dead code — which is what it was.
//!
//! Giving up the render target was not on the table: the tube is the design.
//! Instead [`through_the_glass`] runs the shader's own three lines of
//! projection on the CPU, in the one direction a cursor needs — from the tube
//! node's UV out to the menu image's UV — reading the curvature and the face
//! inset back out of the live material so the two can never disagree, not even
//! under `SPACESHIPS_UI_CRT=0`. [`control_at`] then hit-tests the page's own
//! controls at that point, and what comes out drives exactly what the keyboard
//! drives: [`Menu::focus`], and on a click the same [`Action`] through the same
//! [`apply`]. There is one arming path, not two.
//!
//! It obeys the rule above as well: a mouse that has not moved and a button
//! that has not gone down do not reach the hit test at all, so a still pointer
//! costs one `Option<Vec2>` comparison a frame.
//!
//! [`ui_focus_system`]: bevy::ui::ui_focus_system
//!
//! # Where the data comes from
//!
//! Three seams, and this module owns none of them.
//!
//! - **[`LobbyData`] and [`Account`]**, from `api.rs` over HTTP: the pilot's
//!   record, the balance, the unlock ladder, the standings, and who is signed
//!   in. Every page reads the resource; nothing here builds a request. What the
//!   lobby *does* is write an [`ApiRequest`] — "sign me in", "refresh" — and
//!   that is the whole of its vocabulary.
//! - **[`NetSession`]**, from `net.rs` over the socket: the room browser, the
//!   crew room, the match handover.
//! - **[`LaunchRequest`]**, outward, to `sim_bridge.rs`.
//!
//! This used to be one seam with nothing behind it: `LobbyData` was filled by
//! its own `placeholder()` and the `FEED` annunciator honestly read `CACHED`
//! for the life of the process. The placeholder is still there and is still
//! what the pages show before anything has been fetched — the lamp says which,
//! in [`DataSource`]'s own words, so a screenshot cannot present invented
//! numbers as the server's.
//!
//! # Seeing the screens
//!
//! `SPACESHIPS_UI=<page>` opens straight onto one page and holds it there, for
//! the same reason `SPACESHIPS_COCKPIT=1` and `SPACESHIPS_SCREENSHOT` exist: a
//! visual check should not need someone to sit and click. `SPACESHIPS_UI=off`
//! suppresses the menu entirely, and `SPACESHIPS_UI_CRT=0` flattens the tube so
//! the layout can be judged on its own. See [`forced_screen`].

use bevy::app::{RunFixedMainLoop, RunFixedMainLoopSystems};
use bevy::asset::{uuid_handle, AssetId, RenderAssetUsages};
use bevy::camera::visibility::RenderLayers;
use bevy::camera::RenderTarget;
use bevy::ecs::system::SystemParam;
use bevy::image::Image;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, Extent3d, TextureDimension, TextureFormat, TextureUsages,
};
use bevy::shader::{Shader, ShaderRef};
use bevy::text::{FontWeight, LetterSpacing, LineHeight};
use bevy::ui_render::prelude::{MaterialNode, UiMaterial, UiMaterialPlugin};
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use bevy::world_serialization::{WorldAsset, WorldAssetRoot, WorldInstanceReady};

use spaceships_sim as sim;

use sim::world::{MapKind, Mode};

use spaceships_protocol::ClientMessage;

use crate::api::{
    unlock_key, Account, ApiRequest, DataSource, LobbyData, CALLSIGN_MAX, CALLSIGN_MIN,
    PASSWORD_MAX, PASSWORD_MIN,
};
use crate::cockpit::ViewMode;
use crate::net::{
    wire_map, ConnState, NetCommand, NetConfig, NetSession, NetStatus, Phase, ToServer,
};
use crate::sim_bridge::{MatchSetup, PlayerInput};

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Wires the lobby in: one off-screen camera, one tree, one diffing system.
pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        // `LobbyData` is *not* inserted here: `api::ApiPlugin` owns it, because
        // the module that fills a resource should own it and this one only
        // draws it.
        app.init_resource::<Menu>()
            .init_resource::<Applied>()
            .init_resource::<PreviewSkin>()
            .add_message::<LaunchRequest>()
            .add_message::<ReturnToLobby>()
            .add_systems(Update, reopen_menu)
            .init_resource::<LobbyOpen>()
            // Before `read_input`, so a page opened by the server this frame is
            // the page the keyboard acts on.
            .add_systems(Update, follow_session.before(read_input))
            .add_systems(Startup, forced_room)
            // `PreUpdate`, after `bevy_input` has published the frame's keys
            // and before any consumer in `Update` reads them. See
            // [`swallow_typing`] for why it has to be here and not later.
            .add_systems(PreUpdate, swallow_typing.after(bevy::input::InputSystems))
            .add_plugins(UiMaterialPlugin::<Crt>::default())
            // Chained: the shader has to exist before the material that names
            // it, and the material before the node that draws it.
            .add_systems(Startup, (load_shader, build_menu).chain())
            // Chained for the same reason `cockpit.rs` chains its seven: the
            // page must be settled before the cursor is read, and the cursor
            // before the tree is driven.
            .add_systems(
                Update,
                (
                    mirror_cockpit_flag,
                    read_input,
                    advance_boot,
                    publish_lobby_open,
                    publish_volume,
                    drive_menu,
                    paint_preview,
                    spin_preview,
                    fit_tube,
                )
                    .chain(),
            )
            // Ordered before the whole `BeforeFixedMainLoop` set, which is
            // where `sim_bridge::latch_edges` lives — that set is public, the
            // system inside it is not, and ordering against the set is both
            // sufficient and immune to an edit in that module. Running here
            // rather than in `PreUpdate` also guarantees it lands after
            // `input.rs`'s `gather_input`, which declares no ordering of its
            // own: `PreUpdate` finishes before `RunFixedMainLoop` starts.
            .add_systems(
                RunFixedMainLoop,
                (hold_the_stick, release_the_pointer)
                    .before(RunFixedMainLoopSystems::BeforeFixedMainLoop),
            );
    }
}

// ---------------------------------------------------------------------------
// Design tokens
// ---------------------------------------------------------------------------

/// The instrument-panel palette, taken from `cockpit.rs`.
///
/// Every constant names the field it comes from. They are not approximations:
/// the menu and the panel are meant to be the same aircraft, and a colour
/// re-picked by eye is how two surfaces drift apart.
/// `the_palette_is_the_instrument_panels` pins them.
///
/// `pub(crate)` because `hud.rs` is the third surface of the same aircraft and
/// draws from exactly this list — see that module's header. Nothing else in
/// this file opens up: [`Menu`] and every page type stay private, so the menu's
/// *state* is still this module's alone and only its *colours* are shared.
pub(crate) mod palette {
    use bevy::prelude::Color;

    /// `#rrggbb`, as `cockpit.rs::srgb` spells it.
    pub const fn rgb(hex: u32) -> Color {
        Color::srgb(
            ((hex >> 16) & 0xff) as f32 / 255.0,
            ((hex >> 8) & 0xff) as f32 / 255.0,
            (hex & 0xff) as f32 / 255.0,
        )
    }

    /// `#rrggbb` at an alpha.
    pub const fn rgba(hex: u32, a: f32) -> Color {
        Color::srgba(
            ((hex >> 16) & 0xff) as f32 / 255.0,
            ((hex >> 8) & 0xff) as f32 / 255.0,
            (hex & 0xff) as f32 / 255.0,
            a,
        )
    }

    /// `Palette::screen` — what `createScreen` clears an instrument to.
    pub const SCREEN: Color = rgb(0x05_08_0b);
    /// `Palette::well` — the unfilled half of a gauge.
    pub const WELL: Color = rgb(0x0d_15_1c);
    /// `Swatch::dead` — a readout with no power behind it.
    pub const DEAD: Color = rgb(0x16_22_2c);
    /// `Palette::grid` — the scope's rings and every hairline rule.
    pub const GRID: u32 = 0x1d_3a_4a;
    /// `Palette::caption` — every `SPD`/`HULL`/`MSL` legend on the panel.
    pub const CAPTION: u32 = 0x7f_a6_c0;
    /// `Palette::white`.
    pub const WHITE: Color = rgb(0xea_f6_ff);
    /// `Swatch::blip_friendly` — the phosphor green this design is built on.
    pub const PHOSPHOR: Color = rgb(0x46_ff_9b);
    /// `Swatch::tgt_on` — the lit `TGT` annunciator.
    pub const PHOSPHOR_LAMP: Color = rgb(0x38_ff_9b);
    /// `ADMIN_PROFILE::accent` — the amber half of the pair.
    pub const AMBER: Color = rgb(0xff_c4_51);
    /// `Swatch::orange` — a caution that is not yet a warning.
    pub const ORANGE: Color = rgb(0xff_9d_3d);
    /// `Swatch::red` / `blip_hostile`.
    pub const RED: Color = rgb(0xff_4d_4d);
    /// `DEFAULT_PROFILE::accent` — the strip lighting in the default hull.
    pub const CYAN: Color = rgb(0x5f_d8_ff);
}

use palette as pal;

/// `Palette::caption` at an alpha. Legends are dimmed rather than re-coloured,
/// which is what keeps the page to two hues plus grey.
fn dim(a: f32) -> Color {
    pal::rgba(pal::CAPTION, a)
}

/// Margin between the page and the tube's edge.
///
/// Generous on purpose. Barrel curvature bows the picture outward and rounds
/// the corners off, so anything within a few percent of a corner is on the part
/// of the tube the electron gun cannot reach.
const EDGE_X: f32 = 54.0;
const EDGE_Y: f32 = 38.0;
/// Width of the page-key rail.
const RAIL: f32 = 116.0;
/// Width of the scope column.
const SCOPE_W: f32 = 168.0;
/// Gap between the rail, the page and the scope. Space is what groups things
/// here; there are no boxes to do it.
const GUTTER: f32 = 40.0;
/// Every rule in the design is exactly this thick, and there are four of them.
const HAIR: f32 = 1.0;

// ---------------------------------------------------------------------------
// The tube
// ---------------------------------------------------------------------------

/// One fragment shader, one pass: curvature, fringing, bleed, scanlines,
/// vignette, flicker.
///
/// Written against WebGL2's limits — no storage buffers, no texture arrays, no
/// dynamic indexing, every `textureSample` in uniform control flow.
///
/// The zoom is worth a note. Barrel warping maps the frame's edge midpoints to
/// `1 + k`, i.e. off the texture, so a naive implementation leaves a black band
/// down all four sides. Pre-dividing by `1 + k` puts the edges back exactly on
/// the boundary and leaves only the corners short, which is what a real tube
/// looks like: bowed sides that reach the bezel, corners rounded off.
const CRT_WGSL: &str = r#"
#import bevy_render::globals::Globals
#import bevy_ui::ui_vertex_output::UiVertexOutput

@group(0) @binding(1) var<uniform> globals: Globals;

// x, y = tube size in logical px; z = curvature; w = scanline depth.
@group(1) @binding(0) var<uniform> geometry: vec4<f32>;
// x = convergence error; y = vignette; z = phosphor bleed; w = mains flicker.
@group(1) @binding(1) var<uniform> optics: vec4<f32>;
// x = face inset; y = bezel width; z = spill reach; w = spill gain.
@group(1) @binding(2) var<uniform> housing: vec4<f32>;
@group(1) @binding(3) var tube: texture_2d<f32>;
@group(1) @binding(4) var tube_sampler: sampler;

const TAU: f32 = 6.2831853;
// Scanline pitch, in logical pixels. Fixed in logical space so the spacing is
// the same on a Retina panel as on a 1x one; deriving it from physical pixels
// is what makes scanlines moire.
const PITCH: f32 = 3.0;
// Phosphor does not bleed from an unlit cell. Only what clears this glows.
const BLEED_FLOOR: f32 = 0.14;

fn warp(p: vec2<f32>, k: f32) -> vec2<f32> {
    return p * (1.0 + k * dot(p, p));
}

@fragment
fn fragment(in: UiVertexOutput) -> @location(0) vec4<f32> {
    let size = geometry.xy;
    let k = geometry.z;
    let scan_depth = geometry.w;
    let converge = optics.x;
    let vignette = optics.y;
    let bleed = optics.z;
    let flicker = optics.w;
    let inset = housing.x;
    let bezel_w = housing.y;
    let reach = housing.z;
    let spill_gain = housing.w;

    // Centred; `1 + k` puts the bowed edges back where an unwarped frame would
    // have them, and `inset` then pulls the whole face in to leave room for the
    // housing around it.
    let c = (in.uv * 2.0 - 1.0) * inset / (1.0 + k);
    let w = warp(c, k);

    // `max(abs())` is the tube's rectangular boundary in warped space: 1.0 is
    // the edge of the glass, and larger is off it.
    let edge = max(abs(w.x), abs(w.y));

    // Anti-aliased, not stencilled. `fwidth` is the boundary's width in this
    // pixel, so the falloff is always about a pixel however the window is
    // sized, and the glass curves away rather than being cut out.
    let aa = max(fwidth(edge), 1e-5);
    let glass = 1.0 - smoothstep(1.0 - aa, 1.0 + aa * 2.0, edge);
    let shell = 1.0 - smoothstep(1.0 + bezel_w, 1.0 + bezel_w + aa * 2.0, edge);

    // Clamped, so the taps outside the glass read the border pixel rather than
    // wrapping. That is exactly what the spill wants: the light escaping past
    // the edge is the light at the edge.
    let uv = clamp(w * 0.5 + 0.5, vec2<f32>(0.0), vec2<f32>(1.0));
    let r2 = dot(c, c);

    // Shadow mask: the red and blue guns converge slightly off the green one,
    // and the error grows with deflection, so the fringe shows at the edges and
    // vanishes in the middle.
    let err = converge * r2;
    let uv_r = clamp(warp(c, k + err) * 0.5 + 0.5, vec2<f32>(0.0), vec2<f32>(1.0));
    let uv_b = clamp(warp(c, k - err) * 0.5 + 0.5, vec2<f32>(0.0), vec2<f32>(1.0));
    var picture = vec3<f32>(
        textureSample(tube, tube_sampler, uv_r).r,
        textureSample(tube, tube_sampler, uv).g,
        textureSample(tube, tube_sampler, uv_b).b
    );

    // Phosphor bleed. Eight taps on a ring, thresholded, so a lit glyph glows
    // into the dark around it and a dark page stays dark. This is what makes
    // CRT text read as warm rather than as crisp vector output — and it is the
    // same quantity the housing is lit by, below.
    let sp = 2.2 / size;
    var glow = textureSample(tube, tube_sampler, uv + vec2<f32>( sp.x, 0.0)).rgb;
    glow += textureSample(tube, tube_sampler, uv + vec2<f32>(-sp.x, 0.0)).rgb;
    glow += textureSample(tube, tube_sampler, uv + vec2<f32>(0.0,  sp.y)).rgb;
    glow += textureSample(tube, tube_sampler, uv + vec2<f32>(0.0, -sp.y)).rgb;
    glow += textureSample(tube, tube_sampler, uv + sp * 0.75).rgb;
    glow += textureSample(tube, tube_sampler, uv - sp * 0.75).rgb;
    glow += textureSample(tube, tube_sampler, uv + vec2<f32>(sp.x, -sp.y) * 0.75).rgb;
    glow += textureSample(tube, tube_sampler, uv + vec2<f32>(-sp.x, sp.y) * 0.75).rgb;
    glow *= 0.125;
    picture += max(glow - vec3<f32>(BLEED_FLOOR), vec3<f32>(0.0)) * bleed;

    // Scanlines: one dark band every PITCH logical pixels, renormalised by the
    // mean of the modulation so turning the depth up darkens the gaps rather
    // than the whole page.
    let scan = 0.5 + 0.5 * cos(uv.y * (size.y / PITCH) * TAU);
    picture *= (1.0 - scan_depth * scan) / (1.0 - scan_depth * 0.5);

    // Brightness falls off from the centre of the deflection.
    picture *= 1.0 - vignette * (0.35 * r2 + 0.65 * r2 * r2);

    // Mains hum. Read from the globals uniform rather than written per frame
    // from Rust — see the module docs on why that distinction matters.
    picture *= 1.0 + flicker * sin(globals.time * 47.0);

    // -- the housing --------------------------------------------------------
    //
    // A tube sits in something. Without it the curvature reads as a rectangle
    // cut out of void; with it, as an object. Kept to a thin moulding: a dark
    // neutral that is fractionally lighter along the top, a shadow in the
    // groove where it meets the glass, and a hairline catchlight on the outer
    // top edge.
    let bezel = max(shell - glass, 0.0);
    let up = -w.y / max(edge, 1e-4);
    var shade = 0.022 + 0.018 * max(up, 0.0);
    var frame = vec3<f32>(shade * 0.88, shade * 0.95, shade);
    // The groove.
    frame *= 0.30 + 0.70 * smoothstep(1.0, 1.0 + bezel_w * 0.7, edge);
    // The catchlight.
    frame += vec3<f32>(0.055) * max(up, 0.0)
           * smoothstep(1.0 + bezel_w * 0.72, 1.0 + bezel_w, edge);

    // -- the spill ----------------------------------------------------------
    //
    // A CRT lights its own housing, and the room past it. `reach` is in warped
    // units past the glass; the falloff is exponential, so the moulding is lit
    // and the world beyond it is only tinted.
    let past = max(edge - 1.0, 0.0);
    let spill = exp(-past / reach) * (1.0 - glass);
    let lamp = clamp(glow * 3.2, vec3<f32>(0.0), vec3<f32>(1.0));

    // -- composite ----------------------------------------------------------
    //
    // `BlendState::ALPHA_BLENDING` is **not** premultiplied, so the colour
    // returned here is what the destination is mixed toward and the alpha is
    // how far. Beyond the housing the alpha falls to nothing and the 3D scene
    // behind — `skybox.rs`'s starfield and whatever `scene.rs` has drawn — is
    // what fills the surround, which is the point: the menu is a display *in*
    // the world, not a rectangle over it.
    var col = picture * glass + (frame + lamp * spill * spill_gain) * bezel;
    var a = shell;

    // Past the moulding, only the glow reaches, and it tints rather than
    // covers.
    let outside = max(1.0 - shell, 0.0);
    let halo = spill * spill_gain * 0.5 * outside;
    col = col + lamp * halo;
    a = clamp(a + halo, 0.0, 1.0);

    // Unpremultiply what the alpha will re-apply, so a faint halo is a faint
    // tint of the right colour rather than a dark smear of it.
    if a > 0.001 {
        col = col / a;
    }
    return vec4<f32>(col, a);
}
"#;

/// The shader handle. A fixed id so [`Crt::fragment_shader`], a static method
/// with no access to the world, can name it.
const CRT_SHADER: Handle<Shader> = uuid_handle!("6f3b2c10-9d41-4a7e-b2c8-5e1d7a904f22");

/// The tube, as a `bevy_ui` material.
#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
struct Crt {
    /// `xy` tube size in logical px, `z` curvature, `w` scanline depth.
    #[uniform(0)]
    geometry: Vec4,
    /// `x` convergence error, `y` vignette, `z` bleed, `w` flicker.
    #[uniform(1)]
    optics: Vec4,
    /// `x` face inset, `y` bezel width, `z` spill reach, `w` spill gain.
    #[uniform(2)]
    housing: Vec4,
    /// What the menu rendered into.
    #[texture(3)]
    #[sampler(4)]
    source: Handle<Image>,
}

impl UiMaterial for Crt {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(CRT_SHADER)
    }
}

/// How hard the tube is driven. Restrained on purpose: curvature that reads as
/// a tube rather than as a gimmick, and a bleed that warms 9px text without
/// dissolving it.
const CURVATURE: f32 = 0.05;
const SCANLINE_DEPTH: f32 = 0.16;
/// Convergence error, as a *fraction of the half-width at the frame edge*. The
/// first pass used 0.010, which is 14 px of separation on a 1440-wide window —
/// legible as two overlapping copies of the text rather than as a fringe. A
/// real shadow mask is off by a pixel or two at the corners and by nothing in
/// the middle, so this wants to be small enough to see only as colour.
const CONVERGENCE: f32 = 0.0018;
const VIGNETTE: f32 = 0.22;
const BLEED: f32 = 0.6;
const FLICKER: f32 = 0.005;
/// How far the glass is pulled in from the window, as a divisor. 1.09 leaves
/// about 4% of each half-dimension for the moulding and the surround — enough
/// to read as a housing, not enough to waste the display.
const INSET: f32 = 1.06;
/// Width of the moulding, in warped units past the glass.
const BEZEL: f32 = 0.030;
/// How far the phosphor spill reaches past the glass, in the same units.
const SPILL_REACH: f32 = 0.045;
const SPILL_GAIN: f32 = 0.85;

impl Crt {
    fn new(source: Handle<Image>, size: Vec2, on: bool) -> Crt {
        let (curve, scan, conv, vig, bleed, flick) = if on {
            (
                CURVATURE,
                SCANLINE_DEPTH,
                CONVERGENCE,
                VIGNETTE,
                BLEED,
                FLICKER,
            )
        } else {
            // `SPACESHIPS_UI_CRT=0`: a straight blit, so the layout can be
            // judged without the tube on top of it. One code path either way.
            (0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
        };
        Crt {
            geometry: Vec4::new(size.x.max(1.0), size.y.max(1.0), curve, scan),
            optics: Vec4::new(conv, vig, bleed, flick),
            // The housing stays even with the tube switched off, so
            // `SPACESHIPS_UI_CRT=0` answers "is this the layout" without also
            // answering "is this the frame".
            housing: Vec4::new(INSET, BEZEL, SPILL_REACH, if on { SPILL_GAIN } else { 0.0 }),
            source,
        }
    }

    /// The two terms the pointer has to reproduce: curvature and face inset.
    ///
    /// Read back out of the uniforms rather than from [`CURVATURE`] and
    /// [`INSET`] directly, because `Crt::new` zeroes the curvature under
    /// `SPACESHIPS_UI_CRT=0` — and a cursor mapped through a curve the shader
    /// is not drawing lands next to the row it is over. This is one number with
    /// one home.
    fn glass(&self) -> (f32, f32) {
        (self.geometry.z, self.housing.x)
    }
}

/// Registers [`CRT_WGSL`] under [`CRT_SHADER`].
///
/// The source is a `const &str` in this file rather than a `.wgsl` in
/// `public/`, because `ASSET_ROOT` is the Three.js client's asset directory and
/// `build-wasm.sh` copies a whitelist of three files out of it. A shader that
/// had to be added to both is a shader that silently does not load on the web.
fn load_shader(mut shaders: ResMut<Assets<Shader>>) {
    // `insert` can only fail if the id belongs to a different asset type, which
    // a `const Handle<Shader>` cannot; reporting rather than unwrapping because
    // a menu that failed to load its shader should still start the game.
    if let Err(e) = shaders.insert(
        CRT_SHADER.id(),
        Shader::from_wgsl(CRT_WGSL, "spaceships-client/ui.rs#crt"),
    ) {
        error!("the CRT shader could not be registered: {e}");
    }
}

/// `SPACESHIPS_UI_CRT=0` flattens the tube.
fn crt_on() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        true
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        !matches!(
            std::env::var("SPACESHIPS_UI_CRT").as_deref(),
            Ok("0") | Ok("off")
        )
    }
}

// ---------------------------------------------------------------------------
// The ship on the screen
// ---------------------------------------------------------------------------

/// The render layer the preview lives on.
///
/// The off-screen camera is restricted to it, so it sees the pedestal and the
/// pilot's own airframe and **nothing of the match** — and, symmetrically,
/// `camera.rs`'s camera (which has no `RenderLayers`, so layer 0) never sees
/// the preview. Two scenes, one process, no coordination between the modules
/// that own them.
const PREVIEW_LAYER: usize = 3;

/// Which glTF is spinning.
///
/// `scene.rs` pins `SHIP_MODEL = "spaceship.glb"`. A parallel branch is adding
/// a `SPACESHIPS_SHIP_MODEL` switch and a `public/jet.glb`; this reads the same
/// variable so the two agree the day it lands, and falls back to the model that
/// exists today rather than to one that does not.
fn ship_model() -> String {
    // `scene::ship_model()`, not the environment variable behind it: the
    // preview has to show the hull the player will actually fly, and those
    // stopped being the same thing when the jet became the default.
    crate::scene::ship_model()
}

/// `customization.js:` the preview turns 0.007 rad per frame at 60 Hz. Kept as
/// a rate rather than a per-frame step so it turns at the same speed on a
/// 144 Hz panel.
const PREVIEW_SPIN: f32 = 0.007 * 60.0;
/// How much of the tube the airframe gets, as a percentage. The livery page's
/// three columns of names occupy the rest.
const PREVIEW_COLUMN: f32 = 36.0;
/// The pedestal ring pulses at about 1.8 Hz (`customization.js`).
const RING_HZ: f32 = 1.8;
/// How far the ring breathes, as a fraction of its radius.
const RING_SWELL: f32 = 0.035;

/// The spinning airframe's root.
#[derive(Component)]
struct Preview;

/// The pedestal ring.
#[derive(Component)]
struct PreviewRing;

/// The pose and visibility of one of the two preview entities. A named type
/// only because the disjointness proof (`Without<the other one>`) makes the
/// inline form unreadable.
type PreviewQuery<'w, 's, Mine, Theirs> =
    Query<'w, 's, (&'static mut Transform, &'static mut Visibility), (With<Mine>, Without<Theirs>)>;

/// The two materials the preview's meshes were collapsed onto.
///
/// **Cloned once**, in [`dress_preview`], exactly as `scene.rs::paint_and_upgrade`
/// clones once per ship: the glTF's own materials are shared with every ship in
/// the match, so writing them in place would repaint the whole squadron. Once
/// they exist, changing a colour is two writes to two assets — never a clone,
/// and never per frame.
#[derive(Resource, Default)]
struct PreviewSkin {
    hull: Option<Handle<StandardMaterial>>,
    accent: Option<Handle<StandardMaterial>>,
    /// The paint last written, so [`paint_preview`] can compare and return.
    applied: Option<(u8, u8)>,
}

/// The luminance below which an authored material is an **accent** rather than
/// **hull**. `scene.rs::ACCENT_LUMA`, `ship.js:34`.
const ACCENT_LUMA: f32 = 0.35;

fn is_accent(base_color: Color) -> bool {
    let c = LinearRgba::from(base_color);
    0.2126 * c.red + 0.7152 * c.green + 0.0722 * c.blue < ACCENT_LUMA
}

/// The paint box.
///
/// A short list rather than a colour wheel: `bevy_ui` has no canvas, a wheel
/// would be an image asset this module cannot ship, and a named list is
/// navigable from the keyboard — which matters more here than it looks, because
/// a UI rendered to an off-screen camera gets no pointer at all (see
/// [`read_input`]).
const LIVERY: [(&str, u32); 8] = [
    ("ICE", 0xdf_e9_f5),
    ("STEEL", 0x8e_a2_b4),
    ("PHOSPHOR", 0x46_ff_9b),
    ("AMBER", 0xff_c4_51),
    ("EMBER", 0xff_5a_3c),
    ("VIOLET", 0xc0_84_fc),
    ("AZURE", 0x3d_9d_ff),
    ("GRAPHITE", 0x2a_31_38),
];

/// `customization.js`'s five trail shapes.
const TRAIL_SHAPES: [&str; 5] = ["CIRCLE", "SQUARE", "TRIANGLE", "STAR", "DAVID"];

/// Builds the pedestal, the lights and the airframe, all on [`PREVIEW_LAYER`].
fn build_preview(
    commands: &mut Commands,
    assets: &AssetServer,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) -> Entity {
    let layer = RenderLayers::layer(PREVIEW_LAYER);

    // Key and fill. Two directionals rather than one plus ambient, because
    // `AmbientLight` is global and this module must not touch the match's.
    commands.spawn((
        DirectionalLight {
            illuminance: 5_500.0,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_xyz(4.0, 6.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
        layer.clone(),
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 1_400.0,
            color: pal::CYAN,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_xyz(-5.0, -1.0, -3.0).looking_at(Vec3::ZERO, Vec3::Y),
        layer.clone(),
    ));

    // The pedestal: one emissive ring, lying flat under the hull.
    let ring = meshes.add(Torus::new(1.02, 1.07));
    let ring_mat = materials.add(StandardMaterial {
        base_color: Color::BLACK,
        emissive: LinearRgba::from(pal::PHOSPHOR) * 0.55,
        ..default()
    });
    commands.spawn((
        Mesh3d(ring),
        MeshMaterial3d(ring_mat),
        Transform::from_xyz(0.0, -0.62, 0.0),
        layer.clone(),
        PreviewRing,
    ));

    // The airframe.
    let scene: Handle<WorldAsset> =
        assets.load(bevy::gltf::GltfAssetLabel::Scene(0).from_asset(ship_model()));
    commands
        .spawn((
            Transform::from_scale(Vec3::splat(0.30)),
            Visibility::Hidden,
            layer.clone(),
            Preview,
        ))
        .with_children(|root| {
            root.spawn((
                WorldAssetRoot(scene),
                // `ship.js:45`: the model's nose rests along +x and the
                // simulation calls the nose +z.
                Transform::from_rotation(Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2)),
            ))
            // `WorldInstanceReady` fires on the entity holding the
            // `WorldAssetRoot` and does not propagate, so the observer goes
            // here rather than on the parent — the same trap `scene.rs` notes.
            .observe(dress_preview);
        })
        .id()
}

/// Puts the loaded glTF on [`PREVIEW_LAYER`] and collapses it onto two
/// materials.
///
/// Two jobs in one walk. `RenderLayers` is **not** inherited in Bevy 0.19, so
/// every mesh the loader spawned has to be stamped or the off-screen camera
/// sees nothing; and the authored materials are shared with the match's ships,
/// so they are cloned before a colour is ever written to them.
fn dress_preview(
    ready: On<WorldInstanceReady>,
    mut commands: Commands,
    children: Query<&Children>,
    meshes: Query<&MeshMaterial3d<StandardMaterial>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut skin: ResMut<PreviewSkin>,
) {
    let mut cloned: HashMap<AssetId<StandardMaterial>, Handle<StandardMaterial>> = HashMap::new();

    for descendant in children.iter_descendants(ready.entity) {
        commands
            .entity(descendant)
            .insert(RenderLayers::layer(PREVIEW_LAYER));

        let Ok(MeshMaterial3d(source)) = meshes.get(descendant) else {
            continue;
        };
        let handle = match cloned.get(&source.id()) {
            Some(h) => h.clone(),
            None => {
                let Some(mut mat) = materials.get(source).cloned() else {
                    continue;
                };
                let accent = is_accent(mat.base_color);
                // The same treatment `scene.rs` gives a ship, so the model on
                // the screen is the model that flies.
                mat.metallic = 0.55;
                mat.perceptual_roughness = 0.34;
                let handle = materials.add(mat);
                if accent {
                    skin.accent = Some(handle.clone());
                } else {
                    skin.hull = Some(handle.clone());
                }
                cloned.insert(source.id(), handle.clone());
                handle
            }
        };
        commands.entity(descendant).insert(MeshMaterial3d(handle));
    }
    // Force the first paint: the selection has not changed, but the materials
    // it applies to have only just come into existence.
    skin.applied = None;
}

/// Writes the two materials when the paint changes, and not otherwise.
///
/// The `applied` comparison is the same trick as [`drive_menu`]'s, one level
/// down: mutating a `StandardMaterial` re-uploads its bind group, so doing it
/// every frame would be the GPU-side twin of the DOM HUD's per-frame style
/// write.
fn paint_preview(
    menu: Res<Menu>,
    mut skin: ResMut<PreviewSkin>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let want = (menu.hull, menu.accent);
    if skin.applied == Some(want) {
        return;
    }
    let (Some(hull), Some(accent)) = (skin.hull.clone(), skin.accent.clone()) else {
        // The glTF has not landed yet; try again next frame.
        return;
    };
    skin.applied = Some(want);

    for (handle, idx) in [(hull, want.0), (accent, want.1)] {
        if let Some(mut mat) = materials.get_mut(&handle) {
            let colour = pal::rgb(LIVERY[usize::from(idx) % LIVERY.len()].1);
            // Alpha is the model's, not the palette's: a canopy authored
            // translucent must stay translucent (`scene.rs`).
            mat.base_color = colour.with_alpha(mat.base_color.alpha());
        }
    }
}

/// Turns the airframe and breathes the ring.
///
/// Two `Transform` writes on two entities, and only while the livery page is
/// up. Transforms, not materials: pulsing the ring's emissive would re-upload a
/// bind group every frame, where a scale is propagated by machinery that runs
/// regardless.
fn spin_preview(
    time: Res<Time>,
    menu: Res<Menu>,
    mut q_ship: PreviewQuery<Preview, PreviewRing>,
    mut q_ring: PreviewQuery<PreviewRing, Preview>,
) {
    let showing = menu.open && menu.screen == Screen::Livery;
    let want = if showing {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };

    if let Ok((mut tf, mut vis)) = q_ship.single_mut() {
        vis.set_if_neq(want);
        if showing {
            tf.rotate_y(PREVIEW_SPIN * time.delta_secs());
        }
    }
    if let Ok((mut tf, mut vis)) = q_ring.single_mut() {
        vis.set_if_neq(want);
        if showing {
            let pulse =
                1.0 + RING_SWELL * (time.elapsed_secs() * RING_HZ * std::f32::consts::TAU).sin();
            tf.scale = Vec3::new(pulse, 1.0, pulse);
        }
    }
}

// ---------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------

/// The two faces, resolved once at startup.
///
/// **Bitcount Prop Single** carries almost everything. Its glyphs are built
/// from discrete dots on a grid, which is what a phosphor raster actually is,
/// so it sits with the scanlines and the bleed instead of fighting them — a
/// smooth outline face under a scanline mask reads as a smooth face that has
/// been damaged, where this reads as a face the tube drew. It is variable, and
/// the weight axis is worth using: readouts run light, values run heavy.
///
/// **Orbitron** is kept for the two places that have to read as the *game*
/// rather than as the display — the wordmark and the pilot's callsign. It is
/// the JS client's face (`index.html` pulls it from Google Fonts) and the one
/// `hud.rs` now loads for the in-game HUD, so those strings match across the
/// three surfaces.
///
/// That is two faces and no more. Both are vendored in `public/fonts/`, which
/// `ASSET_ROOT` points at and `build-wasm.sh` copies, so the web build gets the
/// same pair rather than falling back to the embedded FiraMono subset.
///
/// # The size floor
///
/// A dot-grid face turns to mush when the dots approach the scanline pitch.
/// [`Fonts::num`] is therefore floored at [`MIN_DOT_SIZE`], and anything the
/// design wants smaller than that is set in Orbitron instead — which is why
/// [`Fonts::small`] exists and why the footer hints and the annunciator legends
/// use it.
#[derive(Clone)]
struct Fonts {
    /// Orbitron.
    brand: TextFont,
    /// Bitcount Prop Single.
    dot: TextFont,
}

/// Below this, the dot grid and the 3-pixel scanline pitch beat against each
/// other and the text stops being text.
const MIN_DOT_SIZE: f32 = 11.0;

impl Fonts {
    fn load(assets: &AssetServer) -> Fonts {
        Fonts {
            brand: TextFont {
                font: assets
                    .load::<Font>("fonts/Orbitron-VariableFont_wght.ttf")
                    .into(),
                ..default()
            },
            dot: TextFont {
                font: assets
                    .load::<Font>("fonts/BitcountPropSingle-Variable.ttf")
                    .into(),
                ..default()
            },
        }
    }

    /// A heading, in the tube's own face.
    fn head(&self, size: f32, weight: u16) -> TextFont {
        TextFont {
            font_size: FontSize::Px(size.max(MIN_DOT_SIZE)),
            weight: FontWeight(weight),
            ..self.dot.clone()
        }
    }

    /// A readout or a value, in the tube's own face.
    fn num(&self, size: f32, weight: u16) -> TextFont {
        TextFont {
            font_size: FontSize::Px(size.max(MIN_DOT_SIZE)),
            weight: FontWeight(weight),
            ..self.dot.clone()
        }
    }

    /// The small print, in Orbitron — see the size floor above.
    fn small(&self, size: f32, weight: u16) -> TextFont {
        TextFont {
            font_size: FontSize::Px(size),
            weight: FontWeight(weight),
            ..self.brand.clone()
        }
    }

    /// The wordmark and the callsign: the game's own face.
    fn brand(&self, size: f32, weight: u16) -> TextFont {
        TextFont {
            font_size: FontSize::Px(size),
            weight: FontWeight(weight),
            ..self.brand.clone()
        }
    }
}

// ---------------------------------------------------------------------------
// Screens
// ---------------------------------------------------------------------------

/// One page of the display.
///
/// One-to-one with `public/index.html`'s `#lobby-*` panels except for
/// [`Screen::Boot`], which is new, and the profile overlay, which splits into
/// [`Screen::Record`] and [`Screen::Standings`] because a service record and a
/// squadron ladder are not the same document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
enum Screen {
    /// Power-on self test.
    #[default]
    Boot,
    /// Mission board. `#lobby-main`.
    Main,
    /// Solo tasking. `#lobby-single`, `#lobby-tutorial`.
    Solo,
    /// The four circuits. `#lobby-trials`.
    Trials,
    /// The three operations. `#lobby-campaign`.
    Campaign,
    /// Network hub. `#lobby-multi`.
    Net,
    /// Sortie creation. `#lobby-create`.
    Create,
    /// Tasking orders — the room browser. `#lobby-find`.
    Browser,
    /// Crew room — the waiting room. `#lobby-room`.
    Waiting,
    /// Armory / requisition. The shop's ladder.
    Armory,
    /// Livery: the `#customization` drawer, with the airframe spinning on the
    /// screen rather than in a canvas beside it.
    Livery,
    /// Pilot service record. The `#profile-overlay` PROFILE tab.
    Record,
    /// Squadron standings. Its LEADERBOARD tab.
    Standings,
    /// Sign in, enlist, or fly as a guest. `#auth-overlay`, which in the JS is
    /// a modal over everything and here is a page like any other — because a
    /// guest never has to visit it, and a modal that must be dismissed before
    /// the game can be seen is the one thing `#auth-overlay` gets wrong.
    Auth,
    /// Systems configuration. `#settingsPanel`.
    Config,
}

impl Screen {
    const ALL: [Screen; 15] = [
        Screen::Boot,
        Screen::Main,
        Screen::Solo,
        Screen::Trials,
        Screen::Campaign,
        Screen::Net,
        Screen::Create,
        Screen::Browser,
        Screen::Waiting,
        Screen::Armory,
        Screen::Livery,
        Screen::Record,
        Screen::Standings,
        Screen::Auth,
        Screen::Config,
    ];

    fn index(self) -> usize {
        self as usize
    }

    fn title(self) -> &'static str {
        match self {
            Screen::Boot => "STARTING UP",
            Screen::Main => "MAIN MENU",
            Screen::Solo => "SINGLE PLAYER",
            Screen::Trials => "TIME TRIALS",
            Screen::Campaign => "CAMPAIGN",
            Screen::Net => "MULTIPLAYER",
            Screen::Create => "CREATE GAME",
            Screen::Browser => "FIND GAME",
            Screen::Waiting => "WAITING ROOM",
            Screen::Armory => "CUSTOMISE",
            Screen::Livery => "COLOURS",
            Screen::Record => "YOUR STATS",
            Screen::Standings => "LEADERBOARD",
            Screen::Auth => "LOG IN",
            Screen::Config => "SETTINGS",
        }
    }

    /// Which page key on the rail is lit. `Boot` lights none — the rail is not
    /// drawn during a self test.
    fn rail(self) -> Option<usize> {
        Some(match self {
            Screen::Boot => return None,
            Screen::Main => 0,
            Screen::Solo | Screen::Trials | Screen::Campaign => 1,
            Screen::Net | Screen::Create | Screen::Browser | Screen::Waiting => 2,
            Screen::Armory | Screen::Livery => 3,
            Screen::Record | Screen::Standings | Screen::Auth => 4,
            Screen::Config => 5,
        })
    }

    /// Where `ESC` goes. `None` means there is no page below this one.
    fn back(self) -> Option<Screen> {
        Some(match self {
            Screen::Boot | Screen::Main => return None,
            Screen::Solo | Screen::Net | Screen::Armory | Screen::Record | Screen::Config => {
                Screen::Main
            }
            Screen::Trials | Screen::Campaign => Screen::Solo,
            Screen::Create | Screen::Browser => Screen::Net,
            Screen::Waiting => Screen::Net,
            Screen::Livery => Screen::Armory,
            Screen::Standings | Screen::Auth => Screen::Record,
        })
    }

    /// Whether this page needs a socket.
    ///
    /// The four pages of the network rail, which is also the set
    /// [`Menu::setup`] answers [`Mode::Multiplayer`] for — so the two cannot
    /// disagree about what "in the lobby's network half" means.
    fn is_network(self) -> bool {
        matches!(
            self,
            Screen::Net | Screen::Create | Screen::Browser | Screen::Waiting
        )
    }

    /// `SPACESHIPS_UI=<name>`.
    ///
    /// Only [`forced_screen`] calls it, and that is native-only — the browser
    /// has no environment. The tests do too, and they are native, so this is an
    /// `allow` on one target rather than a `cfg` that would hide it from them.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    fn parse(s: &str) -> Option<Screen> {
        Some(match s.to_ascii_lowercase().as_str() {
            "boot" => Screen::Boot,
            "main" => Screen::Main,
            "solo" => Screen::Solo,
            "trials" => Screen::Trials,
            "campaign" => Screen::Campaign,
            "net" | "multi" => Screen::Net,
            "create" => Screen::Create,
            "browser" | "find" | "rooms" => Screen::Browser,
            "waiting" | "room" | "crew" => Screen::Waiting,
            "armory" | "shop" => Screen::Armory,
            "livery" | "colours" | "colors" => Screen::Livery,
            "record" | "profile" => Screen::Record,
            "standings" | "leaderboard" => Screen::Standings,
            "auth" | "login" | "signin" => Screen::Auth,
            "config" | "settings" => Screen::Config,
            _ => return None,
        })
    }
}

/// `SPACESHIPS_UI`: which page to open on, or whether to open at all.
///
/// - `SPACESHIPS_UI=armory` — open on that page and **hold it**: no self test,
///   no auto-advance. One process, one capture, no clicking.
/// - `SPACESHIPS_UI=off` — never show the menu.
/// - unset — the product behaviour: self test, then the mission board.
///
/// With one exception. `sim_bridge.rs` reads `SPACESHIPS_MODE` to choose a
/// match and `cockpit.rs` reads `SPACESHIPS_COCKPIT` to sit the pilot down.
/// Either says the operator has already made the choice this menu exists to
/// offer, so the menu stands down and lets them fly — which keeps every
/// existing capture recipe in this crate working unchanged.
fn forced_screen() -> (Option<Screen>, bool) {
    #[cfg(target_arch = "wasm32")]
    {
        (None, true)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        match std::env::var("SPACESHIPS_UI") {
            Ok(v) if v.eq_ignore_ascii_case("off") || v == "0" => (None, false),
            Ok(v) => match Screen::parse(&v) {
                Some(s) => (Some(s), true),
                None => {
                    warn!("SPACESHIPS_UI={v} is not a page; opening on the mission board");
                    (Some(Screen::Main), true)
                }
            },
            Err(_) => {
                let flying = std::env::var_os("SPACESHIPS_MODE").is_some()
                    || std::env::var_os("SPACESHIPS_COCKPIT").is_some();
                (None, !flying)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// What the menu knows
// ---------------------------------------------------------------------------

/// Which solo tasking is armed. Distinct from [`Mode`] because the trial and
/// campaign entries are doors to another page rather than modes in themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SoloPick {
    Tutorial,
    Train,
    Skirmish,
}

/// Everything the player has chosen but not yet executed.
///
/// Deliberately *not* [`MatchSetup`]: that resource is `sim_bridge`'s input and
/// is written exactly once, at [`Action::Execute`], so a player toying with the
/// theatre selector never touches the simulation's configuration.
#[derive(Resource)]
struct Menu {
    pub(crate) open: bool,
    screen: Screen,
    /// Cursor position per page, so backing out and returning lands where you
    /// left. `#lobby` loses the cursor on every screen change.
    focus: [u8; Screen::ALL.len()],
    /// `SPACESHIPS_UI` pinned this page; do not auto-advance or accept `ESC`.
    pinned: bool,
    opened_at: f32,
    /// There is a match behind this menu to go back to.
    ///
    /// `ESC` in flight pauses — `sim_bridge::fixed_tick` freezes the world
    /// while the menu is up — and `ESC` again is meant to resume. It could not:
    /// the second press ran `Screen::back()`, which walks *down* the page tree,
    /// and from the main page there is nothing below, so it printed
    /// `NO PAGE BELOW` and left the pilot in a menu with no way out of it while
    /// their match sat frozen behind it.
    ///
    /// Set only by the pause path. A menu reopened because the match *ended*
    /// (`ReturnToLobby`) clears it, so `ESC` there cannot resume a match that
    /// is over.
    resumable: bool,

    solo: SoloPick,
    trial: u8,
    mission: u8,
    map: MapKind,
    /// "Secret Hard Mode". `MatchSetup::hard_mode`.
    hard: bool,
    /// Create-sortie: private room.
    private: bool,
    /// Create-sortie: auto-fill uneven teams with a bot. `#autoBotInput`, the
    /// toggle `CLAUDE.md` tells testers to clear to get an empty map.
    auto_bot: bool,
    sortie: u8,
    item: u8,
    /// Index into [`LIVERY`].
    hull: u8,
    accent: u8,
    /// Index into [`TRAIL_SHAPES`].
    trail: u8,
    scheme: u8,
    flags: [bool; Flag::ALL.len()],
    /// Music and effects, 0..=10.
    volume: [u8; 2],

    /// The footer message, and a counter so that re-issuing the same string
    /// still reads as a change to the model.
    ///
    /// A `String` rather than a `&'static str`, which is what it used to be.
    /// The footer now also prints things the *server* said — a rejected
    /// sign-in, a callsign already taken — and those are not known at compile
    /// time. It still allocates only when something happens: `notice_rev` is
    /// what [`MenuModel`] watches, and `say` is called on an action, never on a
    /// frame.
    notice: String,
    notice_rev: u16,

    /// What the socket last reported. See [`NetView`].
    net: NetView,
    /// A request that is waiting for the socket to finish opening.
    ///
    /// `flush_outbox` **drops** anything written while the socket is not open,
    /// exactly as the JS `readyState` guard does — so "press CREATE SORTIE"
    /// cannot simply write a `create` and hope. The handshake takes a round
    /// trip; this is the one frame of state that spans it, and
    /// [`follow_session`] sends it the moment [`Phase::Idle`] is reached.
    pending: Option<Pending>,

    /// What the REST client last reported. See [`AccountView`].
    acct: AccountView,
    /// What is typed into [`Screen::Auth`]'s two fields.
    form: AuthForm,
    /// Whether the cursor is resting on one of those fields, and the keyboard
    /// is therefore the form's rather than the aircraft's. Written by
    /// [`read_input`], read by [`swallow_typing`].
    typing: bool,
}

/// [`Screen::Auth`]'s two fields.
///
/// The only text input in the client. It is deliberately tiny: no caret, no
/// selection, no clipboard — a callsign is `[A-Za-z0-9]{3,20}` by the server's
/// own rule and a passphrase is typed once, so what is actually needed is
/// "append a character" and "remove the last one". Anything more is a text
/// editor, and a text editor rendered through a barrel-warped CRT shader with
/// no pointer resolution is not a thing to write casually.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct AuthForm {
    callsign: String,
    /// Never drawn, never logged, never sent anywhere but `/api/login` and
    /// `/api/register` over the body of a POST. [`AuthForm::masked`] is what
    /// reaches a node.
    secret: String,
    /// Bumped on every edit. [`MenuModel`] watches this rather than the text,
    /// which cannot go in a `Copy` model — the same trick as
    /// [`NetSession::rev`], and it has the pleasant side effect that the
    /// passphrase never enters the diffing path at all.
    rev: u16,
}

impl AuthForm {
    /// The passphrase as it is drawn.
    fn masked(&self) -> String {
        "*".repeat(self.secret.chars().count())
    }

    /// Whether these credentials are worth sending.
    ///
    /// The server's own rules (`server/db.js`'s `registerPilot`), checked here
    /// so a typo costs no round trip and the reason is specific. The server
    /// still checks: this is courtesy, not enforcement.
    fn complaint(&self) -> Option<&'static str> {
        if self.callsign.chars().count() < CALLSIGN_MIN {
            Some("CALLSIGN IS 3-20 LETTERS AND DIGITS")
        } else if self.secret.chars().count() < PASSWORD_MIN {
            Some("PASSWORD IS 6 CHARACTERS OR MORE")
        } else {
            None
        }
    }

    /// Appends what a key produced, if this field will have it.
    ///
    /// Returns whether anything changed, so the caller can bump [`Self::rev`]
    /// only on a real edit — an arrow key must not mark the menu dirty.
    fn push(&mut self, field: Field, text: &str) -> bool {
        let (buffer, max, keep): (&mut String, usize, fn(char) -> bool) = match field {
            Field::Callsign => (&mut self.callsign, CALLSIGN_MAX, |c: char| {
                c.is_ascii_alphanumeric()
            }),
            // A passphrase is whatever the pilot chose, so the only rule is
            // that it is printable: control characters here would be a header
            // or a JSON escape, never a character somebody meant to type.
            Field::Secret => (&mut self.secret, PASSWORD_MAX, |c: char| {
                c.is_ascii_graphic() || c == ' '
            }),
        };
        let mut changed = false;
        for c in text.chars().filter(|c| keep(*c)) {
            if buffer.chars().count() >= max {
                break;
            }
            buffer.push(c);
            changed = true;
        }
        changed
    }

    fn backspace(&mut self, field: Field) -> bool {
        let buffer = match field {
            Field::Callsign => &mut self.callsign,
            Field::Secret => &mut self.secret,
        };
        buffer.pop().is_some()
    }
}

/// Which of [`AuthForm`]'s two fields a control edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Callsign,
    Secret,
}

/// The part of [`Account`] the pages colour themselves from.
///
/// A `Copy`/`Eq` mirror, for exactly the reason [`NetView`] is one: [`model`]
/// compares the whole screen as a single value, and a `String` token cannot go
/// in it. [`follow_session`] is the only writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct AccountView {
    /// [`Account::rev`].
    rev: u32,
    signed_in: bool,
    /// Whether a request is in flight, which is what the auth page shows as
    /// `WORKING` and what stops a second sign-in being fired over the first.
    busy: bool,
}

/// A lobby request that could not go out yet. See [`Menu::pending`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pending {
    /// `list-rooms`.
    List,
    /// `create`, with whatever access and complement is armed when it goes.
    Create,
    /// `join`, by room code.
    Join(String),
}

/// The part of [`NetSession`] the pages colour themselves from.
///
/// A `Copy`/`Eq` mirror rather than a borrow of the resource, for the reason
/// the whole module is built around: [`model`] reduces the screen to one value
/// and compares it whole, and a `Vec<PlayerInfo>` cannot go in that value. The
/// counter can, and [`NetSession::rev`] moves whenever any of those rows does.
///
/// It is a *mirror*, not a second source of truth — [`follow_session`] is the
/// only writer and copies it wholesale. Anything that needs the rows themselves
/// (the browser's text, the crew room's seats) reads the resource directly, in
/// [`drive_menu`], behind this counter's comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct NetView {
    /// [`NetSession::rev`].
    rev: u32,
    /// [`Phase`] as an index, so the model stays integers.
    phase: u8,
    /// Rows in the browser.
    rooms: u8,
    /// Seats in the crew room.
    seats: u8,
    /// Whether this client may press launch.
    host: bool,
}

impl NetView {
    fn of(session: &NetSession) -> NetView {
        NetView {
            rev: session.rev,
            phase: match session.phase {
                Phase::Offline => 0,
                Phase::Idle => 1,
                Phase::Room => 2,
                Phase::Playing => 3,
            },
            #[allow(clippy::cast_possible_truncation)]
            rooms: session.rooms.len().min(SORTIE_ROWS) as u8,
            #[allow(clippy::cast_possible_truncation)]
            seats: session.players.len().min(SEAT_ROWS) as u8,
            host: session.host,
        }
    }
}

/// Rows the room browser draws. The server lists every open, unstarted room and
/// the JS browser scrolls; this one is a fixed pool, because a page built once
/// cannot grow and a scroll view on a CRT is the wrong instrument. Six is more
/// than the live server has ever had open at once.
const SORTIE_ROWS: usize = 6;

/// Seats the crew room draws. `server/index.js` has no cap, but a room is two
/// teams of five in every mode the game offers.
const SEAT_ROWS: usize = 10;

impl Default for Menu {
    fn default() -> Self {
        let (forced, enabled) = forced_screen();
        Menu {
            open: enabled,
            screen: forced.unwrap_or(Screen::Boot),
            focus: [0; Screen::ALL.len()],
            pinned: forced.is_some(),
            opened_at: 0.0,
            // Nothing to go back to before a match has ever been launched.
            resumable: false,
            solo: SoloPick::Skirmish,
            trial: 0,
            mission: 0,
            map: MapKind::Space,
            hard: false,
            private: false,
            auto_bot: true,
            sortie: 0,
            item: 6,
            // `customization.js:8`/`:11`'s defaults, as near as this palette
            // gets: a pale hull and a dark accent, which is the split
            // `is_accent` is looking for in the first place.
            hull: 0,
            accent: 7,
            trail: 0,
            scheme: 0,
            flags: Flag::DEFAULTS,
            volume: [6, 8],
            notice: "READY".to_owned(),
            notice_rev: 0,
            net: NetView::default(),
            pending: None,
            acct: AccountView::default(),
            form: AuthForm::default(),
            typing: false,
        }
    }
}

impl Menu {
    fn cursor(&self) -> u8 {
        self.focus[self.screen.index()]
    }

    fn say(&mut self, msg: impl Into<String>) {
        self.notice = msg.into();
        self.notice_rev = self.notice_rev.wrapping_add(1);
    }

    /// Mirrors the socket's view onto the menu.
    ///
    /// The one place [`NetView`] is written, and it does one thing besides
    /// copying: **it keeps the armed sortie inside the list it indexes.** The
    /// browser is a fixed pool of six rows over a list that changes under it,
    /// so a refresh that comes back shorter — somebody's room filled up, or
    /// started — would otherwise leave the selection pointing at a row that has
    /// just become empty, and `JOIN` would refuse a browser that is visibly
    /// showing rooms. That is the same failure [`arms_on_focus`] describes,
    /// arriving from the server's side rather than the cursor's.
    ///
    /// Guarded, because an unconditional write would mark [`Menu`] changed
    /// every frame and defeat [`MenuModel`]'s early out.
    fn watch(&mut self, view: NetView) {
        if self.net == view {
            return;
        }
        self.net = view;
        if self.sortie >= view.rooms {
            self.sortie = 0;
        }
    }

    fn go(&mut self, screen: Screen) {
        if self.pinned {
            return;
        }
        self.screen = screen;
        self.say(match screen {
            Screen::Standings => "CACHED - NO DATA LINK",
            Screen::Browser => "LOOKING FOR GAMES",
            Screen::Waiting => "WAITING FOR HOST",
            _ => "READY",
        });
    }

    /// The match this selection describes: `sim_bridge::MatchSetup`'s five
    /// fields, which are exactly `SPACESHIPS_MODE`, `MAP`, `SEED`, `HARD` and
    /// `CALLSIGN`.
    fn setup(&self, data: &LobbyData) -> MatchSetup {
        let mut setup = MatchSetup {
            map: self.map,
            hard_mode: self.hard,
            callsign: data.pilot.callsign.clone(),
            ..MatchSetup::default()
        };
        setup.mode = match self.screen {
            Screen::Trials => Mode::Trials(self.trial + 1),
            Screen::Campaign => Mode::Campaign(self.mission + 1),
            s if s.is_network() => Mode::Multiplayer,
            _ => match self.solo {
                SoloPick::Tutorial => Mode::Tutorial,
                SoloPick::Train => Mode::Training,
                SoloPick::Skirmish => Mode::Skirmish,
            },
        };
        setup
    }
}

/// A configuration toggle, ordered as it appears on [`Screen::Config`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Flag {
    /// `spaceships:pixelFilter`.
    Retro,
    /// `spaceships:ultraGraphics`.
    Ultra,
    /// "Show Stats".
    Stats,
    /// "Enemy Trails".
    Trails,
    /// `spaceships:viewMode`. Mirrored onto `cockpit.rs`'s [`ViewMode`] by
    /// [`mirror_cockpit_flag`], which also carries a `V` pressed in flight back
    /// here, so this row and the seat never disagree.
    Cockpit,
    /// "Secret Hard Mode". Mirrors [`Menu::hard`], so it can be set from either
    /// page exactly as the JS lets you.
    Hard,
}

impl Flag {
    const ALL: [Flag; 6] = [
        Flag::Retro,
        Flag::Ultra,
        Flag::Stats,
        Flag::Trails,
        Flag::Cockpit,
        Flag::Hard,
    ];
    const DEFAULTS: [bool; 6] = [true, false, false, true, false, false];

    fn label(self) -> &'static str {
        match self {
            Flag::Retro => "RETRO PIXEL FILTER",
            Flag::Ultra => "ULTRA GRAPHICS",
            Flag::Stats => "TELEMETRY READOUT",
            Flag::Trails => "ENEMY TRAILS",
            Flag::Cockpit => "COCKPIT VIEW",
            Flag::Hard => "HARD ENEMIES",
        }
    }
}

/// `input.js`'s three schemes.
const SCHEMES: [&str; 3] = ["MOUSE + KEYS", "KEYBOARD", "GAMEPAD"];

// ---------------------------------------------------------------------------
// The data seam
// ---------------------------------------------------------------------------

// The data seam itself — `LobbyData`, `PilotRecord`, `Standing`, `DataSource`
// — lives in `api.rs`, which is the module that fills it. The room browser's
// rows and the crew room's seats come off the socket instead, as
// `spaceships_protocol::RoomSummary` and `PlayerInfo`; see [`NetSession`].
//
// Nothing below this line knows how either of them arrived.

// ---------------------------------------------------------------------------
// The armory ladder
// ---------------------------------------------------------------------------

/// One rung.
///
/// The prices are a **balance decision, not a layout one**, which is why they
/// live in one table rather than in the builder: changing the economy is an
/// edit to this array and nothing else.
struct Rung {
    tier: u8,
    name: &'static str,
    class: &'static str,
    cost: u32,
    blurb: &'static str,
}

/// The tier headings.
const TIERS: [&str; 7] = [
    "T0  ISSUED",
    "T1  COLOURS",
    "T2  SECOND SHIP",
    "T3  MID",
    "T4  LATE",
    "T5  CHASE",
    "T6  PRESTIGE",
];

/// The ladder.
///
/// The problem this replaces, from `BACKLOG.md` section 8: five buys totalling
/// **1,400** and then the admin ship at **125,000** — a 250x gap with nothing in
/// it. A player clears the whole shop in one evening and then has nothing to
/// chase. The gap is filled with the thing the rest of the roadmap already pays
/// for: section 7 replaces the ship models with fighter jets, and several jets
/// unlocked in sequence *is* a ladder needing no new systems — `ship-model` is
/// already a protocol message and `unlock_admin_ship` is already a column.
///
/// The rules the shape encodes, which are what actually matter:
///
/// - **Every tier is visible from the start.** A locked rung shows its name,
///   its price and what it costs to reach; an empty shop with one absurd item
///   at the bottom is what made the old one feel like a wall.
/// - **A tier opens when the one below it is cleared**, so the ladder is
///   climbed rather than cherry-picked. See [`tier_open`].
/// - **Each tier has an airframe as its keystone**, with cheaper cosmetic rungs
///   beside it, so the pilot who wants the jet and the pilot who wants the
///   paint are both spending.
/// - **The admin ship does not move.** Its price was never the problem; the
///   emptiness beneath it was.
const ARMORY: [Rung; 16] = [
    Rung {
        tier: 0,
        name: "MK.I  LANCER",
        class: "SHIP",
        cost: 0,
        blurb: "Issued on enlistment. The hull every pilot learns on.",
    },
    Rung {
        tier: 1,
        name: "SAVE COLOURS",
        class: "COLOURS",
        cost: 50,
        blurb: "Persist a scheme across sorties instead of re-picking it.",
    },
    Rung {
        tier: 1,
        name: "TRAIL PROFILE",
        class: "COLOURS",
        cost: 200,
        blurb: "Five exhaust shapes, visible to every other pilot.",
    },
    Rung {
        tier: 1,
        name: "HULL COLOUR",
        class: "COLOURS",
        cost: 250,
        blurb: "Free choice of hull finish.",
    },
    Rung {
        tier: 1,
        name: "ACCENT COLOUR",
        class: "COLOURS",
        cost: 400,
        blurb: "Free choice of accent, below the luminance split.",
    },
    Rung {
        tier: 1,
        name: "TRAIL COLOUR",
        class: "COLOURS",
        cost: 500,
        blurb: "Free choice of trail colour.",
    },
    Rung {
        tier: 2,
        name: "MK.II  HALBERD",
        class: "SHIP",
        cost: 2_000,
        blurb: "Second hull. Heavier nose, the same flight model.",
    },
    Rung {
        tier: 2,
        name: "TRACER COLOUR",
        class: "ORDNANCE",
        cost: 1_200,
        blurb: "Bolt and beam tint. Reads at range; changes no damage.",
    },
    Rung {
        tier: 3,
        name: "MK.III  SABRE",
        class: "SHIP",
        cost: 8_000,
        blurb: "Third hull. Long canopy, the best forward view of the five.",
    },
    Rung {
        tier: 3,
        name: "COCKPIT: NIGHT",
        class: "DISPLAY",
        cost: 4_500,
        blurb: "Dark interior, red strip lighting. Cockpit view only.",
    },
    Rung {
        tier: 3,
        name: "PANEL: AMBER",
        class: "DISPLAY",
        cost: 3_000,
        blurb: "Amber instrument theme in place of the standard cyan.",
    },
    Rung {
        tier: 4,
        name: "MK.IV  WARDEN",
        class: "SHIP",
        cost: 25_000,
        blurb: "Fourth hull. Twin tails, the widest wing of the line.",
    },
    Rung {
        tier: 4,
        name: "NOSE ART",
        class: "MARKINGS",
        cost: 9_000,
        blurb: "Decal set applied forward of the canopy.",
    },
    Rung {
        tier: 4,
        name: "CALLSIGN",
        class: "MARKINGS",
        cost: 6_000,
        blurb: "Your callsign under the canopy rail and on the nameplate.",
    },
    Rung {
        tier: 5,
        name: "MK.V  REVENANT",
        class: "SHIP",
        cost: 60_000,
        blurb: "Fifth hull. The chase item: months of sorties, not one evening.",
    },
    Rung {
        tier: 6,
        name: "ADMIN SHIP",
        class: "SHIP",
        cost: 125_000,
        blurb: "Unchanged at 125,000. A genuine flex should stay out of reach.",
    },
];

/// Whether a tier can be bought from yet: every rung of the tier below is held,
/// or it is tier 0.
fn tier_open(tier: u8, owned: u32) -> bool {
    if tier == 0 {
        return true;
    }
    ARMORY
        .iter()
        .enumerate()
        .filter(|(_, r)| r.tier == tier - 1)
        .all(|(i, _)| owned & (1 << i) != 0)
}

/// What a rung says on its right-hand side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stock {
    Held,
    /// Affordable now.
    Available,
    /// Unlocked, but the balance is short.
    Short,
    /// The tier below is not cleared.
    Locked,
}

fn stock(index: usize, data: &LobbyData) -> Stock {
    let owned = data.pilot.owned;
    if owned & (1 << index) != 0 {
        return Stock::Held;
    }
    let rung = &ARMORY[index];
    if !tier_open(rung.tier, owned) {
        Stock::Locked
    } else if data.pilot.credits >= rung.cost {
        Stock::Available
    } else {
        Stock::Short
    }
}

// ---------------------------------------------------------------------------
// Launching
// ---------------------------------------------------------------------------

/// The lobby's answer to "which match".
///
/// `sim_bridge.rs` says of [`MatchSetup`]: *"There is no lobby in the Bevy
/// client yet, so it comes from the environment. When a lobby lands it should
/// write this resource and rebuild `SimWorld` from it; nothing else needs to
/// change."* This is that lobby, and [`apply`] does write the resource — but
/// **rebuilding `SimWorld` is not this module's to do**. `scene.rs` spawns the
/// asteroid field once in `Startup` from the world that existed then, so
/// swapping the world from here would leave the old field on screen against a
/// new one in the simulation.
///
/// So the handover is: this module writes [`MatchSetup`] and sends this
/// message; `sim_bridge` reads it. What it needs to add is one system —
///
/// ```text
/// fn relaunch(
///     mut msgs: MessageReader<LaunchRequest>,
///     mut world: ResMut<SimWorld>,
///     mut roster: ResMut<Roster>,
///     mut setup: ResMut<MatchSetup>,
/// ) {
///     let Some(req) = msgs.read().last() else { return };
///     *setup = req.setup.clone();
///     let (w, r) = new_match(&setup);
///     *world = SimWorld(w);
///     *roster = r;
/// }
/// ```
///
/// — plus whatever `scene.rs` needs to rebuild its static geometry, which is
/// the real work and is that module's. `new_match` is already `pub`, so the
/// system above is the whole simulation-side change.
///
/// `online` is the half `new_match` cannot serve: `Mode::Multiplayer` is not
/// solo, `new_match` `debug_assert!`s that it is, and the spawns come from the
/// server's `start` message rather than from a seed. It is carried here so the
/// message is complete when that path lands.
///
/// `dead_code` on the fields for the same reason `net.rs` allows it on
/// `FromServer`: this module is the *producer*, and a lobby that computed the
/// selection and then dropped it so the compiler stayed quiet would be worse
/// than a warning.
/// Whether the lobby is currently covering the screen.
///
/// `hud.rs` reads this so the flight overlay stands down behind the menu, the
/// same way it stands down in the cockpit. A one-field mirror rather than
/// making [`Menu`] public, so nothing outside this module can reach into the
/// menu's internals — and it is only written when the value actually changes,
/// which keeps `hud.rs`'s no-per-frame-writes property intact.
#[derive(Resource, Default)]
pub struct LobbyOpen(pub bool);

fn publish_lobby_open(menu: Res<Menu>, mut out: ResMut<LobbyOpen>) {
    if out.0 != menu.open {
        out.0 = menu.open;
    }
}

/// Sends the two faders to the mixer.
///
/// [`audio::MixerLevels`] says "write these from wherever the settings UI ends
/// up living" — and nothing ever did. It was `init_resource`'d from its
/// `Default` and read by two systems, so both sliders moved a number in
/// [`Menu::volume`], redrew themselves, and changed nothing you could hear.
///
/// `Menu::volume` is 0..=10 per `Action::Volume`; the mixer wants 0.0..=1.0.
/// Guarded so an unchanged mix is not written every frame, which is the same
/// discipline `hud.rs` keeps for `bevy_ui` and this module keeps for
/// [`LobbyOpen`].
fn publish_volume(menu: Res<Menu>, mut levels: ResMut<crate::audio::MixerLevels>) {
    let music = f32::from(menu.volume[0]) / 10.0;
    let sfx = f32::from(menu.volume[1]) / 10.0;
    if levels.music != music || levels.sfx != sfx {
        levels.music = music;
        levels.sfx = sfx;
    }
}

/// Keeps [`Flag::Cockpit`] and [`ViewMode::first_person`] equal, both ways.
///
/// The two used to be unrelated: `cockpit.rs` read `SPACESHIPS_COCKPIT` once at
/// startup and `V` moved it thereafter, while this row moved a bit in
/// [`Menu::flags`] that nothing read — so a player who switched the display's
/// `COCKPIT VIEW` on got no cockpit, and one who pressed `V` found the row
/// still claiming they were in the chase camera.
///
/// [`ViewMode`] is the authority, because it is what every system in
/// `cockpit.rs` reads and what the env var seeds. The rule is therefore:
///
/// - the row moved (it differs from what this system last left there) — the
///   player just toggled it, so push it onto the view;
/// - otherwise — pull the view onto the row, which is how `V` gets back here.
///
/// The first run seeds rather than pushes, so `SPACESHIPS_COCKPIT=1` is
/// reflected on the row instead of being immediately overwritten by
/// [`Flag::DEFAULTS`]. Writes are guarded on both sides: an unconditional one
/// would mark [`Menu`] changed every frame and defeat [`MenuModel`]'s early
/// out.
fn mirror_cockpit_flag(
    mut menu: ResMut<Menu>,
    mut view: ResMut<ViewMode>,
    mut last: Local<Option<bool>>,
) {
    let i = Flag::Cockpit as usize;
    let row = menu.flags[i];

    let Some(was) = *last else {
        // Startup: the view mode already holds the env override or the default,
        // and the row has never been touched.
        if row != view.first_person {
            menu.flags[i] = view.first_person;
        }
        *last = Some(view.first_person);
        return;
    };

    if row != was {
        if view.first_person != row {
            view.first_person = row;
        }
    } else if row != view.first_person {
        menu.flags[i] = view.first_person;
    }
    *last = Some(menu.flags[i]);
}

/// Reopens the menu from outside this module.
///
/// The inverse of [`LaunchRequest`], and deliberately the same shape of thing:
/// [`Menu`] stays private, and the only way in is a message. `hud.rs` sends
/// this when a finished match has had its result on screen long enough — the
/// clock used to reach zero and simply sit there, because nothing anywhere
/// consumed `SimEvent::MatchEnded`.
#[derive(Message, Debug, Clone, Copy)]
pub struct ReturnToLobby;

fn reopen_menu(mut requests: MessageReader<ReturnToLobby>, mut menu: ResMut<Menu>) {
    if requests.read().next().is_none() {
        return;
    }
    requests.clear();
    // Same state `Escape` produces (`toggle_menu`), so there is one definition
    // of "the menu is up" and returning from a match cannot drift from it.
    if !menu.open {
        menu.open = true;
    }
    // The match this menu came back from is over, so `ESC` must not offer to
    // return to it.
    menu.resumable = false;
}

#[derive(Message, Debug, Clone)]
#[allow(dead_code)]
pub struct LaunchRequest {
    /// The five fields `SPACESHIPS_MODE`/`MAP`/`SEED`/`HARD`/`CALLSIGN` set.
    pub setup: MatchSetup,
    /// True when the match is served by the network rather than built locally.
    pub online: bool,
}

// ---------------------------------------------------------------------------
// Controls
// ---------------------------------------------------------------------------

/// What activating a control does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Go(Screen),
    /// Leave the display and fly.
    Execute,

    SetSolo(SoloPick),
    SetTrial(u8),
    SetMission(u8),
    SetMap(MapKind),
    SetPrivate(bool),
    SetAutoBot(bool),
    SetScheme(u8),
    SetSortie(u8),
    SetItem(u8),
    SetHull(u8),
    SetAccent(u8),
    SetTrail(u8),
    Toggle(Flag),
    /// Channel 0 = music, 1 = effects.
    Volume(u8),
    Requisition,

    // -- network ------------------------------------------------------------
    //
    // Seven actions, one per thing the server can be asked. They are separate
    // variants rather than one `Net(ClientMessage)` because `Action` is `Copy`
    // and `Eq` — [`control_state`] matches on it and [`MenuNodes::applied`]
    // compares the result — and `ClientMessage` is neither.
    /// Step [`crate::net::ENDPOINTS`] and reconnect.
    CycleEndpoint,
    /// `create`: open a room with the armed access and complement.
    Transmit,
    /// `list-rooms`: refresh the browser.
    Refresh,
    /// `join`: enter the room under the cursor.
    JoinSortie,
    /// `start`: host only, and the last thing the lobby does before flight.
    LaunchNet,
    /// `leave`: give the seat up without closing the socket.
    LeaveRoom,

    // -- the account --------------------------------------------------------
    //
    // Five more, and the same reasoning as the network seven: `Action` is
    // `Copy` and `Eq`, and `api::ApiRequest` carries a `String` and is neither.
    /// Put the caret in one of [`Screen::Auth`]'s two fields.
    Edit(Field),
    /// `POST /api/login` with what is typed.
    SignIn,
    /// `POST /api/register`, and then sign in with the same credentials.
    Enlist,
    /// Drop the token and go back to being a guest.
    SignOut,
    /// Re-read the record, the balance, the ladder and the standings.
    RefreshData,
}

/// One focusable row, and the entities whose colour expresses its state.
///
/// No border and no background at rest — the cursor tick and the text colour
/// carry the state. Boxes inside boxes is the fastest way to make a page look
/// busy, and there are none here.
struct ControlDef {
    screen: Screen,
    action: Action,
    root: Entity,
    /// The 2px cursor tick down the left edge: `cockpit.rs`'s annunciator idiom
    /// at menu scale, a lamp that is always there and changes colour.
    tick: Entity,
    label: Entity,
    /// Right-hand value, or [`Entity::PLACEHOLDER`].
    value: Entity,
}

const ST_FOCUS: u8 = 1;
const ST_SELECTED: u8 = 2;
const ST_DISABLED: u8 = 4;
const ST_HELD: u8 = 8;

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/// Steps the sweep is quantised to.
///
/// At [`SWEEP_RATE`] a turn takes 2.85 s, so 96 steps is ~34 writes a second to
/// one [`UiTransform`], against 144 at a display's rate — and a `UiTransform` is
/// applied after layout, so none of them is a relayout. The arm moves 3.75
/// degrees between steps, under six pixels at the scope's radius.
const SWEEP_STEPS: u8 = 96;
/// Radians per second, from `cockpit.rs`'s `SWEEP_RATE` — the same scope.
const SWEEP_RATE: f32 = 2.2;
/// Caution blink, one direction. `hud.rs`'s `msl-lock-blink` is 0.25 s; a
/// caution lamp is less urgent than a missile.
const CAUTION_HALF_PERIOD: f32 = 0.5;

/// Lines in the self test.
const BOOT_LINES: usize = 7;
/// Seconds per line.
const BOOT_STEP: f32 = 0.2;
/// Seconds the completed test is held before the mission board comes up.
const BOOT_HOLD: f32 = 0.8;

/// Everything the pixels can show, reduced to something `Copy` and `Eq`.
///
/// The rule from `hud.rs`: no floats, every continuous quantity quantised to
/// what the screen can resolve, so a frame in which nothing happened compares
/// equal and [`drive_menu`] returns without touching a component. The two
/// time-driven fields are **forced to zero when their condition is false** — a
/// phase that ticked unconditionally would make the model differ every frame
/// and quietly reintroduce the bug.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct MenuModel {
    /// False in flight, and everything below is then constant.
    open: bool,
    screen: u8,
    /// Whether the rails are drawn — false during the self test.
    chrome: bool,
    focus: u8,
    /// Every selection the pages colour themselves from, packed into one
    /// integer so the control pass is guarded by a single comparison.
    sel: u64,
    credits: u32,
    /// Self-test progress, 0..=[`BOOT_LINES`].
    boot: u8,
    /// Sweep angle in [`SWEEP_STEPS`]ths of a turn. **Zero unless the scope is
    /// on screen.**
    sweep: u8,
    /// The lit half of the caution square wave. **False unless something is
    /// actually cautioning.**
    caution: bool,
    /// [`ConnState`] as an index.
    link: u8,
    /// Whole seconds the display has been up. **Zero when closed.**
    clock: u32,
    /// Footer message generation.
    notice: u16,
    /// What the socket last reported. **Zeroed when closed**, for the same
    /// reason `sweep` and `clock` are: a `players` broadcast landing every
    /// second would otherwise make the model differ on every frame of a live
    /// match and defeat the early-out this whole module is shaped around.
    net: NetView,
    /// Frames in and out, summed. **Zero unless the `DATA LINK` panel is on
    /// screen**, which is the only thing that draws it — it moves at 20 Hz
    /// while a match runs and belongs to no other page.
    frames: u32,
    /// [`LobbyData::rev`], which moves whenever a reply lands. The dossier, the
    /// standings and the trial bests are all behind this one comparison, for
    /// the same reason the roster is behind `NetView::rev`: none of them is
    /// `Copy`.
    data_rev: u32,
    /// The `FEED` lamp's source, as an index — so a refresh that changed
    /// nothing but the *provenance* still repaints the lamp.
    feed: u8,
    /// Who is signed in. **Zeroed when closed**, like `net`.
    acct: AccountView,
    /// [`AuthForm::rev`]. The text itself never enters the model, which is
    /// incidentally why the passphrase cannot reach a diff.
    form: u16,
}

/// The model [`drive_menu`] last wrote. `None` until the first frame, which is
/// what makes that frame write everything.
#[derive(Resource, Default)]
struct Applied(Option<MenuModel>);

/// Packs every selection into one integer.
///
/// Only equality is ever asked of it, so the widths only have to be wide enough
/// for the values and narrow enough to fit — and the second half is the one
/// that bites. Adding the livery fields took the total past 64 and silently
/// shifted the first two out of the top, so a page stopped repainting when the
/// solo tasking changed. [`SELECTION_BITS`] and the test below exist so the
/// next field to be added fails loudly instead.
fn selection_key(m: &Menu, data: &LobbyData) -> u64 {
    let mut k: u64 = 0;
    let mut used = 0;
    let mut push = |bits: u64, width: u32| {
        used += width;
        k = (k << width) | (bits & ((1 << width) - 1));
    };
    push(
        match m.solo {
            SoloPick::Tutorial => 0,
            SoloPick::Train => 1,
            SoloPick::Skirmish => 2,
        },
        2,
    );
    push(u64::from(m.trial), 2);
    push(u64::from(m.mission), 2);
    push(u64::from(m.map == MapKind::Terrain), 1);
    push(u64::from(m.hard), 1);
    push(u64::from(m.private), 1);
    push(u64::from(m.auto_bot), 1);
    push(u64::from(m.sortie), 3);
    push(u64::from(m.item), 4);
    push(u64::from(m.hull), 3);
    push(u64::from(m.accent), 3);
    push(u64::from(m.trail), 3);
    push(u64::from(m.scheme), 2);
    for f in m.flags {
        push(u64::from(f), 1);
    }
    push(u64::from(m.volume[0]), 4);
    push(u64::from(m.volume[1]), 4);
    push(u64::from(data.pilot.owned), 16);
    debug_assert_eq!(used, SELECTION_BITS, "the packing widths moved");
    debug_assert!(used <= 64, "the selection key has overflowed");
    k
}

/// How many bits [`selection_key`] packs. Must stay at or under 64.
const SELECTION_BITS: u32 = 2 + 2 + 2 + 1 + 1 + 1 + 1 + 3 + 4 + 3 + 3 + 3 + 2 + 6 + 4 + 4 + 16;

/// Reduces the world to a [`MenuModel`].
fn model(m: &Menu, data: &LobbyData, net: &NetStatus, now: f32) -> MenuModel {
    if !m.open {
        // One constant, so the whole module is a single comparison in flight.
        return MenuModel::default();
    }
    let up = (now - m.opened_at).max(0.0);
    let booting = m.screen == Screen::Boot;

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let boot = if booting {
        ((up / BOOT_STEP) as usize).min(BOOT_LINES) as u8
    } else {
        BOOT_LINES as u8
    };

    let link = match net.state {
        ConnState::Offline => 0,
        ConnState::Connecting => 1,
        ConnState::Online => 2,
        ConnState::Retrying => 3,
        ConnState::Failed => 4,
    };
    // What can raise a caution: no data link, or numbers that are not the
    // server's current answer — never fetched, or fetched and since gone stale.
    let cautioning = !booting && (net.state != ConnState::Online || data.source.is_suspect());

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let sweep = if booting {
        0
    } else {
        let turns = up * SWEEP_RATE / std::f32::consts::TAU;
        ((turns.fract() * f32::from(SWEEP_STEPS)) as u8) % SWEEP_STEPS
    };

    MenuModel {
        open: true,
        screen: m.screen as u8,
        chrome: !booting,
        focus: m.cursor(),
        sel: selection_key(m, data),
        credits: data.pilot.credits,
        boot,
        sweep,
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        caution: cautioning && ((up / CAUTION_HALF_PERIOD) as u32).is_multiple_of(2),
        link,
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        clock: up as u32,
        notice: m.notice_rev,
        net: m.net,
        frames: if m.screen == Screen::Net {
            #[allow(clippy::cast_possible_truncation)]
            let total = net.sent.saturating_add(net.received) as u32;
            total
        } else {
            0
        },
        data_rev: data.rev,
        feed: match data.source {
            DataSource::Placeholder => 0,
            DataSource::Public => 1,
            DataSource::Live => 2,
            DataSource::Stale => 3,
        },
        acct: m.acct,
        form: m.form.rev,
    }
}

// ---------------------------------------------------------------------------
// Handles
// ---------------------------------------------------------------------------

/// Every entity [`drive_menu`] writes to. Built once, never added to.
#[derive(Resource)]
struct MenuNodes {
    /// The off-screen tree's root.
    root: Entity,
    /// The full-screen node the tube is drawn on, and the camera feeding it.
    tube: Entity,
    camera: Entity,
    /// The render target, so [`fit_tube`] can resize it with the window.
    target: Handle<Image>,
    /// The tube's material, rewritten only on a resize.
    material: Handle<Crt>,

    chrome: Entity,
    /// The right-hand third of the backdrop, lifted on the livery page so the
    /// airframe behind it shows. See [`build_menu`].
    backdrop: Entity,
    pages: [Entity; Screen::ALL.len()],
    title: Entity,
    credits: Entity,
    sweep: Entity,
    lamps: [Entity; LAMPS.len()],
    lamp_values: [Entity; LAMPS.len()],
    notice: Entity,
    clock: Entity,
    boot_status: [Entity; BOOT_LINES],
    boot_fill: Entity,
    /// [`Screen::Main`]'s armed line.
    armed: Entity,
    /// [`Screen::Config`]'s two level readouts.
    volume: [Entity; 2],
    /// [`Screen::Solo`]'s brief values.
    brief: [Entity; 3],
    /// [`Screen::Armory`]'s detail: name, cost, class + status, blurb.
    detail: [Entity; 4],
    /// Everything the four network pages write. See [`NetNodes`].
    net: NetNodes,
    /// Everything the pilot's own pages write. See [`DossierNodes`].
    dossier: DossierNodes,
    controls: Vec<ControlDef>,
    /// Last-applied state byte per control.
    applied: Vec<u8>,
}

/// The handles the four network pages hand back.
///
/// One struct rather than seven more out-parameters on [`build_pages`], which
/// already carries four.
#[derive(Debug, Clone, Copy)]
struct NetNodes {
    /// [`Screen::Net`]'s `DATA LINK` values: endpoint, identity, frames.
    link_values: [Entity; 3],
    /// [`Screen::Waiting`]'s room code and access line.
    room_code: Entity,
    room_access: Entity,
    /// [`Screen::Waiting`]'s seat pool.
    seats: [SeatNodes; SEAT_ROWS],
    /// Control indices for the rows whose *text* is live, so [`drive_menu`] does
    /// not have to search [`MenuNodes::controls`] for them by action.
    sortie_rows: [usize; SORTIE_ROWS],
    launch_row: usize,
    endpoint_row: usize,
}

impl NetNodes {
    const EMPTY: NetNodes = NetNodes {
        link_values: [Entity::PLACEHOLDER; 3],
        room_code: Entity::PLACEHOLDER,
        room_access: Entity::PLACEHOLDER,
        seats: [SeatNodes::EMPTY; SEAT_ROWS],
        sortie_rows: [usize::MAX; SORTIE_ROWS],
        launch_row: usize::MAX,
        endpoint_row: usize::MAX,
    };
}

/// Rows the standings draw.
///
/// `GET /api/leaderboard` returns the top fifty. Ten is what fits on the tube
/// at a size the phosphor bleed leaves legible, and a scroll view on a CRT is
/// the wrong instrument — the same reasoning as [`SORTIE_ROWS`]. The rows past
/// the end of a shorter list are hidden rather than blanked.
const STANDING_ROWS: usize = 10;

/// The kill-mark tally's pool, one mark per ten kills.
///
/// Fixed, and hidden down to the count, because the tree is built once: a
/// dossier that respawned its marks on every refresh would be the per-frame
/// churn this module is shaped to avoid, and 70 marks is 700 kills, which is
/// past `Captain` on the live ladder.
const KILL_MARKS: usize = 70;

/// Everything the pilot's own pages write.
///
/// One struct rather than nine more out-parameters on [`build_pages`], for the
/// same reason [`NetNodes`] is one. These pages used to be built *once* from
/// [`LobbyData::placeholder`] and never touched again — which was fine while
/// the data could not change and is exactly wrong now that it arrives over the
/// network a second after the page does.
#[derive(Debug, Clone, Copy)]
struct DossierNodes {
    /// [`Screen::Main`]'s callsign and the rank line under it.
    board_name: Entity,
    board_line: Entity,
    /// [`Screen::Record`]'s, likewise.
    record_name: Entity,
    record_line: Entity,
    /// The six numbers in the record's grid, in build order.
    stats: [Entity; 6],
    /// The kill-mark pool, shown down to `kills / 10`.
    marks: [Entity; KILL_MARKS],
    /// [`Screen::Standings`]'s pool: the row, then its five columns.
    standings: [StandingNodes; STANDING_ROWS],
    /// [`Screen::Auth`]: the two field values, and the line that says who is
    /// signed in.
    fields: [Entity; 2],
    identity: Entity,
    /// Control indices whose *value* is live: the four circuits' best times,
    /// the three operations' state, and every rung's `HELD`/price/`LOCKED`.
    trial_rows: [usize; 4],
    mission_rows: [usize; 3],
    rung_rows: [usize; ARMORY.len()],
    /// The seven tier headings, which go dead above the ladder's open tier.
    tier_captions: [Entity; TIERS.len()],
}

impl DossierNodes {
    const EMPTY: DossierNodes = DossierNodes {
        board_name: Entity::PLACEHOLDER,
        board_line: Entity::PLACEHOLDER,
        record_name: Entity::PLACEHOLDER,
        record_line: Entity::PLACEHOLDER,
        stats: [Entity::PLACEHOLDER; 6],
        marks: [Entity::PLACEHOLDER; KILL_MARKS],
        standings: [StandingNodes::EMPTY; STANDING_ROWS],
        fields: [Entity::PLACEHOLDER; 2],
        identity: Entity::PLACEHOLDER,
        trial_rows: [usize::MAX; 4],
        mission_rows: [usize::MAX; 3],
        rung_rows: [usize::MAX; ARMORY.len()],
        tier_captions: [Entity::PLACEHOLDER; TIERS.len()],
    };
}

/// One standings row's writable parts.
#[derive(Debug, Clone, Copy)]
struct StandingNodes {
    /// The row itself, hidden with `Display::None` past the end of the list.
    root: Entity,
    position: Entity,
    callsign: Entity,
    rank: Entity,
    kills: Entity,
    wins: Entity,
}

impl StandingNodes {
    const EMPTY: StandingNodes = StandingNodes {
        root: Entity::PLACEHOLDER,
        position: Entity::PLACEHOLDER,
        callsign: Entity::PLACEHOLDER,
        rank: Entity::PLACEHOLDER,
        kills: Entity::PLACEHOLDER,
        wins: Entity::PLACEHOLDER,
    };
}

/// One crew-room seat's writable parts.
#[derive(Debug, Clone, Copy)]
struct SeatNodes {
    /// The row itself, hidden with `Display::None` when the seat is empty.
    root: Entity,
    /// The 2 px team stripe: cyan for team 0, red for team 1.
    tick: Entity,
    name: Entity,
    /// `LEAD` on the host's row, blank on everyone else's.
    lead: Entity,
}

impl SeatNodes {
    const EMPTY: SeatNodes = SeatNodes {
        root: Entity::PLACEHOLDER,
        tick: Entity::PLACEHOLDER,
        name: Entity::PLACEHOLDER,
        lead: Entity::PLACEHOLDER,
    };
}

/// The annunciator strip: three lamps, not a grid of six.
const LAMPS: [&str; 3] = ["LINK", "FEED", "ADV"];

// ---------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------

/// Carries the fonts and the control index, so a page body reads as layout
/// rather than as plumbing. The role `cockpit.rs`'s `Bld` plays.
struct Ui<'a> {
    f: &'a Fonts,
    controls: &'a mut Vec<ControlDef>,
}

impl Ui<'_> {
    fn control(&mut self, def: ControlDef) {
        self.controls.push(def);
    }
}

fn row(gap_px: f32) -> Node {
    Node {
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::Center,
        column_gap: px(gap_px),
        ..default()
    }
}

fn col(gap_px: f32) -> Node {
    Node {
        flex_direction: FlexDirection::Column,
        row_gap: px(gap_px),
        ..default()
    }
}

/// A page's primary column.
///
/// **Fixed width on purpose.** A content-sized column takes the width of its
/// widest child and wraps every longer one, which is what put "NETWORK
/// OPERATIONS" on two lines under "SINGLE PLAYER" in the first pass.
fn page_col(width_pct: f32) -> Node {
    Node {
        width: percent(width_pct),
        flex_direction: FlexDirection::Column,
        ..default()
    }
}

fn grow() -> Node {
    Node {
        flex_grow: 1.0,
        min_width: px(0),
        min_height: px(0),
        ..default()
    }
}

fn gap(h: f32) -> Node {
    Node {
        height: px(h),
        min_height: px(h),
        ..default()
    }
}

/// A one-pixel rule. There are four in the whole design.
fn rule() -> impl Bundle {
    (
        Node {
            width: percent(100),
            height: px(HAIR),
            min_height: px(HAIR),
            ..default()
        },
        BackgroundColor(pal::rgba(pal::GRID, 0.75)),
    )
}

/// A caption: upper case, tracked, the panel's legend colour.
///
/// Below [`MIN_DOT_SIZE`] this drops to Orbitron rather than scaling the dot
/// grid down past the point where the tube can resolve it.
fn caption(f: &Fonts, text: &str, size: f32, colour: Color) -> impl Bundle {
    tracked(f, text, size, colour, size * 0.18)
}

/// A caption with the tracking opened out, for the few strings that are the
/// page rather than a label on it.
fn tracked(f: &Fonts, text: &str, size: f32, colour: Color, track: f32) -> impl Bundle {
    let font = if size < MIN_DOT_SIZE {
        f.small(size, 600)
    } else {
        f.head(size, 700)
    };
    (
        Text::new(text.to_owned()),
        font,
        TextColor(colour),
        LetterSpacing::Px(track),
    )
}

/// A readout.
fn readout(f: &Fonts, text: &str, size: f32, colour: Color) -> impl Bundle {
    let font = if size < MIN_DOT_SIZE {
        f.small(size, 500)
    } else {
        f.num(size, 450)
    };
    (
        Text::new(text.to_owned()),
        font,
        TextColor(colour),
        LineHeight::RelativeToFont(1.45),
    )
}

/// The wordmark and the callsign. The one place Orbitron is used at size.
fn wordmark(f: &Fonts, text: &str, size: f32, colour: Color, track: f32) -> impl Bundle {
    (
        Text::new(text.to_owned()),
        f.brand(size, 800),
        TextColor(colour),
        LetterSpacing::Px(track),
    )
}

/// A section heading: one small caption with air under it, and no rule. The gap
/// is the grouping.
/// What a tasking page's rows do, said once, in the dimmest thing on the page.
///
/// The same words on all three, because they are the same promise: the row
/// under the cursor is the match that starts. Deliberately a bundle rather than
/// a control — see the call sites.
fn hint(f: &Fonts) -> impl Bundle {
    tracked(f, "SELECT TO LAUNCH", 8.0, dim(0.35), 2.4)
}

fn section(parent: &mut ChildSpawnerCommands, f: &Fonts, text: &str) {
    parent.spawn((
        caption(f, text, 9.0, dim(0.7)),
        Node {
            margin: UiRect::bottom(px(8)),
            ..default()
        },
    ));
}

/// A selectable row.
///
/// Not a [`Button`], and carries no marker component tying the entity back to
/// its [`ControlDef`]. Both were there to be picked up by `Interaction`, which
/// cannot fire in a tree rendered to an image — see the module docs. The row is
/// found the other way round now: [`MenuNodes::controls`] already holds the
/// root entity, so [`Pointer::rect`] asks for the rect it wants rather than
/// waiting to be told about a hover that never comes.
#[allow(clippy::too_many_arguments)]
fn control_row(
    parent: &mut ChildSpawnerCommands,
    ui: &mut Ui,
    screen: Screen,
    action: Action,
    label: &str,
    value: Option<&str>,
    size: f32,
) {
    let f = ui.f.clone();
    let mut tick = Entity::PLACEHOLDER;
    let mut label_e = Entity::PLACEHOLDER;
    let mut value_e = Entity::PLACEHOLDER;

    let root = parent
        .spawn((
            Node {
                width: percent(100),
                min_height: px(size * 1.9),
                align_items: AlignItems::Center,
                column_gap: px(12),
                ..default()
            },
            BackgroundColor(Color::NONE),
        ))
        .with_children(|r| {
            tick = r
                .spawn((
                    Node {
                        width: px(2),
                        min_width: px(2),
                        height: px(size * 1.1),
                        ..default()
                    },
                    BackgroundColor(pal::rgba(pal::GRID, 0.6)),
                ))
                .id();
            label_e = r
                .spawn((
                    caption(&f, label, size, dim(0.75)),
                    Node {
                        flex_grow: 1.0,
                        min_width: px(0),
                        ..default()
                    },
                ))
                .id();
            if let Some(v) = value {
                value_e = r.spawn(readout(&f, v, size * 0.8, dim(0.5))).id();
            }
        })
        .id();

    ui.control(ControlDef {
        screen,
        action,
        root,
        tick,
        label: label_e,
        value: value_e,
    });
}

/// A label/value line. No box, no rule — two columns and a gap.
///
/// Returns the value's entity, for the handful of lines that are live and are
/// rewritten by [`drive_menu`]. Most callers drop it: a line whose value never
/// changes needs no handle, and keeping one would put it in [`MenuNodes`] where
/// it would read as something that does.
fn kv(parent: &mut ChildSpawnerCommands, f: &Fonts, k: &str, v: &str, colour: Color) -> Entity {
    let mut value = Entity::PLACEHOLDER;
    parent
        .spawn(Node {
            width: percent(100),
            flex_direction: FlexDirection::Row,
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Center,
            min_height: px(22),
            ..default()
        })
        .with_children(|r| {
            r.spawn(caption(f, k, 8.0, dim(0.5)));
            value = r.spawn(readout(f, v, 11.0, colour)).id();
        });
    value
}

/// `1234567` as `1,234,567`. `format!` has no grouping flag.
fn thousands(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A trial best as `m:ss.hh`, or the empty slate. `trial{1..4}_best` is a
/// nullable REAL, and `null` means "no time set" rather than "zero".
fn fmt_trial(best: Option<f32>) -> String {
    match best {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Some(t) => {
            let total = (t * 100.0).round() as u32;
            format!(
                "{}:{:02}.{:02}",
                total / 6000,
                total / 100 % 60,
                total % 100
            )
        }
        None => "-:--.--".to_owned(),
    }
}

/// Writes one setting to wherever this build keeps them.
///
/// The mirror of `scene.rs::read_setting`, and deliberately the *same* keys, so
/// a colour picked here is the colour that flies. On the web that is
/// `localStorage`. **Natively there is nowhere to put it** — `scene.rs` reads
/// an environment variable on that side, which a running process cannot write
/// for its own next launch, so the choice lasts as long as the process. That is
/// a missing profile store, not something to invent a third scheme for here.
#[cfg(target_arch = "wasm32")]
fn save_setting(key: &str, value: &str) {
    if let Some(store) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = store.set_item(key, value);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn save_setting(key: &str, value: &str) {
    debug!("{key} = {value} (not persisted: the native build has no profile store)");
}

/// A colour, in the `#rrggbb` form `customization.js` writes.
fn save_colour(key: &str, hex: u32) {
    save_setting(key, &format!("#{hex:06x}"));
}

fn display_of(visible: bool) -> Display {
    if visible {
        Display::Flex
    } else {
        Display::None
    }
}

// ---------------------------------------------------------------------------
// Building the tree
// ---------------------------------------------------------------------------

/// The self test's lines, and what each reports.
const BOOT_TEST: [(&str, &str); BOOT_LINES] = [
    ("SETTINGS", "OK"),
    ("INERTIAL REFERENCE", "ALIGNED"),
    ("FLIGHT MODEL", "OK"),
    ("ORDNANCE BUS", "SAFE"),
    ("RADAR", "STANDBY"),
    ("CONNECTION", "NO CARRIER"),
    ("REQUISITION CACHE", "STALE"),
];

#[expect(
    clippy::too_many_lines,
    reason = "one contiguous declaration of the display: the off-screen \
              camera, the tube, the header, both rails and the footer. \
              Splitting it into single-use builders would hide the layout \
              rather than clarify it."
)]
fn build_menu(
    mut commands: Commands,
    assets: Res<AssetServer>,
    menu: Res<Menu>,
    data: Res<LobbyData>,
    windows: Query<&Window>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<Crt>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut standard: ResMut<Assets<StandardMaterial>>,
) {
    let fonts = Fonts::load(&assets);
    let mut controls: Vec<ControlDef> = Vec::new();
    let mut ui = Ui {
        f: &fonts,
        controls: &mut controls,
    };
    let f = &fonts;

    // -- the off-screen target ----------------------------------------------
    let (phys, logical) =
        windows
            .iter()
            .next()
            .map_or((UVec2::new(1280, 720), Vec2::new(1280.0, 720.0)), |w| {
                (
                    UVec2::new(w.physical_width().max(1), w.physical_height().max(1)),
                    Vec2::new(w.width().max(1.0), w.height().max(1.0)),
                )
            });
    let target = images.add(render_target(phys));
    let material = materials.add(Crt::new(target.clone(), logical, crt_on()));

    // The one camera that draws the menu, targeting an image rather than the
    // window.
    //
    // A `Camera3d`, not a `Camera2d`, and restricted to [`PREVIEW_LAYER`]. Both
    // choices are load-bearing. 3D because the livery page's airframe has to be
    // *screen content* — geometry this camera renders before `bevy_ui` draws
    // the page over it, so the curvature and the scanlines are applied to the
    // ship and the text together. A preview composited on top of the finished
    // tube would be a sticker on the glass. The render layer because that is
    // what stops this camera seeing the match, and stops `camera.rs`'s camera
    // (layer 0, untouched) seeing the preview.
    //
    // `DefaultUiCamera` only ever considers cameras whose `RenderTarget` is the
    // primary window, so this one cannot become the default and `hud.rs` —
    // which relies on that resolution and is not this module's file — is
    // unaffected whatever its order is. The negative order is belt and braces.
    let camera = commands
        .spawn((
            Camera3d::default(),
            Camera {
                order: -1,
                clear_color: ClearColorConfig::Custom(pal::SCREEN),
                is_active: menu.open,
                ..default()
            },
            // Framed so the airframe sits in the right-hand third, which is the
            // column the livery page deliberately leaves empty.
            Transform::from_xyz(-2.65, 0.55, 6.6).looking_at(Vec3::new(-2.65, -0.1, 0.0), Vec3::Y),
            RenderLayers::layer(PREVIEW_LAYER),
            RenderTarget::Image(target.clone().into()),
        ))
        .id();

    build_preview(&mut commands, &assets, &mut meshes, &mut standard);

    // -- the tube: one node on the real camera ------------------------------
    let tube = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(0),
                top: px(0),
                width: percent(100),
                height: percent(100),
                display: display_of(menu.open),
                ..default()
            },
            MaterialNode(material.clone()),
            // Above `hud.rs`'s tree, whose root has no `GlobalZIndex` and so
            // sits at 0. That is how the in-flight HUD is covered without this
            // module reaching into another's private handles.
            GlobalZIndex(100),
        ))
        .id();

    let mut chrome = Entity::PLACEHOLDER;
    let mut backdrop = Entity::PLACEHOLDER;
    let mut pages = [Entity::PLACEHOLDER; Screen::ALL.len()];
    let mut title = Entity::PLACEHOLDER;
    let mut credits = Entity::PLACEHOLDER;
    let mut sweep = Entity::PLACEHOLDER;
    let mut lamps = [Entity::PLACEHOLDER; LAMPS.len()];
    let mut lamp_values = [Entity::PLACEHOLDER; LAMPS.len()];
    let mut notice = Entity::PLACEHOLDER;
    let mut clock = Entity::PLACEHOLDER;
    let mut boot_status = [Entity::PLACEHOLDER; BOOT_LINES];
    let mut boot_fill = Entity::PLACEHOLDER;
    let mut armed = Entity::PLACEHOLDER;
    let mut volume = [Entity::PLACEHOLDER; 2];
    let mut brief = [Entity::PLACEHOLDER; 3];
    let mut detail = [Entity::PLACEHOLDER; 4];
    let mut net_nodes = NetNodes::EMPTY;
    let mut dossier = DossierNodes::EMPTY;

    // -- the menu itself, rendered into the target --------------------------
    let root = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(0),
                top: px(0),
                width: percent(100),
                height: percent(100),
                flex_direction: FlexDirection::Column,
                display: display_of(menu.open),
                ..default()
            },
            // **Transparent**, not `pal::SCREEN`. The off-screen camera clears
            // to that colour itself, and the livery page's airframe is drawn by
            // that camera *before* `bevy_ui` paints this tree over it — so a
            // root with a background would hide the ship completely. It did,
            // for one build.
            BackgroundColor(Color::NONE),
            // Everything below this node draws into the off-screen image, not
            // into the window.
            UiTargetCamera(camera),
        ))
        .id();

    commands.entity(root).with_children(|screen_root| {
        // ---- the backdrop ---------------------------------------------------
        //
        // In two parts, and both opaque, because the off-screen camera draws
        // the livery preview *before* `bevy_ui` paints this tree — so anything
        // with a background hides it. The right-hand third stands down on the
        // livery page and nowhere else, which is what confines the airframe
        // (and whatever `skybox.rs` has attached to this camera) to the column
        // that page deliberately leaves empty.
        // Absolutely positioned, not a flex row. A row whose left child grows
        // re-expands over the whole tube the moment the right one leaves
        // layout, which hides the airframe the removal was meant to reveal.
        let split = 100.0 - PREVIEW_COLUMN;
        // The panel side: always opaque.
        screen_root.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(0),
                top: px(0),
                width: percent(split),
                height: percent(100),
                ..default()
            },
            BackgroundColor(pal::SCREEN),
        ));
        // The preview side, lifted on the livery page.
        backdrop = screen_root
            .spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: percent(split),
                    top: px(0),
                    right: px(0),
                    height: percent(100),
                    ..default()
                },
                BackgroundColor(pal::SCREEN),
            ))
            .id();
        // ...and a short gradient over the join, so the boundary between the
        // page and the preview is a falloff rather than a seam. Spawned after
        // the panel so it draws on top of it.
        screen_root.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: percent(split),
                top: px(0),
                width: px(110),
                height: percent(100),
                ..default()
            },
            BackgroundGradient::from(LinearGradient::to_right(vec![
                ColorStop::percent(pal::SCREEN, 0.0),
                ColorStop::percent(pal::rgba(0x05_08_0b, 0.0), 100.0),
            ])),
        ));

        // ---- the self test: full bleed, no rails ---------------------------
        pages[Screen::Boot.index()] = screen_root
            .spawn(Node {
                position_type: PositionType::Absolute,
                left: px(0),
                top: px(0),
                width: percent(100),
                height: percent(100),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                display: display_of(menu.screen == Screen::Boot),
                ..default()
            })
            .with_children(|b| {
                b.spawn(Node {
                    width: px(430),
                    flex_direction: FlexDirection::Column,
                    ..default()
                })
                .with_children(|stack| {
                    stack.spawn(wordmark(f, "SPACESHIPS", 34.0, pal::PHOSPHOR, 13.0));
                    stack.spawn(gap(34.0));
                    for (i, (name, _)) in BOOT_TEST.iter().enumerate() {
                        stack
                            .spawn(Node {
                                width: percent(100),
                                flex_direction: FlexDirection::Row,
                                justify_content: JustifyContent::SpaceBetween,
                                min_height: px(23),
                                align_items: AlignItems::Center,
                                ..default()
                            })
                            .with_children(|line| {
                                line.spawn(readout(f, name, 11.0, dim(0.65)));
                                boot_status[i] = line.spawn(readout(f, "", 11.0, pal::DEAD)).id();
                            });
                    }
                    stack.spawn(gap(30.0));
                    // A well and a fill, as every bar in `dash.js` and `hud.rs`
                    // is built.
                    stack
                        .spawn((
                            Node {
                                width: percent(100),
                                height: px(3),
                                ..default()
                            },
                            BackgroundColor(pal::WELL),
                        ))
                        .with_children(|w| {
                            boot_fill = w
                                .spawn((
                                    Node {
                                        width: percent(0),
                                        height: percent(100),
                                        ..default()
                                    },
                                    BackgroundColor(pal::PHOSPHOR),
                                ))
                                .id();
                        });
                });
            })
            .id();

        // ---- the framed display --------------------------------------------
        chrome = screen_root
            .spawn(Node {
                width: percent(100),
                height: percent(100),
                flex_direction: FlexDirection::Column,
                padding: UiRect::axes(px(EDGE_X), px(EDGE_Y)),
                display: display_of(menu.screen != Screen::Boot),
                ..default()
            })
            .with_children(|frame| {
                // -- header: two words and a number -------------------------
                frame
                    .spawn(Node {
                        width: percent(100),
                        align_items: AlignItems::Baseline,
                        column_gap: px(18),
                        margin: UiRect::bottom(px(10)),
                        ..default()
                    })
                    .with_children(|h| {
                        h.spawn(wordmark(f, "SPACESHIPS", 17.0, pal::PHOSPHOR, 5.0));
                        title = h
                            .spawn(caption(f, Screen::Main.title(), 9.0, dim(0.7)))
                            .id();
                        h.spawn(grow());
                        credits = h.spawn(readout(f, "0", 17.0, pal::AMBER)).id();
                    });
                frame.spawn(rule());

                // -- body ---------------------------------------------------
                frame
                    .spawn(Node {
                        width: percent(100),
                        flex_grow: 1.0,
                        flex_direction: FlexDirection::Row,
                        column_gap: px(GUTTER),
                        min_height: px(0),
                        padding: UiRect::vertical(px(26)),
                        ..default()
                    })
                    .with_children(|body| {
                        // rail: six words, no box, no caption
                        body.spawn(Node {
                            width: px(RAIL),
                            min_width: px(RAIL),
                            flex_direction: FlexDirection::Column,
                            row_gap: px(2),
                            ..default()
                        })
                        .with_children(|r| {
                            for (screen, label) in [
                                (Screen::Main, "MAIN"),
                                (Screen::Solo, "SOLO"),
                                (Screen::Net, "MULTIPLAYER"),
                                (Screen::Armory, "CUSTOMISE"),
                                (Screen::Record, "STATS"),
                                (Screen::Config, "SETTINGS"),
                            ] {
                                // Rail keys belong to no page: they are live on
                                // all of them, and `Boot` is the sentinel for
                                // "always" because `Boot` itself hides the rail.
                                control_row(
                                    r,
                                    &mut ui,
                                    Screen::Boot,
                                    Action::Go(screen),
                                    label,
                                    None,
                                    12.0,
                                );
                            }
                            r.spawn(grow());
                            section(r, f, "MAP");
                            control_row(
                                r,
                                &mut ui,
                                Screen::Boot,
                                Action::SetMap(MapKind::Space),
                                "SPACE",
                                None,
                                12.0,
                            );
                            control_row(
                                r,
                                &mut ui,
                                Screen::Boot,
                                Action::SetMap(MapKind::Terrain),
                                "SIERRAS",
                                None,
                                12.0,
                            );
                        });

                        // the page stack
                        body.spawn(Node {
                            flex_grow: 1.0,
                            min_width: px(0),
                            flex_direction: FlexDirection::Column,
                            ..default()
                        })
                        .with_children(|stack| {
                            build_pages(
                                stack,
                                &mut ui,
                                f,
                                &mut pages,
                                &mut armed,
                                &mut volume,
                                &mut brief,
                                &mut detail,
                                &mut net_nodes,
                                &mut dossier,
                                &menu,
                                &data,
                            );
                        });

                        // scope column
                        body.spawn(Node {
                            width: px(SCOPE_W),
                            min_width: px(SCOPE_W),
                            flex_direction: FlexDirection::Column,
                            row_gap: px(22),
                            ..default()
                        })
                        .with_children(|r| {
                            sweep = build_scope(r);
                            r.spawn(col(7.0)).with_children(|strip| {
                                for (i, name) in LAMPS.iter().enumerate() {
                                    strip.spawn(row(9.0)).with_children(|line| {
                                        lamps[i] = line
                                            .spawn((
                                                Node {
                                                    width: px(6),
                                                    height: px(6),
                                                    min_width: px(6),
                                                    ..default()
                                                },
                                                BackgroundColor(pal::DEAD),
                                            ))
                                            .id();
                                        line.spawn((
                                            caption(f, name, 8.0, dim(0.55)),
                                            Node {
                                                flex_grow: 1.0,
                                                ..default()
                                            },
                                        ));
                                        lamp_values[i] =
                                            line.spawn(readout(f, "", 9.0, dim(0.35))).id();
                                    });
                                }
                            });
                            r.spawn(grow());
                        });
                    });

                // -- footer -------------------------------------------------
                frame.spawn(rule());
                frame
                    .spawn(Node {
                        width: percent(100),
                        align_items: AlignItems::Center,
                        column_gap: px(22),
                        margin: UiRect::top(px(10)),
                        ..default()
                    })
                    .with_children(|ft| {
                        ft.spawn(caption(f, "ESC  BACK      ENTER  SELECT", 8.0, dim(0.4)));
                        ft.spawn(grow());
                        notice = ft.spawn(caption(f, "READY", 9.0, pal::AMBER)).id();
                        ft.spawn(grow());
                        clock = ft
                            .spawn(readout(f, "00:00", 9.0, pal::rgba(0x46_ff_9b, 0.55)))
                            .id();
                    });
            })
            .id();
    });

    let applied = vec![0xff; controls.len()];
    commands.insert_resource(MenuNodes {
        root,
        tube,
        camera,
        target,
        material,
        chrome,
        pages,
        title,
        credits,
        sweep,
        lamps,
        lamp_values,
        notice,
        clock,
        boot_status,
        boot_fill,
        backdrop,
        armed,
        volume,
        brief,
        detail,
        net: net_nodes,
        dossier,
        controls,
        applied,
    });
}

/// The image the menu renders into.
///
/// `Rgba8UnormSrgb` rather than the `Bgra8UnormSrgb` a swapchain usually
/// prefers: bgra is not a renderable format in WebGL2, and this crate ships to
/// the browser.
fn render_target(size: UVec2) -> Image {
    let extent = Extent3d {
        width: size.x,
        height: size.y,
        depth_or_array_layers: 1,
    };
    let mut image = Image::new_fill(
        extent,
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_descriptor.usage =
        TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST | TextureUsages::RENDER_ATTACHMENT;
    image
}

/// The scope: two rings, a fixed pool of blips, one arm.
///
/// A transcription of `cockpit.rs`'s radar into `bevy_ui`. The blips are a
/// fixed pool at fixed positions — there is no contact list in a lobby, and
/// inventing one that moved would put a per-frame write into a menu for
/// decoration. Returns the arm, the one node written per frame.
fn build_scope(parent: &mut ChildSpawnerCommands) -> Entity {
    let mut arm = Entity::PLACEHOLDER;
    parent
        .spawn((
            Node {
                width: percent(100),
                aspect_ratio: Some(1.0),
                border: UiRect::all(px(HAIR)),
                border_radius: BorderRadius::MAX,
                overflow: Overflow::clip(),
                ..default()
            },
            BorderColor::all(pal::rgba(pal::GRID, 0.9)),
        ))
        .with_children(|scope| {
            scope.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: percent(26),
                    top: percent(26),
                    right: percent(26),
                    bottom: percent(26),
                    border: UiRect::all(px(HAIR)),
                    border_radius: BorderRadius::MAX,
                    ..default()
                },
                BorderColor::all(pal::rgba(pal::GRID, 0.7)),
            ));
            for (x, y, hostile) in [
                (34.0_f32, 27.0_f32, false),
                (63.0, 38.0, true),
                (44.0, 66.0, false),
                (71.0, 61.0, true),
            ] {
                scope.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: percent(x),
                        top: percent(y),
                        width: px(3),
                        height: px(3),
                        ..default()
                    },
                    BackgroundColor(if hostile { pal::RED } else { pal::PHOSPHOR }),
                ));
            }
            // The arm: a full-size transparent node, so the rotation is about
            // the scope's centre rather than the line's own.
            arm = scope
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: px(0),
                        top: px(0),
                        width: percent(100),
                        height: percent(100),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::FlexStart,
                        ..default()
                    },
                    UiTransform::IDENTITY,
                ))
                .with_children(|a| {
                    a.spawn((
                        Node {
                            width: px(HAIR),
                            height: percent(50),
                            ..default()
                        },
                        BackgroundColor(pal::PHOSPHOR_LAMP),
                    ));
                })
                .id();
        });
    arm
}

// ---------------------------------------------------------------------------
// The pages
// ---------------------------------------------------------------------------

/// Spawns the twelve framed pages into the page stack.
///
/// Every page is a column with a lot of air in it and at most two groups. The
/// discipline: one thing *is* the page, everything else is smaller, dimmer or
/// both, and anything that reads without a caption does not get one.
#[expect(
    clippy::too_many_lines,
    reason = "twelve page bodies, each a handful of lines. Reading them side \
              by side is the point; the JS spreads the same twelve across \
              index.html and five modules in lobby/."
)]
#[expect(
    clippy::too_many_arguments,
    reason = "one out-parameter per group of handles the pages hand back. The \
              network four are already collapsed into `NetNodes`; collapsing \
              the rest would be a struct per page for no reader's benefit."
)]
fn build_pages(
    stack: &mut ChildSpawnerCommands,
    ui: &mut Ui,
    f: &Fonts,
    pages: &mut [Entity; Screen::ALL.len()],
    armed: &mut Entity,
    volume: &mut [Entity; 2],
    brief: &mut [Entity; 3],
    detail: &mut [Entity; 4],
    net: &mut NetNodes,
    dossier: &mut DossierNodes,
    menu: &Menu,
    data: &LobbyData,
) {
    // The pilot's own numbers are written by `drive_menu` from `LobbyData` the
    // moment a reply lands, so the strings baked in here are only what is on
    // screen for the first frame. They are still built from the placeholder
    // rather than from nothing, because an empty page that fills in a second
    // later reads as broken and a page that changes reads as live.
    let pilot = &data.pilot;

    // Every page is the same absolute fill; only one has `Display::Flex`.
    // Vertically centred rather than top-anchored. A page's content is a
    // fraction of the height available to it, and hanging it from the top
    // leaves all the air in one lump at the bottom, which reads as a mistake
    // rather than as space. Centred, the air is split above and below and the
    // composition sits against the scope in the right rail.
    let page = |visible: bool| Node {
        position_type: PositionType::Absolute,
        left: px(0),
        top: px(0),
        right: px(0),
        bottom: px(0),
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::Center,
        column_gap: px(GUTTER),
        display: display_of(visible),
        ..default()
    };
    let on = |s: Screen| menu.screen == s;

    // ---- MISSION BOARD ----------------------------------------------------
    pages[Screen::Main.index()] = stack
        .spawn(page(on(Screen::Main)))
        .with_children(|p| {
            p.spawn(page_col(52.0)).with_children(|c| {
                dossier.board_name = c
                    .spawn(wordmark(f, &pilot.callsign, 32.0, pal::PHOSPHOR, 9.0))
                    .id();
                dossier.board_line = c.spawn(readout(f, "", 10.0, dim(0.6))).id();
                c.spawn(gap(44.0));
                for (screen, label) in [
                    (Screen::Solo, "SINGLE PLAYER"),
                    (Screen::Net, "MULTIPLAYER"),
                    (Screen::Armory, "CUSTOMISE"),
                    (Screen::Record, "YOUR STATS"),
                    (Screen::Auth, "LOG IN"),
                    (Screen::Config, "SETTINGS"),
                ] {
                    control_row(c, ui, Screen::Main, Action::Go(screen), label, None, 18.0);
                }
                c.spawn(gap(44.0));
                section(c, f, "SELECTED");
                *armed = c.spawn(readout(f, "", 13.0, pal::WHITE)).id();
                c.spawn(gap(12.0));
                control_row(c, ui, Screen::Main, Action::Execute, "START", None, 15.0);
            });
        })
        .id();

    // ---- SOLO -------------------------------------------------------------
    pages[Screen::Solo.index()] = stack
        .spawn(page(on(Screen::Solo)))
        .with_children(|p| {
            p.spawn(page_col(52.0)).with_children(|c| {
                for (pick, label) in [
                    (SoloPick::Tutorial, "TUTORIAL"),
                    (SoloPick::Train, "TRAIN VS BOT"),
                    (SoloPick::Skirmish, "SKIRMISH"),
                ] {
                    control_row(
                        c,
                        ui,
                        Screen::Solo,
                        Action::SetSolo(pick),
                        label,
                        None,
                        18.0,
                    );
                }
                // Not a control: a caption, so it costs no cursor stop. It is
                // here because the three rows above it no longer arm a
                // preference for a separate `EXECUTE` to consume — they launch
                // — and a row that flies the aircraft should say so before it
                // is pressed rather than after.
                c.spawn(gap(10.0));
                c.spawn(hint(f));
                c.spawn(gap(32.0));
                for (screen, label) in [
                    (Screen::Trials, "TIME TRIALS"),
                    (Screen::Campaign, "CAMPAIGN"),
                ] {
                    control_row(c, ui, Screen::Solo, Action::Go(screen), label, None, 18.0);
                }
            });
            p.spawn(page_col(38.0)).with_children(|c| {
                for (i, k) in ["OBJECTIVE", "ENEMY", "DURATION"].into_iter().enumerate() {
                    c.spawn(col(3.0)).with_children(|b| {
                        b.spawn(caption(f, k, 8.0, dim(0.4)));
                        brief[i] = b.spawn(readout(f, "", 13.0, pal::WHITE)).id();
                    });
                    c.spawn(gap(20.0));
                }
            });
        })
        .id();

    // ---- TRIALS -----------------------------------------------------------
    pages[Screen::Trials.index()] = stack
        .spawn(page(on(Screen::Trials)))
        .with_children(|p| {
            p.spawn(page_col(70.0)).with_children(|c| {
                for i in 0..4usize {
                    #[allow(clippy::cast_possible_truncation)]
                    control_row(
                        c,
                        ui,
                        Screen::Trials,
                        Action::SetTrial(i as u8),
                        ["ARRIVAL", "SHELF", "NEEDLE", "LONG WAY"][i],
                        Some(&format!(
                            "{:>2} CP     {}",
                            [12, 14, 16, 18][i],
                            fmt_trial(pilot.trial_best[i])
                        )),
                        18.0,
                    );
                    // The best time is the pilot's, so it arrives with the
                    // dossier rather than being known at build time.
                    dossier.trial_rows[i] = ui.controls.len() - 1;
                }
                c.spawn(gap(10.0));
                c.spawn(hint(f));
            });
        })
        .id();

    // ---- CAMPAIGN ---------------------------------------------------------
    pages[Screen::Campaign.index()] = stack
        .spawn(page(on(Screen::Campaign)))
        .with_children(|p| {
            p.spawn(page_col(70.0)).with_children(|c| {
                for (i, name) in ["IRONCLAD", "STORMFRONT", "FINAL SIEGE"]
                    .into_iter()
                    .enumerate()
                {
                    let state = match pilot.campaign_lives[i] {
                        Some(l) => format!("{l} OF 3 LIVES"),
                        None => "NOT FLOWN".to_owned(),
                    };
                    #[allow(clippy::cast_possible_truncation)]
                    control_row(
                        c,
                        ui,
                        Screen::Campaign,
                        Action::SetMission(i as u8),
                        name,
                        Some(&state),
                        18.0,
                    );
                    dossier.mission_rows[i] = ui.controls.len() - 1;
                }
                c.spawn(gap(10.0));
                c.spawn(hint(f));
            });
        })
        .id();

    // ---- NETWORK ----------------------------------------------------------
    pages[Screen::Net.index()] = stack
        .spawn(page(on(Screen::Net)))
        .with_children(|p| {
            p.spawn(page_col(52.0)).with_children(|c| {
                control_row(
                    c,
                    ui,
                    Screen::Net,
                    Action::Go(Screen::Create),
                    "CREATE GAME",
                    None,
                    18.0,
                );
                control_row(
                    c,
                    ui,
                    Screen::Net,
                    Action::Go(Screen::Browser),
                    "FIND GAME",
                    None,
                    18.0,
                );
                c.spawn(gap(30.0));
                // The no-environment-variable escape hatch. A packaged build
                // dials the live game; this is how the machine that is *running*
                // a server reaches it, and how a LAN game is arranged by
                // someone who will never see a shell.
                control_row(
                    c,
                    ui,
                    Screen::Net,
                    Action::CycleEndpoint,
                    "SERVER",
                    Some("- - - -"),
                    15.0,
                );
                net.endpoint_row = ui.controls.len() - 1;
            });
            p.spawn(page_col(38.0)).with_children(|c| {
                section(c, f, "CONNECTION");
                net.link_values[0] = kv(c, f, "SERVER", "- - - -", pal::rgba(0xea_f6_ff, 0.8));
                net.link_values[1] = kv(c, f, "IDENTITY", "GUEST", pal::AMBER);
                net.link_values[2] = kv(c, f, "FRAMES", "0 OUT   0 IN", pal::rgba(0xea_f6_ff, 0.8));
            });
        })
        .id();

    // ---- CREATE -----------------------------------------------------------
    pages[Screen::Create.index()] = stack
        .spawn(page(on(Screen::Create)))
        .with_children(|p| {
            p.spawn(page_col(52.0)).with_children(|c| {
                section(c, f, "ACCESS");
                control_row(
                    c,
                    ui,
                    Screen::Create,
                    Action::SetPrivate(false),
                    "OPEN",
                    None,
                    15.0,
                );
                control_row(
                    c,
                    ui,
                    Screen::Create,
                    Action::SetPrivate(true),
                    "PRIVATE",
                    None,
                    15.0,
                );
                c.spawn(gap(30.0));
                section(c, f, "PLAYERS");
                control_row(
                    c,
                    ui,
                    Screen::Create,
                    Action::SetAutoBot(true),
                    "ADD A BOT",
                    None,
                    15.0,
                );
                control_row(
                    c,
                    ui,
                    Screen::Create,
                    Action::SetAutoBot(false),
                    "EMPTY MAP",
                    None,
                    16.0,
                );
                c.spawn(gap(44.0));
                control_row(
                    c,
                    ui,
                    Screen::Create,
                    Action::Transmit,
                    "CONFIRM",
                    None,
                    15.0,
                );
            });
        })
        .id();

    // ---- BROWSER ----------------------------------------------------------
    //
    // A fixed pool of rows, written from `NetSession` rather than built from
    // it: the whole tree is built once (see the module docs) and a browser that
    // rebuilt itself on every `rooms-list` would be the per-frame relayout this
    // module exists to avoid, at the worst possible moment.
    pages[Screen::Browser.index()] = stack
        .spawn(page(on(Screen::Browser)))
        .with_children(|p| {
            p.spawn(page_col(88.0)).with_children(|c| {
                for i in 0..SORTIE_ROWS {
                    #[allow(clippy::cast_possible_truncation)]
                    control_row(
                        c,
                        ui,
                        Screen::Browser,
                        Action::SetSortie(i as u8),
                        "- - - -",
                        Some(" "),
                        16.0,
                    );
                    net.sortie_rows[i] = ui.controls.len() - 1;
                }
                c.spawn(gap(30.0));
                control_row(
                    c,
                    ui,
                    Screen::Browser,
                    Action::Refresh,
                    "REFRESH",
                    None,
                    15.0,
                );
                control_row(
                    c,
                    ui,
                    Screen::Browser,
                    Action::JoinSortie,
                    "JOIN",
                    None,
                    15.0,
                );
            });
        })
        .id();

    // ---- WAITING ----------------------------------------------------------
    pages[Screen::Waiting.index()] = stack
        .spawn(page(on(Screen::Waiting)))
        .with_children(|p| {
            p.spawn(page_col(60.0)).with_children(|c| {
                net.room_code = c
                    .spawn(tracked(f, "- - - -", 44.0, pal::PHOSPHOR, 16.0))
                    .id();
                net.room_access = c.spawn(readout(f, "OPEN", 9.0, dim(0.5))).id();
                c.spawn(gap(50.0));
                for seat in 0..SEAT_ROWS {
                    let mut tick = Entity::PLACEHOLDER;
                    let mut name = Entity::PLACEHOLDER;
                    let mut lead = Entity::PLACEHOLDER;
                    let root = c
                        .spawn(Node {
                            // `row(12.0)` plus a height and a starting state.
                            // Spelled out rather than spread over two bundle
                            // members: two `Node`s in one bundle is a
                            // duplicate-component panic at spawn time.
                            min_height: px(21),
                            display: Display::None,
                            ..row(12.0)
                        })
                        .with_children(|r| {
                            tick = r
                                .spawn((
                                    Node {
                                        width: px(2),
                                        min_width: px(2),
                                        height: px(15),
                                        ..default()
                                    },
                                    BackgroundColor(pal::CYAN),
                                ))
                                .id();
                            name = r
                                .spawn((
                                    caption(f, "", 14.0, pal::WHITE),
                                    Node {
                                        flex_grow: 1.0,
                                        min_width: px(0),
                                        ..default()
                                    },
                                ))
                                .id();
                            lead = r.spawn(readout(f, "", 9.0, pal::AMBER)).id();
                        })
                        .id();
                    net.seats[seat] = SeatNodes {
                        root,
                        tick,
                        name,
                        lead,
                    };
                }
                c.spawn(gap(40.0));
                control_row(
                    c,
                    ui,
                    Screen::Waiting,
                    Action::LaunchNet,
                    "STANDBY",
                    None,
                    15.0,
                );
                net.launch_row = ui.controls.len() - 1;
                control_row(
                    c,
                    ui,
                    Screen::Waiting,
                    Action::LeaveRoom,
                    "LEAVE",
                    None,
                    15.0,
                );
            });
        })
        .id();

    // ---- ARMORY -----------------------------------------------------------
    pages[Screen::Armory.index()] = stack
        .spawn(page(on(Screen::Armory)))
        .with_children(|p| {
            // The ladder itself.
            p.spawn(page_col(60.0)).with_children(|c| {
                let mut last_tier = u8::MAX;
                for (i, rung) in ARMORY.iter().enumerate() {
                    if rung.tier != last_tier {
                        if last_tier != u8::MAX {
                            c.spawn(gap(14.0));
                        }
                        last_tier = rung.tier;
                        dossier.tier_captions[rung.tier as usize] = c
                            .spawn(caption(
                                f,
                                TIERS[rung.tier as usize],
                                8.0,
                                if tier_open(rung.tier, pilot.owned) {
                                    dim(0.55)
                                } else {
                                    pal::DEAD
                                },
                            ))
                            .id();
                    }
                    let right = match stock(i, data) {
                        Stock::Held => "HELD".to_owned(),
                        Stock::Locked => "LOCKED".to_owned(),
                        _ => thousands(rung.cost),
                    };
                    #[allow(clippy::cast_possible_truncation)]
                    control_row(
                        c,
                        ui,
                        Screen::Armory,
                        Action::SetItem(i as u8),
                        rung.name,
                        Some(&right),
                        13.0,
                    );
                    // Held, priced or locked is the *pilot's* ladder, so it
                    // arrives with the dossier. Baking it in here and never
                    // rewriting it is what left a fresh account looking as
                    // though it owned the first three tiers.
                    dossier.rung_rows[i] = ui.controls.len() - 1;
                }
            });

            // What is highlighted.
            p.spawn(page_col(38.0)).with_children(|c| {
                detail[0] = c.spawn(tracked(f, "", 19.0, pal::WHITE, 2.0)).id();
                c.spawn(gap(26.0));
                detail[1] = c.spawn(readout(f, "", 32.0, pal::AMBER)).id();
                detail[2] = c.spawn(readout(f, "", 10.0, pal::PHOSPHOR)).id();
                c.spawn(gap(26.0));
                detail[3] = c.spawn(readout(f, "", 10.0, dim(0.75))).id();
                c.spawn(gap(40.0));
                control_row(
                    c,
                    ui,
                    Screen::Armory,
                    Action::Requisition,
                    "REQUISITION",
                    None,
                    15.0,
                );
                control_row(
                    c,
                    ui,
                    Screen::Armory,
                    Action::Go(Screen::Livery),
                    "COLOURS",
                    None,
                    15.0,
                );
            });
        })
        .id();

    // ---- LIVERY -----------------------------------------------------------
    //
    // Three columns of names, and the aircraft itself in the space to their
    // right. The airframe is not a UI node: it is geometry the off-screen
    // camera renders *before* the page, so it goes through the curvature, the
    // scanlines, the bleed and the vignette with everything else. Drawing it
    // over the finished tube would make it a sticker on the glass.
    pages[Screen::Livery.index()] = stack
        .spawn(page(on(Screen::Livery)))
        .with_children(|p| {
            p.spawn(page_col(20.0)).with_children(|c| {
                section(c, f, "HULL");
                for (i, (name, _)) in LIVERY.iter().enumerate() {
                    #[allow(clippy::cast_possible_truncation)]
                    control_row(
                        c,
                        ui,
                        Screen::Livery,
                        Action::SetHull(i as u8),
                        name,
                        None,
                        12.0,
                    );
                }
            });
            p.spawn(page_col(20.0)).with_children(|c| {
                section(c, f, "ACCENT");
                for (i, (name, _)) in LIVERY.iter().enumerate() {
                    #[allow(clippy::cast_possible_truncation)]
                    control_row(
                        c,
                        ui,
                        Screen::Livery,
                        Action::SetAccent(i as u8),
                        name,
                        None,
                        12.0,
                    );
                }
            });
            p.spawn(page_col(18.0)).with_children(|c| {
                section(c, f, "TRAIL");
                for (i, name) in TRAIL_SHAPES.iter().enumerate() {
                    #[allow(clippy::cast_possible_truncation)]
                    control_row(
                        c,
                        ui,
                        Screen::Livery,
                        Action::SetTrail(i as u8),
                        name,
                        None,
                        12.0,
                    );
                }
            });
            // The right-hand third is left empty on purpose: that is where the
            // airframe is, and it is behind this tree rather than in it.
            p.spawn(grow());
        })
        .id();

    // ---- RECORD -----------------------------------------------------------
    pages[Screen::Record.index()] = stack
        .spawn(page(on(Screen::Record)))
        .with_children(|p| {
            p.spawn(page_col(94.0)).with_children(|c| {
                dossier.record_name = c
                    .spawn(wordmark(f, &pilot.callsign, 32.0, pal::PHOSPHOR, 9.0))
                    .id();
                dossier.record_line = c.spawn(readout(f, "", 10.0, dim(0.6))).id();
                c.spawn(gap(46.0));

                // Six numbers, no cells, no borders. The grid gaps group them.
                c.spawn(Node {
                    display: Display::Grid,
                    grid_template_columns: RepeatedGridTrack::flex(3, 1.0),
                    row_gap: px(30),
                    column_gap: px(34),
                    ..default()
                })
                .with_children(|g| {
                    for (i, (k, colour)) in [
                        ("FLIGHT HOURS", pal::PHOSPHOR),
                        ("KILL MARKS", pal::AMBER),
                        ("EXCHANGE", pal::WHITE),
                        ("GAMES", pal::WHITE),
                        ("BOTS DOWNED", pal::WHITE),
                        ("CAPITAL KILLS", pal::WHITE),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        g.spawn(col(4.0)).with_children(|cell| {
                            cell.spawn(caption(f, k, 8.0, dim(0.4)));
                            dossier.stats[i] = cell.spawn(readout(f, "0", 24.0, colour)).id();
                        });
                    }
                });

                c.spawn(gap(46.0));
                // One mark per ten kills, in groups of five: a dossier's tally,
                // and the only decoration on the page.
                //
                // Only the marks earned are shown. The first pass also drew the
                // unearned ones at low alpha, which under the phosphor bleed
                // smeared into a single bar and told the pilot nothing.
                //
                // A **fixed pool**, hidden down to the count, rather than a list
                // rebuilt per refresh: the tree is built once, and a dossier
                // that despawned and respawned seventy nodes every time the
                // balance changed would be the churn this module is shaped to
                // avoid.
                c.spawn(Node {
                    flex_direction: FlexDirection::Row,
                    column_gap: px(14),
                    flex_wrap: FlexWrap::Wrap,
                    row_gap: px(8),
                    ..default()
                })
                .with_children(|marks| {
                    for grp in 0..KILL_MARKS.div_ceil(5) {
                        marks.spawn(row(4.0)).with_children(|g| {
                            for i in 0..5usize {
                                let n = grp * 5 + i;
                                if n >= KILL_MARKS {
                                    break;
                                }
                                dossier.marks[n] = g
                                    .spawn((
                                        Node {
                                            width: px(2),
                                            height: px(16),
                                            display: Display::None,
                                            ..default()
                                        },
                                        BackgroundColor(pal::AMBER),
                                    ))
                                    .id();
                            }
                        });
                    }
                });
                c.spawn(gap(36.0));
                control_row(
                    c,
                    ui,
                    Screen::Record,
                    Action::Go(Screen::Standings),
                    "LEADERBOARD",
                    None,
                    15.0,
                );
                control_row(
                    c,
                    ui,
                    Screen::Record,
                    Action::Go(Screen::Auth),
                    "LOG IN",
                    None,
                    15.0,
                );
                control_row(
                    c,
                    ui,
                    Screen::Record,
                    Action::RefreshData,
                    "REFRESH",
                    None,
                    15.0,
                );
            });
        })
        .id();

    // ---- STANDINGS --------------------------------------------------------
    //
    // A fixed pool of ten, for the reason the room browser is one: the tree is
    // built once. The rows past the end of a shorter list stand down.
    pages[Screen::Standings.index()] = stack
        .spawn(page(on(Screen::Standings)))
        .with_children(|p| {
            p.spawn(Node {
                width: percent(100),
                flex_direction: FlexDirection::Column,
                row_gap: px(8),
                ..default()
            })
            .with_children(|c| {
                for i in 0..STANDING_ROWS {
                    let mut nodes = StandingNodes::EMPTY;
                    // One `Node`, not a bundle of two: `row()` *is* a `Node`,
                    // and a bundle with the same component twice is a panic at
                    // spawn rather than a compile error.
                    nodes.root = c
                        .spawn(Node {
                            display: Display::None,
                            ..row(18.0)
                        })
                        .with_children(|r| {
                            nodes.position = r.spawn(readout(f, "", 11.0, dim(0.35))).id();
                            nodes.callsign = r
                                .spawn((
                                    caption(f, "", 14.0, pal::WHITE),
                                    Node {
                                        width: px(160),
                                        ..default()
                                    },
                                ))
                                .id();
                            nodes.rank = r
                                .spawn((
                                    readout(f, "", 9.0, dim(0.45)),
                                    Node {
                                        flex_grow: 1.0,
                                        ..default()
                                    },
                                ))
                                .id();
                            nodes.kills = r.spawn(readout(f, "", 11.0, pal::WHITE)).id();
                            nodes.wins = r.spawn(readout(f, "", 11.0, pal::WHITE)).id();
                        })
                        .id();
                    dossier.standings[i] = nodes;
                }
            });
        })
        .id();

    // ---- AUTH -------------------------------------------------------------
    //
    // Two fields and four rows. The JS puts this behind `#auth-overlay`, a modal
    // that covers the game until it is dismissed; here it is a page like any
    // other, because a guest never has to come here at all and a wall between a
    // player and the game they opened is the one thing that overlay gets wrong.
    pages[Screen::Auth.index()] = stack
        .spawn(page(on(Screen::Auth)))
        .with_children(|p| {
            p.spawn(page_col(56.0)).with_children(|c| {
                section(c, f, "ACCOUNT");
                // The cursor resting on one of these *is* the caret; there is no
                // second focus model. `control_row`'s value node is what the
                // typed text goes into, which is why both are registered as
                // controls rather than drawn as `kv` lines.
                for (field, label) in [(Field::Callsign, "CALLSIGN"), (Field::Secret, "PASSWORD")] {
                    control_row(
                        c,
                        ui,
                        Screen::Auth,
                        Action::Edit(field),
                        label,
                        Some("_"),
                        16.0,
                    );
                    dossier.fields[field as usize] = ui.controls[ui.controls.len() - 1].value;
                }
                c.spawn(gap(8.0));
                c.spawn(tracked(
                    f,
                    "TYPE WITH THE CURSOR ON A FIELD",
                    8.0,
                    dim(0.35),
                    2.4,
                ));
                c.spawn(gap(34.0));
                control_row(c, ui, Screen::Auth, Action::SignIn, "LOG IN", None, 17.0);
                control_row(c, ui, Screen::Auth, Action::Enlist, "SIGN UP", None, 15.0);
                control_row(c, ui, Screen::Auth, Action::SignOut, "SIGN OUT", None, 15.0);
            });
            p.spawn(page_col(34.0)).with_children(|c| {
                section(c, f, "PILOT");
                dossier.identity = c.spawn(readout(f, "GUEST", 15.0, pal::AMBER)).id();
                c.spawn(gap(20.0));
                // Said plainly, because it is the honest answer to "do I have
                // to do this" and the JS buries it in a link under the form.
                c.spawn(readout(
                    f,
                    "A guest flies everything.\nAn account is what makes\nit count.",
                    9.0,
                    dim(0.45),
                ));
            });
        })
        .id();

    // ---- CONFIG -----------------------------------------------------------
    pages[Screen::Config.index()] = stack
        .spawn(page(on(Screen::Config)))
        .with_children(|p| {
            p.spawn(page_col(48.0)).with_children(|c| {
                section(c, f, "AUDIO");
                for (ch, label) in [(0u8, "MUSIC"), (1, "EFFECTS")] {
                    control_row(
                        c,
                        ui,
                        Screen::Config,
                        Action::Volume(ch),
                        label,
                        Some(""),
                        13.0,
                    );
                    // The row was just registered, so its value node is the
                    // last one in the index. Cheaper than threading two more
                    // out-parameters through `build_pages` for two strings.
                    volume[usize::from(ch)] = ui.controls[ui.controls.len() - 1].value;
                }
                c.spawn(gap(30.0));
                section(c, f, "CONTROL");
                for (i, s) in SCHEMES.iter().enumerate() {
                    #[allow(clippy::cast_possible_truncation)]
                    control_row(
                        c,
                        ui,
                        Screen::Config,
                        Action::SetScheme(i as u8),
                        s,
                        None,
                        14.0,
                    );
                }
            });
            p.spawn(page_col(46.0)).with_children(|c| {
                section(c, f, "SETTINGS");
                // No ON/OFF column. The label and the cursor tick go amber
                // when a flag is set, which says it without a second word — and
                // a static "OFF" beside a lit label, which is what the first
                // pass shipped, says the opposite of the truth.
                for flag in Flag::ALL {
                    control_row(
                        c,
                        ui,
                        Screen::Config,
                        Action::Toggle(flag),
                        flag.label(),
                        None,
                        14.0,
                    );
                }
            });
        })
        .id();
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// The controls the current page owns, in cursor order.
///
/// The page's own controls first and the rail after, so arriving on a page and
/// pressing `ENTER` does what the page is for rather than re-opening the page
/// you are already on.
fn page_controls(nodes: &MenuNodes, screen: Screen) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::new();
    for rail_pass in [false, true] {
        for (i, d) in nodes.controls.iter().enumerate() {
            let is_rail = d.screen == Screen::Boot;
            if is_rail == rail_pass && (is_rail || d.screen == screen) {
                #[allow(clippy::cast_possible_truncation)]
                out.push(i as u16);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The pointer
// ---------------------------------------------------------------------------

/// [`CRT_WGSL`]'s `warp`, on the CPU.
///
/// Deliberately the same three tokens as the shader's body, because the two
/// have to agree to the pixel and the cheapest way to keep them agreeing is for
/// a reviewer to be able to read them side by side.
fn warp(p: Vec2, k: f32) -> Vec2 {
    p * (1.0 + k * p.dot(p))
}

/// Where a point on the tube lands on the menu image, or `None` if it landed
/// off the glass.
///
/// This is the fragment shader's projection, forward, on one point:
///
/// ```wgsl
/// let c = (in.uv * 2.0 - 1.0) * inset / (1.0 + k);
/// let w = warp(c, k);
/// let uv = clamp(w * 0.5 + 0.5, 0.0, 1.0);
/// ```
///
/// The shader runs it per fragment to ask "what part of the page is under this
/// pixel of glass", which is the same question a cursor asks — so nothing has
/// to be inverted. `cursor` and `tube` are in the window's physical pixels
/// (what `bevy_ui` measures the tube node in); `image` is the off-screen
/// target's size, and the result is in its pixels, which is the space the menu
/// tree is laid out in.
///
/// The rejection is the shader's `edge`: `max(abs(w))` is the tube's
/// rectangular boundary in warped space, 1.0 is the edge of the glass, and
/// anything larger is on the moulding or the room beyond it. The shader clamps
/// there because it still has a pixel to shade; a pointer has nothing to point
/// at, so it says so.
fn through_the_glass(cursor: Vec2, tube: Rect, image: Vec2, k: f32, inset: f32) -> Option<Vec2> {
    let size = tube.size();
    if size.x <= 0.0 || size.y <= 0.0 || image.x <= 0.0 || image.y <= 0.0 {
        return None;
    }
    let uv = (cursor - tube.min) / size;
    let c = (uv * 2.0 - Vec2::ONE) * inset / (1.0 + k);
    let w = warp(c, k);
    if w.x.abs().max(w.y.abs()) > 1.0 {
        return None;
    }
    Some((w * 0.5 + Vec2::splat(0.5)) * image)
}

/// The first of `rows` containing `point`, as `(slot, control)`.
///
/// A linear scan, and it stays one: `rows` is the page's own cursor order,
/// which is under twenty entries on the widest page, and it only runs on a
/// frame the pointer actually moved. The rows do not overlap — they are
/// siblings in a column — so "first" and "topmost" are the same answer.
fn control_at(point: Vec2, rows: &[(u16, Rect)]) -> Option<(u8, u16)> {
    #[allow(clippy::cast_possible_truncation)]
    rows.iter()
        .position(|(_, r)| r.contains(point))
        .map(|slot| (slot as u8, rows[slot].0))
}

/// Everything resolving the pointer needs, as one system parameter.
///
/// Grouped rather than spread over [`read_input`]'s signature because the four
/// of them are one concern, and because the alternative is a nine-line
/// parameter list in which the keyboard and the mouse are indistinguishable.
#[derive(SystemParam)]
struct Pointer<'w, 's> {
    windows: Query<'w, 's, &'static Window>,
    buttons: Res<'w, ButtonInput<MouseButton>>,
    /// The tube's material, for the curve the cursor has to be mapped through.
    /// See [`Crt::glass`].
    materials: Res<'w, Assets<Crt>>,
    /// Every laid-out node's rect. One query serves the tube and the controls
    /// because both are `bevy_ui` nodes; they are simply measured against
    /// different targets, which is the whole problem this module is solving.
    ///
    /// Read one frame late — layout runs in `PostUpdate` — exactly as
    /// `bevy_ui`'s own `ui_focus_system` reads it.
    rects: Query<'w, 's, (&'static ComputedNode, &'static UiGlobalTransform)>,
    /// Where the cursor was the last time it was resolved.
    was: Local<'s, Option<Vec2>>,
    /// And which page it was resolved against.
    page: Local<'s, u8>,
}

/// What the pointer is on: a slot in the page's cursor order, the control that
/// slot names, and whether the left button went down on it this frame.
struct Aim {
    slot: u8,
    control: u16,
    clicked: bool,
}

impl Pointer<'_, '_> {
    /// A laid-out node's rect, in its own tree's pixels.
    ///
    /// `None` for a node with no extent, which is what a page that is
    /// `Display::None` and everything under it measures as — so an off-page
    /// control cannot be clicked even if one reached this far.
    fn rect(&self, entity: Entity) -> Option<Rect> {
        let (node, transform) = self.rects.get(entity).ok()?;
        (node.size.x > 0.0 && node.size.y > 0.0)
            .then(|| Rect::from_center_size(transform.translation, node.size))
    }

    /// Resolves the cursor onto one of `list`'s controls.
    ///
    /// Returns `None` when nothing can have changed — no window, no cursor, the
    /// pointer off the glass, or, first of all, a pointer that has not moved
    /// and a button that has not gone down. That last case is the one the
    /// module's no-per-frame-writes rule cares about: it costs an `Option<Vec2>`
    /// comparison and touches nothing.
    ///
    /// A page change counts as movement even though the mouse did not move,
    /// because every row moved out from under it — click a door and the row now
    /// under the pointer should light immediately rather than after a jiggle.
    /// That is an event, not a frame, and it costs one byte compared.
    fn resolve(&mut self, nodes: &MenuNodes, screen: Screen, list: &[u16]) -> Option<Aim> {
        let clicked = self.buttons.just_pressed(MouseButton::Left);
        let turned = *self.page != screen as u8;
        *self.page = screen as u8;
        let at = self
            .windows
            .single()
            .ok()
            .and_then(Window::physical_cursor_position);
        if at == *self.was && !clicked && !turned {
            return None;
        }
        *self.was = at;

        let cursor = at?;
        let (k, inset) = self.materials.get(&nodes.material)?.glass();
        let tube = self.rect(nodes.tube)?;
        let image = self.rect(nodes.root)?.size();
        let point = through_the_glass(cursor, tube, image, k, inset)?;

        let rows: Vec<(u16, Rect)> = list
            .iter()
            .filter_map(|&i| Some((i, self.rect(nodes.controls[i as usize].root)?)))
            .collect();
        let (slot, control) = control_at(point, &rows)?;
        Some(Aim {
            slot,
            control,
            clicked,
        })
    }
}

/// Keyboard and pointer. Moves the cursor, fires actions, nothing else.
#[expect(
    clippy::too_many_arguments,
    reason = "one resource per thing an action can touch. `apply` takes them as \
              two borrows and a collector precisely so that the list stops \
              growing here."
)]
fn read_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut typed: MessageReader<KeyboardInput>,
    time: Res<Time>,
    nodes: Option<Res<MenuNodes>>,
    session: Res<NetSession>,
    mut config: ResMut<NetConfig>,
    mut menu: ResMut<Menu>,
    mut data: ResMut<LobbyData>,
    mut setup: ResMut<MatchSetup>,
    mut launch: MessageWriter<LaunchRequest>,
    mut outbox: MessageWriter<ToServer>,
    mut commands: MessageWriter<NetCommand>,
    mut api: MessageWriter<ApiRequest>,
    mut pointer: Pointer,
    mut audio: ResMut<crate::audio::AudioCommands>,
) {
    let Some(nodes) = nodes else { return };

    // `ESC` while flying brings the display back up. This is the only path in;
    // the JS client has no pause concept either.
    if !menu.open {
        if keys.just_pressed(KeyCode::Escape) {
            menu.open = true;
            menu.screen = Screen::Main;
            menu.opened_at = time.elapsed_secs();
            // Paused rather than abandoned: `fixed_tick` freezes the world
            // while this is up, and the second `ESC` below puts the pilot back
            // where they left off.
            menu.resumable = true;
            menu.say("PAUSED");
        }
        return;
    }

    // The self test takes no input beyond skipping it.
    if menu.screen == Screen::Boot {
        if !menu.pinned && (keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Escape))
        {
            menu.opened_at = time.elapsed_secs() - (BOOT_LINES as f32 * BOOT_STEP + BOOT_HOLD);
        }
        return;
    }

    let page = menu.screen.index();
    let list = page_controls(&nodes, menu.screen);
    if list.is_empty() {
        return;
    }

    // -- pointer ------------------------------------------------------------
    //
    // Hover moves the cursor rather than lighting a second highlight. One
    // highlighted line at a time is how a page-key display behaves, and it
    // halves the state the model has to carry.
    //
    // It writes `menu.focus[page]` and nothing else. The keyboard block below
    // reads that back as its own starting cursor and previews whatever it ends
    // on, so hovering a row arms it through the *same* call to `preview` that
    // arrowing onto it uses — there is no second arming path to keep in step.
    // A click fires the same `Action` through the same `apply`.
    let mut fire: Option<Action> = None;
    if let Some(aim) = pointer.resolve(&nodes, menu.screen, &list) {
        if menu.focus[page] != aim.slot {
            menu.focus[page] = aim.slot;
            audio.play(crate::audio::Sfx::UiMove);
        }
        if aim.clicked {
            audio.play(crate::audio::Sfx::UiSelect);
            fire = Some(nodes.controls[aim.control as usize].action);
        }
    }

    // -- keyboard -----------------------------------------------------------
    #[allow(clippy::cast_possible_truncation)]
    let n = list.len() as u8;
    let cur = menu.cursor().min(n.saturating_sub(1));
    let back = keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::ArrowLeft);
    let fwd = keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::ArrowRight);
    let next = if back {
        (cur + n - 1) % n
    } else if fwd {
        (cur + 1) % n
    } else {
        cur
    };
    // The tube should sound like one: a relay tick as the cursor steps. Fired
    // only on an actual move, so holding a direction at a list end is silent
    // instead of buzzing against the wrap.
    if next != cur {
        audio.play(crate::audio::Sfx::UiMove);
    }
    menu.focus[page] = next;
    // Arrowing onto a row *is* choosing it. The armory's detail panel and the
    // solo brief both describe "the selection", and a cursor that moved without
    // changing it left the two disagreeing — the ladder highlighting one rung
    // while the panel described another.
    //
    // Unless the row is dead, which is the whole of [`arms_on_focus`] and the
    // bug it exists to stop. See there.
    let def = &nodes.controls[list[next as usize] as usize];
    let focused = def.action;
    if arms_on_focus(def, &menu, &data) {
        preview(focused, &mut menu);
    }

    // -- typing -------------------------------------------------------------
    //
    // The cursor resting on a field *is* the caret: there is one text input in
    // the whole client and it has no caret model, no selection and no
    // clipboard. Characters are consumed only while the cursor is on a field,
    // so `W`/`A`/`S`/`D` are steering everywhere else, exactly as before.
    let editing = matches!(focused, Action::Edit(_));
    if menu.typing != editing {
        menu.typing = editing;
    }
    if let Action::Edit(field) = focused {
        type_into_form(&mut typed, field, &mut menu);
    } else {
        // Drained regardless, so a key pressed on another page cannot be
        // delivered to a field the next time one is focused.
        typed.clear();
    }

    let was = menu.screen;
    // Cloned rather than borrowed: `apply` takes `&mut LobbyData` (a purchase
    // moves the balance) and the callsign lives inside it.
    let callsign = data.pilot.callsign.clone();
    let mut ops = NetOps::new(&session, &mut config, &callsign);

    if keys.just_pressed(KeyCode::Escape) && !menu.pinned {
        audio.play(crate::audio::Sfx::UiBack);
        match menu.screen.back() {
            Some(s) => menu.go(s),
            // Nothing below the main page. If a match is paused behind this
            // menu, that is what `ESC` means here — resume it.
            None if menu.resumable => menu.open = false,
            None => menu.say("NO PAGE BELOW"),
        }
        on_screen_change(was, &mut menu, &mut ops);
        ops.drain(&mut outbox, &mut commands, &mut api);
        return;
    }
    if keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::NumpadEnter) {
        audio.play(crate::audio::Sfx::UiSelect);
        // `ENTER` in a form means "next field", and in the last field means
        // "submit" — which is what every login form on earth does, and what a
        // pilot will try before they think to arrow down to `IDENTIFY`.
        // Resolved here rather than in `apply`, because it is the *cursor* that
        // moves and only this function knows the page's cursor order.
        match focused {
            Action::Edit(Field::Callsign) => {
                menu.focus[page] = (next + 1) % n;
            }
            Action::Edit(Field::Secret) => fire = Some(Action::SignIn),
            _ => fire = Some(nodes.controls[list[next as usize] as usize].action),
        }
    }

    if let Some(action) = fire {
        apply(
            action,
            &mut menu,
            &mut data,
            &mut setup,
            &mut launch,
            &mut ops,
        );
        on_screen_change(was, &mut menu, &mut ops);
    }
    ops.drain(&mut outbox, &mut commands, &mut api);
}

/// Feeds one frame of keystrokes into the focused field.
///
/// `logical_key` rather than `key_code`, so the character is the one the
/// keyboard *layout* produces — a French `AZERTY` and a US `QWERTY` type the
/// same passphrase — and `text` in preference to it, because that is the field
/// winit fills for a composed or dead-key sequence.
///
/// Only `Pressed` events, and only ones that produced a character:
/// `Key::Backspace` is handled by name and everything else (`Tab`, `F5`, the
/// arrows, the modifiers) falls through and is ignored.
fn type_into_form(typed: &mut MessageReader<KeyboardInput>, field: Field, menu: &mut Menu) {
    let mut changed = false;
    for event in typed.read() {
        if !event.state.is_pressed() {
            continue;
        }
        match &event.logical_key {
            Key::Backspace => changed |= menu.form.backspace(field),
            Key::Character(text) => changed |= menu.form.push(field, text),
            // `Key::Space` is its own variant and does not arrive as a
            // `Character`, which is a real trap: without this a passphrase with
            // a space in it silently loses it.
            Key::Space => changed |= menu.form.push(field, " "),
            _ => {
                if let Some(text) = &event.text {
                    changed |= menu.form.push(field, text);
                }
            }
        }
    }
    if changed {
        menu.form.rev = menu.form.rev.wrapping_add(1);
    }
}

/// What a page change costs the socket.
///
/// Every route onto a page runs through here — a click, `ENTER`, `ESC`, and
/// [`follow_session`]'s own moves — so there is exactly one place that knows
/// "opening the network page means opening a socket" and it cannot be bypassed
/// by adding a fourth way to change page.
fn on_screen_change(was: Screen, menu: &mut Menu, ops: &mut NetOps) {
    if menu.screen == was {
        return;
    }
    match menu.screen {
        // The `DATA LINK` panel is only honest if there is a link, and every
        // page below this one needs one anyway.
        Screen::Net => ops.open_link(),
        // A cached room list is a list of rooms that have since filled up.
        Screen::Browser => {
            ops.open_link();
            menu.pending = Some(Pending::List);
        }
        // The four pages that show the server's numbers ask for them on the way
        // in. Unforced, so `api.rs`'s cooldown makes walking between them one
        // round trip rather than four — and so a client that never opens any of
        // them never makes a request at all, which is what keeps a guest's
        // session genuinely silent.
        Screen::Record | Screen::Standings | Screen::Armory | Screen::Auth => {
            ops.ask(ApiRequest::Refresh { force: false });
        }
        _ => {}
    }

    // Giving the seat up.
    //
    // `server/index.js` answers `leave` with **nothing at all** — it just drops
    // the socket out of `room.players` — so the client has to decide for itself
    // when it is no longer in a room, and this is where. Two cases, and the
    // second is the one that bites:
    //
    // - Walking off the network rail with a seat booked leaves a ghost in
    //   somebody else's crew room until the socket closes.
    // - **Coming back to the network page after a match.** The room stays on
    //   the server with `started` set, so a client that did not say `leave`
    //   sits in [`Phase::Playing`] forever, and every later `create` or `join`
    //   is queued behind a phase that will never reach [`Phase::Idle`]. That
    //   read as "the CREATE SORTIE button stopped working after one match".
    let booked = matches!(ops.session.phase, Phase::Room | Phase::Playing);
    let left_the_rail = was.is_network() && !menu.screen.is_network();
    let back_at_the_top = menu.screen == Screen::Net && was != Screen::Net;
    if booked && (left_the_rail || back_at_the_top) {
        ops.send(ClientMessage::Leave);
    }
}

/// The side effects an [`Action`] can have on the server — over the socket, and
/// over HTTP.
///
/// Collected rather than written directly. [`apply`] is called from
/// [`read_input`] *and* from this module's tests, and a `MessageWriter` cannot
/// be built without a `World` — so the actions push onto these three vectors and
/// the caller drains them. It also keeps the ordering obvious: every frame's
/// commands go out after every frame's decisions, never interleaved with them.
struct NetOps<'a> {
    session: &'a NetSession,
    config: &'a mut NetConfig,
    /// What the socket should announce. Mirrored onto [`NetConfig::callsign`]
    /// so it is in place before the handshake finishes.
    callsign: &'a str,
    send: Vec<ClientMessage>,
    commands: Vec<NetCommand>,
    /// REST requests, for `api.rs` to turn into HTTP. The lobby never builds a
    /// request; it says what it wants.
    api: Vec<ApiRequest>,
}

impl<'a> NetOps<'a> {
    fn new(session: &'a NetSession, config: &'a mut NetConfig, callsign: &'a str) -> NetOps<'a> {
        NetOps {
            session,
            config,
            callsign,
            send: Vec::new(),
            commands: Vec::new(),
            api: Vec::new(),
        }
    }

    fn send(&mut self, msg: ClientMessage) {
        self.send.push(msg);
    }

    fn ask(&mut self, request: ApiRequest) {
        self.api.push(request);
    }

    /// Asks for a socket, and makes sure the name that goes out with it is the
    /// pilot's.
    ///
    /// `Connect` is idempotent — `run_commands` ignores it when a socket
    /// already exists — so this is safe to call on every visit to the page.
    fn open_link(&mut self) {
        if self.config.callsign != self.callsign {
            self.config.callsign = self.callsign.to_owned();
        }
        self.commands.push(NetCommand::Connect);
    }

    fn drain(
        self,
        out: &mut MessageWriter<ToServer>,
        cmds: &mut MessageWriter<NetCommand>,
        api: &mut MessageWriter<ApiRequest>,
    ) {
        for cmd in self.commands {
            cmds.write(cmd);
        }
        for msg in self.send {
            out.write(ToServer(msg));
        }
        for request in self.api {
            api.write(request);
        }
    }
}

/// Whether moving the cursor onto this control should arm what it names.
///
/// # The bug this is
///
/// *"I can see my friend's lobby but I can't physically join it."*
///
/// [`Screen::Browser`] is a **fixed pool of six rows** — the tree is built once,
/// so the browser cannot grow — and the rows past the end of the live list read
/// dead. `JOIN` sits below all six. So reaching it from the keyboard means
/// arrowing *through* the five empty rows, and every one of them armed itself
/// on the way past, because [`preview`] was called on whatever the cursor
/// landed on. By the time the cursor reached `JOIN`, [`Menu::sortie`] was `5` —
/// an empty slot — and `Action::JoinSortie` looked up row 5 of a one-room list,
/// found nothing, and said `NO SUCH SORTIE`.
///
/// The room was on screen the whole time, on row 0, exactly as the player said.
///
/// It is also why it looked intermittent: a mouse that jumps straight to `JOIN`
/// never crosses the empty rows, so clicking worked and arrowing did not.
///
/// The rule, therefore: **a row that reads dead cannot be armed**, which is the
/// `disabled` flag [`control_state`] already computes and paints from. One
/// definition of "this row is not a row" now serves the paint, the arming, and
/// [`apply`]'s refusal, so they cannot disagree again. A locked circuit or
/// operation behaves the same way — the cursor may rest on it, but the armed
/// tasking stays whatever was last actually chosen, which is what the brief
/// beside it goes on describing.
fn arms_on_focus(def: &ControlDef, menu: &Menu, data: &LobbyData) -> bool {
    let (_, disabled, _) = control_state(def, menu, data);
    !disabled
}

/// Moves the selection a control stands for, without the side effects of
/// activating it. Everything that is a *choice* previews; everything that is an
/// *action* — a page change, a purchase, a launch — does not.
fn preview(action: Action, menu: &mut Menu) {
    match action {
        Action::SetSolo(p) => menu.solo = p,
        Action::SetTrial(t) => menu.trial = t,
        Action::SetMission(m) => menu.mission = m,
        Action::SetSortie(s) => menu.sortie = s,
        Action::SetItem(i) => menu.item = i,
        // The paint applies as the cursor passes over it. That is the whole
        // point of a live preview: you look at the aircraft, not at the list.
        Action::SetHull(i) => menu.hull = i,
        Action::SetAccent(i) => menu.accent = i,
        Action::SetTrail(i) => menu.trail = i,
        _ => {}
    }
}

/// Carries out one activation.
fn apply(
    action: Action,
    menu: &mut Menu,
    data: &mut LobbyData,
    setup: &mut MatchSetup,
    launch: &mut MessageWriter<LaunchRequest>,
    ops: &mut NetOps,
) {
    match action {
        // -- network --------------------------------------------------------
        //
        // None of these writes a frame straight out. `flush_outbox` drops
        // anything sent while the socket is not open, so the ones that need a
        // live link go through [`Menu::pending`] and [`follow_session`] posts
        // them the moment there is one. `start` and `leave` are the exceptions:
        // both are only reachable from the crew room, which cannot be on screen
        // without a socket.
        Action::CycleEndpoint => {
            if ops.config.cycle_endpoint() {
                ops.commands.push(NetCommand::Reconnect);
                menu.say("SERVER CHANGED");
            } else {
                menu.say("SERVER IS THIS PAGE");
            }
        }
        Action::Transmit => {
            ops.open_link();
            menu.pending = Some(Pending::Create);
            menu.say("TRANSMITTING");
        }
        Action::Refresh => {
            ops.open_link();
            menu.pending = Some(Pending::List);
            menu.say("LOOKING FOR GAMES");
        }
        Action::JoinSortie => match ops.session.rooms.get(usize::from(menu.sortie)) {
            Some(room) => {
                ops.open_link();
                menu.pending = Some(Pending::Join(room.code.clone()));
                menu.say("JOINING");
            }
            None => menu.say("GAME NOT FOUND"),
        },
        Action::LaunchNet => {
            if ops.session.host {
                ops.send(ClientMessage::Start);
                menu.say("LAUNCHING");
            } else {
                menu.say("ONLY THE HOST CAN START");
            }
        }
        Action::LeaveRoom => {
            ops.send(ClientMessage::Leave);
            menu.go(Screen::Net);
        }

        Action::Go(s) => menu.go(s),
        // The three tasking rows *are* the launch. Choosing a mode used to arm
        // a preference and leave the pilot to find `EXECUTE` on another page,
        // which is the complaint this shape answers: a row that names a match
        // starts that match. Arrowing or hovering over one still only arms it —
        // that is `preview`, and it is what keeps the brief beside the list
        // describing the row under the cursor.
        Action::SetSolo(p) => {
            menu.solo = p;
            execute(menu, data, setup, launch);
        }
        Action::SetTrial(t) => {
            if trial_locked(t) {
                menu.say("FINISH THE PREVIOUS TRIAL");
                return;
            }
            menu.trial = t;
            execute(menu, data, setup, launch);
        }
        Action::SetMission(m) => {
            if mission_locked(m) {
                menu.say("FINISH THE PREVIOUS MISSION");
                return;
            }
            menu.mission = m;
            execute(menu, data, setup, launch);
        }
        Action::SetMap(m) => {
            menu.map = m;
            menu.say("THEATRE SET");
        }
        Action::SetPrivate(v) => {
            menu.private = v;
            menu.say("ACCESS SET");
        }
        Action::SetAutoBot(v) => {
            menu.auto_bot = v;
            menu.say(if v {
                "TEAMS WILL BE EVENED"
            } else {
                "NO AUTO-FILL - EMPTY MAP"
            });
        }
        Action::SetScheme(s) => {
            menu.scheme = s;
            menu.say("SCHEME SET");
        }
        Action::SetSortie(s) => {
            // `ENTER` on an empty slot is the same mistake the cursor used to
            // make on its way past one — see [`arms_on_focus`]. Refuse it here
            // too, so there is no route by which the armed sortie is a row that
            // is not there.
            if usize::from(s) >= ops.session.rooms.len() {
                menu.say("GAME NOT FOUND");
                return;
            }
            menu.sortie = s;
            menu.say("ORDER SELECTED");
        }
        Action::SetItem(i) => menu.item = i,
        Action::SetHull(i) => {
            menu.hull = i;
            save_colour("spaceships:shipColor", LIVERY[usize::from(i)].1);
            menu.say("HULL APPLIED");
        }
        Action::SetAccent(i) => {
            menu.accent = i;
            save_colour("spaceships:shipAccentColor", LIVERY[usize::from(i)].1);
            menu.say("ACCENT APPLIED");
        }
        Action::SetTrail(i) => {
            menu.trail = i;
            save_setting(
                "spaceships:trailShape",
                &TRAIL_SHAPES[usize::from(i)].to_ascii_lowercase(),
            );
            menu.say("TRAIL APPLIED");
        }
        Action::Toggle(flag) => {
            let i = flag as usize;
            menu.flags[i] = !menu.flags[i];
            if flag == Flag::Hard {
                menu.hard = menu.flags[i];
            }
            menu.say("SETTING CHANGED");
        }
        Action::Volume(ch) => {
            let v = &mut menu.volume[ch as usize];
            *v = if *v >= 10 { 0 } else { *v + 1 };
            menu.say("LEVEL SET");
        }
        // Requisition is the one action that spends something real, so it is
        // the one that must not be optimistic. The balance moves when the
        // *server* says it did — `api.rs` writes it back out of the reply — and
        // this only asks.
        Action::Requisition => {
            let i = menu.item as usize;
            match stock(i, data) {
                Stock::Available => match unlock_key(i) {
                    Some(feature) => {
                        ops.ask(ApiRequest::Unlock(feature));
                        menu.say("REQUISITIONING");
                    }
                    // Eleven of the sixteen rungs are `BACKLOG.md`'s planned
                    // ladder and have no column in `pilots.db`. Charging real
                    // credits for a bit that exists only in this process would
                    // be worse than refusing, and pretending it worked until
                    // the next refresh silently un-bought it would be worse
                    // still. See `api::UNLOCKS`.
                    None => menu.say("NOT IN SERVICE"),
                },
                Stock::Held => menu.say("ALREADY HELD"),
                Stock::Short => menu.say("INSUFFICIENT BALANCE"),
                Stock::Locked => menu.say("BUY THE PREVIOUS TIER FIRST"),
            }
        }

        // -- the account ----------------------------------------------------
        //
        // Nothing here touches a token or a URL: `api.rs` owns both, and the
        // lobby's whole vocabulary is these four requests.
        Action::Edit(_) => {
            // Typing is not an activation — [`type_into_form`] handles it while
            // the cursor rests here. `ENTER` is caught in `read_input` before it
            // reaches this arm, so this is only reached by a click on the row.
            menu.say("TYPE, THEN SELECT IDENTIFY");
        }
        Action::SignIn | Action::Enlist => {
            if menu.acct.busy {
                menu.say("WORKING");
                return;
            }
            if let Some(complaint) = menu.form.complaint() {
                menu.say(complaint);
                return;
            }
            let username = menu.form.callsign.clone();
            let password = std::mem::take(&mut menu.form.secret);
            // Cleared the moment it is handed over, so it lives in the menu for
            // as few frames as possible and a screenshot of a paused lobby can
            // never contain it.
            menu.form.rev = menu.form.rev.wrapping_add(1);
            if action == Action::Enlist {
                ops.ask(ApiRequest::Enlist { username, password });
                menu.say("SIGNING UP");
            } else {
                ops.ask(ApiRequest::SignIn { username, password });
                menu.say("IDENTIFYING");
            }
        }
        Action::SignOut => {
            if menu.acct.signed_in {
                ops.ask(ApiRequest::SignOut);
            } else {
                menu.say("NO PILOT SIGNED IN");
            }
        }
        Action::RefreshData => {
            ops.ask(ApiRequest::Refresh { force: true });
            menu.say("REFRESHING");
        }

        Action::Execute => execute(menu, data, setup, launch),
    }
}

/// Leaves the display and flies the armed selection.
///
/// The one place [`MatchSetup`] and [`LaunchRequest`] are written, reached from
/// [`Action::Execute`] on the mission board and from the tasking rows that name
/// a match directly. `sim_bridge.rs` consumes the message and rebuilds the
/// world; nothing else here has to know that.
fn execute(
    menu: &mut Menu,
    data: &LobbyData,
    setup: &mut MatchSetup,
    launch: &mut MessageWriter<LaunchRequest>,
) {
    let next = menu.setup(data);
    let online = !next.mode.is_solo();
    // `MatchSetup` is `sim_bridge`'s input and is written exactly here: toying
    // with the theatre selector must not touch the simulation's configuration.
    *setup = next.clone();
    launch.write(LaunchRequest {
        setup: next,
        online,
    });
    menu.open = false;
    menu.say("STARTING");
}

/// Circuits 3 and 4, and operation 3, are not flown yet.
///
/// One definition each, because [`control_state`] paints from them and [`apply`]
/// refuses from them — and a row that reads as locked and launches anyway is
/// worse than either behaviour on its own. They became load-bearing the moment
/// a tasking row started launching on activation rather than arming.
fn trial_locked(t: u8) -> bool {
    t >= 2
}

fn mission_locked(m: u8) -> bool {
    m >= 2
}

/// `SPACESHIPS_ROOM`: open straight into a networked room.
///
/// The same idea as [`forced_screen`] and for the same reason — a check that
/// needs two clients in one match should not need somebody sitting at both
/// keyboards.
///
/// - `SPACESHIPS_ROOM=ABCD` — connect and join that code.
/// - `SPACESHIPS_ROOM=new` — connect and create a room with whatever the
///   `CREATE SORTIE` page defaults to (open, space, auto-fill on).
///
/// It goes through exactly the same [`Menu::pending`] path a button press does,
/// so it exercises the shipped flow rather than a test-only shortcut. The one
/// thing it adds is the [`NetCommand::Connect`] the page visit would have sent.
fn forced_room(mut menu: ResMut<Menu>, mut commands: MessageWriter<NetCommand>) {
    let Some(code) = forced_room_code() else {
        return;
    };
    menu.screen = Screen::Net;
    menu.pending = Some(if code.eq_ignore_ascii_case("new") {
        Pending::Create
    } else {
        Pending::Join(code.to_uppercase())
    });
    menu.say("OPENING DATA LINK");
    commands.write(NetCommand::Connect);
}

/// `std::env::var` on `wasm32-unknown-unknown` always returns `Err`, so this
/// needs no `cfg` — the same trick `MatchSetup::from_env` uses.
fn forced_room_code() -> Option<String> {
    std::env::var("SPACESHIPS_ROOM")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// Follows the socket: mirrors [`NetSession`] onto [`Menu`], posts whatever was
/// waiting for a link, and moves the page when the *server* decides something.
///
/// Three things happen on this side of a lobby that the player did not ask for,
/// and all three are here:
///
/// - **The room reply.** `create` and `join` are both answered with `room`, so
///   "did it work" is a message, not a return value. When the phase reaches
///   [`Phase::Room`] the crew room comes up, whichever page asked for it.
/// - **The host pressing start.** Every other client learns the match has begun
///   from a `start` frame. The menu has to get out of the way, and it must be
///   the *menu* that closes rather than the match that starts, because
///   `sim_bridge::fixed_tick` freezes the world while this is up — a lobby left
///   open over a live match is a player sitting still while everyone shoots at
///   them.
/// - **Losing the room.** A dropped socket, a `leave`, or a host who left takes
///   the crew room with it, and a page listing four pilots who are no longer
///   there is worse than an empty one.
///
/// Each of the three is guarded on the page it applies to rather than on the
/// phase alone, which is what stops the second one fighting `ESC`: pausing a
/// networked match reopens the menu on [`Screen::Main`], the phase is still
/// [`Phase::Playing`], and without that guard this would slam it shut again on
/// the very next frame.
fn follow_session(
    mut session: ResMut<NetSession>,
    data: Res<LobbyData>,
    mut account: ResMut<Account>,
    mut config: ResMut<NetConfig>,
    mut menu: ResMut<Menu>,
    mut outbox: MessageWriter<ToServer>,
) {
    if config.callsign != data.pilot.callsign {
        config.callsign.clone_from(&data.pilot.callsign);
    }

    menu.watch(NetView::of(&session));

    // The account's mirror, on the same terms as the session's: guarded, so an
    // unchanged account marks nothing dirty.
    let acct = AccountView {
        rev: account.rev,
        signed_in: account.signed_in(),
        busy: account.busy > 0,
    };
    if menu.acct != acct {
        menu.acct = acct;
    }

    // What the REST client had to say. Passed through as the server wrote it —
    // "Callsign already taken", "Invalid callsign or password" — because those
    // are the two messages a pilot signing in actually needs, and paraphrasing
    // them into one house string would hide which of the two happened. This is
    // why [`Menu::notice`] is a `String`.
    if let Some(message) = account.take_notice() {
        menu.say(message);
    }

    // The socket's errors are a closed set, so they *are* paraphrased: there
    // are exactly two `server/index.js` emits, and both read better as the
    // lobby's own words.
    if let Some(message) = session.take_notice() {
        menu.say(match message.as_str() {
            "Room not found" => "GAME NOT FOUND",
            "Game already started" => "GAME ALREADY STARTED",
            _ => "SERVER REFUSED",
        });
        menu.pending = None;
    }

    if session.phase == Phase::Idle {
        if let Some(pending) = menu.pending.take() {
            outbox.write(ToServer(match pending {
                Pending::List => ClientMessage::ListRooms,
                Pending::Create => ClientMessage::Create {
                    private: menu.private,
                    map: wire_map(menu.map),
                    allow_bot: menu.auto_bot,
                },
                Pending::Join(code) => ClientMessage::Join { code },
            }));
        }
    }

    match session.phase {
        Phase::Room if matches!(menu.screen, Screen::Create | Screen::Browser | Screen::Net) => {
            menu.go(Screen::Waiting);
        }
        Phase::Playing if menu.open && menu.screen.is_network() => {
            menu.open = false;
            // The match is behind the menu now, so `ESC` means "resume" rather
            // than "there is no page below".
            menu.resumable = true;
            menu.say("STARTING");
        }
        Phase::Offline | Phase::Idle if menu.screen == Screen::Waiting => {
            menu.go(Screen::Net);
        }
        _ => {}
    }
}

/// Advances the self test, and steps off it when it finishes.
fn advance_boot(time: Res<Time>, mut menu: ResMut<Menu>) {
    if !menu.open || menu.screen != Screen::Boot || menu.pinned {
        return;
    }
    if menu.opened_at == 0.0 && time.elapsed_secs() > 0.0 {
        menu.opened_at = time.elapsed_secs();
    }
    if time.elapsed_secs() - menu.opened_at >= BOOT_LINES as f32 * BOOT_STEP + BOOT_HOLD {
        menu.screen = Screen::Main;
        menu.say("READY");
    }
}

/// Keys the display navigates with, which a text field must not swallow.
///
/// Everything else is a character while a field has the cursor. See
/// [`swallow_typing`].
const NAVIGATION_KEYS: [KeyCode; 7] = [
    KeyCode::Enter,
    KeyCode::NumpadEnter,
    KeyCode::Escape,
    KeyCode::ArrowUp,
    KeyCode::ArrowDown,
    KeyCode::ArrowLeft,
    KeyCode::ArrowRight,
];

/// While a field has the cursor, the keyboard belongs to the form and to
/// nothing else.
///
/// # The bug this is
///
/// Typing a passphrase pressed every hotkey in it. `L` toggles fullscreen
/// (`input.rs`), `V` toggles the cockpit view (`cockpit.rs`), and both read
/// `ButtonInput<KeyCode>` directly with no idea a lobby exists — so
/// `hello` went fullscreen twice and `viper` seated the pilot in a cockpit
/// behind the menu.
///
/// The arbitration belongs here for the same reason [`hold_the_stick`]'s and
/// [`release_the_pointer`]'s do: those modules have no reason to know about a
/// lobby, and [`LobbyOpen`] is this module's *export*, not its import. What is
/// different is that this one cannot be done by zeroing an output afterwards —
/// a fullscreen toggle has already happened by then — so it consumes the press
/// instead, in `PreUpdate`, immediately after `bevy_input` publishes it and
/// before any consumer in `Update` runs.
///
/// It clears **everything except [`NAVIGATION_KEYS`]** rather than a list of
/// known hotkeys, so a hotkey added anywhere in this crate tomorrow is
/// suppressed without anyone remembering to come back here.
///
/// `Menu::typing` is one frame old — [`read_input`] sets it in `Update`. That
/// is the frame the cursor *arrives* on a field, on which nothing has been
/// typed yet.
fn swallow_typing(menu: Res<Menu>, mut keys: ResMut<ButtonInput<KeyCode>>) {
    if !menu.open || !menu.typing {
        return;
    }
    let swallowed: Vec<KeyCode> = keys
        .get_just_pressed()
        .copied()
        .filter(|k| !NAVIGATION_KEYS.contains(k))
        .collect();
    // Guarded: an unchanged `ButtonInput` must not be marked dirty every frame
    // the cursor happens to rest on a field.
    if swallowed.is_empty() {
        return;
    }
    for key in swallowed {
        keys.clear_just_pressed(key);
        // Released as well as un-pressed, so a held key cannot leave a consumer
        // that reads `pressed` (rather than `just_pressed`) stuck down.
        keys.release(key);
    }
}

/// While the display is up, the stick is held: no throttle, no steering, no
/// trigger.
///
/// `input.rs` writes [`PlayerInput`] unconditionally in `PreUpdate`, so
/// navigating a menu with the arrow keys would otherwise fly the ship. This
/// runs before the whole `BeforeFixedMainLoop` set — where `sim_bridge`'s edge
/// latch lives — so the edges never get latched either, and pressing `ENTER` on
/// a page does not also loose a missile.
fn hold_the_stick(menu: Res<Menu>, mut input: ResMut<PlayerInput>) {
    if !menu.open {
        return;
    }
    let id = input.0.id;
    input.0 = sim::world::Input {
        id,
        ..Default::default()
    };
}

/// ...and while the display is up the pointer belongs to the pilot, not to the
/// aim.
///
/// `input.rs::grab_cursor` takes pointer lock on the first left click and hides
/// the cursor, which is exactly right for flying and fatal for a menu: a locked
/// pointer stops reporting a position, and an invisible one cannot be aimed at
/// a row. That module has no reason to know a lobby exists — [`LobbyOpen`] is
/// this module's export, not its import — so the arbitration belongs here,
/// beside [`hold_the_stick`], which stands the stick down for the same reason.
///
/// The scheduling does the rest. The grab is taken in `PreUpdate`, this runs in
/// `RunFixedMainLoop` just after it, [`read_input`] reads the cursor in
/// `Update` after that, and `bevy_winit` only applies [`CursorOptions`] in
/// `Last` — so the lock is undone before the window manager ever sees it, and
/// the pointer keeps moving. Both writes are guarded, so an open display costs
/// two comparisons a frame and marks nothing changed.
fn release_the_pointer(
    menu: Res<Menu>,
    mut cursors: Query<&mut CursorOptions, With<PrimaryWindow>>,
) {
    if !menu.open {
        return;
    }
    let Ok(mut cursor) = cursors.single_mut() else {
        return;
    };
    if cursor.grab_mode != CursorGrabMode::None {
        cursor.grab_mode = CursorGrabMode::None;
    }
    if !cursor.visible {
        cursor.visible = true;
    }
}

/// Keeps the off-screen target the size of the window.
///
/// Compares two integers against what it last applied and returns; a window
/// that is not being dragged costs nothing. Bevy's `Window` component changes
/// for reasons that have nothing to do with size, so `Changed<Window>` would
/// not be the cheaper test it looks like.
fn fit_tube(
    windows: Query<&Window>,
    nodes: Option<Res<MenuNodes>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<Crt>>,
    mut applied: Local<Option<UVec2>>,
) {
    let (Some(nodes), Ok(window)) = (nodes, windows.single()) else {
        return;
    };
    let phys = UVec2::new(window.physical_width(), window.physical_height());
    if phys.x == 0 || phys.y == 0 || *applied == Some(phys) {
        return;
    }
    *applied = Some(phys);

    if let Some(mut image) = images.get_mut(&nodes.target) {
        image.resize(Extent3d {
            width: phys.x,
            height: phys.y,
            depth_or_array_layers: 1,
        });
    }
    if let Some(mut crt) = materials.get_mut(&nodes.material) {
        *crt = Crt::new(
            nodes.target.clone(),
            Vec2::new(window.width().max(1.0), window.height().max(1.0)),
            crt_on(),
        );
    }
}

// ---------------------------------------------------------------------------
// The diff
// ---------------------------------------------------------------------------

/// Brings the tree in line with the model, writing only what moved.
///
/// The shape, from `hud.rs`: build the model, compare it whole, and **return
/// before acquiring a single `Mut`** if it matches. Everything after the
/// early-out is behind its own field comparison, so the cost of a frame is
/// proportional to what changed rather than to how many nodes exist.
#[expect(
    clippy::too_many_arguments,
    reason = "one query per component type written. Splitting the system would \
              split the single comparison that makes it cheap."
)]
fn drive_menu(
    time: Res<Time>,
    menu: Res<Menu>,
    data: Res<LobbyData>,
    account: Res<Account>,
    net: Res<NetStatus>,
    session: Res<NetSession>,
    config: Res<NetConfig>,
    nodes: Option<ResMut<MenuNodes>>,
    mut applied: ResMut<Applied>,
    mut q_node: Query<&mut Node>,
    mut q_bg: Query<&mut BackgroundColor>,
    mut q_text: Query<&mut Text>,
    mut q_colour: Query<&mut TextColor>,
    mut q_xform: Query<&mut UiTransform>,
    mut q_vis: Query<&mut Visibility>,
    mut q_cam: Query<&mut Camera>,
) {
    let Some(mut nodes) = nodes else { return };

    let next = model(&menu, &data, &net, time.elapsed_secs());
    let prev = applied.0;

    // The early-out. In flight the model is `MenuModel::default()` on every
    // frame, so this system ends here having read four resources and compared
    // twelve integers. No `Mut` is taken, so no node is flagged changed, so
    // `bevy_ui` skips the whole tree in layout and the render world skips it in
    // extraction.
    if prev == Some(next) {
        return;
    }
    applied.0 = Some(next);

    macro_rules! moved {
        ($($field:ident),+ $(,)?) => {
            prev.is_none_or(|p| $(p.$field != next.$field)||+)
        };
    }

    // -- open / closed ------------------------------------------------------
    if moved!(open) {
        set_display(&mut q_node, nodes.root, next.open);
        set_display(&mut q_node, nodes.tube, next.open);
        // The off-screen pass is switched off with the display, so a menu that
        // is not up costs no render pass at all.
        if let Ok(mut camera) = q_cam.get_mut(nodes.camera) {
            camera.is_active = next.open;
        }
    }
    if !next.open {
        return;
    }

    // -- page ---------------------------------------------------------------
    //
    // `moved!(screen, open)`, and the previous page is only trusted when the
    // menu was *open* to draw it. A closed menu's model is a constant — that is
    // the early-out this module is built around — so `prev.screen` is `Boot`
    // for the whole of a match however the menu looked when it closed. Hiding
    // that one on the way back in left the page that was actually on screen
    // still displayed underneath the new one, which read as two pages printed
    // over each other. It took a networked match to make it obvious, because
    // that is the first flow that closes the menu from a page other than the
    // one it reopens on — but launching a skirmish from `SOLO OPERATIONS` and
    // pressing `ESC` did it too.
    if moved!(screen, open) {
        match page_to_hide(prev) {
            Some(page) => set_display(&mut q_node, nodes.pages[page], false),
            None => {
                for i in 0..nodes.pages.len() {
                    set_display(&mut q_node, nodes.pages[i], false);
                }
            }
        }
        set_display(&mut q_node, nodes.pages[next.screen as usize], true);
        set_text(&mut q_text, nodes.title, || {
            Screen::ALL[next.screen as usize].title().to_owned()
        });
    }
    if moved!(chrome) {
        set_display(&mut q_node, nodes.chrome, next.chrome);
    }
    if moved!(screen) {
        // One write: the airframe is visible exactly where the backdrop is not.
        //
        // `Visibility`, not `Display` — this is the case `hud.rs` documents.
        // The backdrop is the right-hand child of a row whose left child grows,
        // so taking it out of layout lets the left one expand over the whole
        // tube and hide the airframe anyway. It cost a build to find.
        set(
            &mut q_vis,
            nodes.backdrop,
            if Screen::ALL[next.screen as usize] == Screen::Livery {
                Visibility::Hidden
            } else {
                Visibility::Inherited
            },
        );
    }

    // -- header -------------------------------------------------------------
    if moved!(credits) {
        set_text(&mut q_text, nodes.credits, || thousands(next.credits));
    }

    // -- self test ----------------------------------------------------------
    if moved!(boot) {
        for (i, (_, result)) in BOOT_TEST.iter().enumerate() {
            let done = usize::from(next.boot) > i;
            let result = *result;
            set_text(&mut q_text, nodes.boot_status[i], || {
                if done { result } else { "" }.to_owned()
            });
            set(
                &mut q_colour,
                nodes.boot_status[i],
                TextColor(if matches!(result, "NO CARRIER" | "STALE") {
                    pal::AMBER
                } else {
                    pal::PHOSPHOR
                }),
            );
        }
        #[allow(clippy::cast_precision_loss)]
        let pct = f32::from(next.boot) / BOOT_LINES as f32 * 100.0;
        set_width(&mut q_node, nodes.boot_fill, pct);
    }

    // -- the scope ----------------------------------------------------------
    if moved!(sweep) {
        let turn = f32::from(next.sweep) / f32::from(SWEEP_STEPS) * std::f32::consts::TAU;
        set(
            &mut q_xform,
            nodes.sweep,
            UiTransform::from_rotation(Rot2::radians(turn)),
        );
    }

    // -- annunciators -------------------------------------------------------
    if moved!(link, caution, sel, credits) {
        for (i, (colour, label)) in lamp_states(&menu, &data, next).into_iter().enumerate() {
            set(&mut q_bg, nodes.lamps[i], BackgroundColor(colour));
            set_text(&mut q_text, nodes.lamp_values[i], || label.to_owned());
            set(&mut q_colour, nodes.lamp_values[i], TextColor(colour));
        }
    }

    // -- the network pages --------------------------------------------------
    //
    // One guard for all four: `NetView::rev` moves whenever any row does, and
    // `screen` covers walking onto a page whose rows have not changed since it
    // was last looked at.
    if moved!(net, screen) {
        write_network_pages(
            &nodes,
            &session,
            &config,
            &mut q_node,
            &mut q_text,
            &mut q_bg,
        );
    }
    if moved!(frames) {
        set_text(&mut q_text, nodes.net.link_values[2], || {
            format!("{} OUT   {} IN", net.sent, net.received)
        });
    }

    // -- the pilot's own pages ----------------------------------------------
    //
    // One guard for the mission board's header, the service record, the
    // standings, the circuits and the operations: `LobbyData::rev` moves
    // whenever any reply lands, and `screen` covers walking onto a page whose
    // numbers have not changed since it was last looked at. Every write inside
    // goes through `set_text`, which takes the `Mut` only when the string
    // actually differs — so a refresh that returned the same dossier costs
    // comparisons and no component writes at all.
    if moved!(data_rev, screen, acct, feed) {
        write_dossier(
            &nodes,
            &data,
            &account,
            &mut q_node,
            &mut q_text,
            &mut q_colour,
        );
    }
    if moved!(form, screen, focus) {
        write_auth_fields(&nodes, &menu, &mut q_text);
    }

    // -- footer -------------------------------------------------------------
    if moved!(notice) {
        set_text(&mut q_text, nodes.notice, || menu.notice.clone());
    }
    if moved!(clock) {
        set_text(&mut q_text, nodes.clock, || {
            format!("{:02}:{:02}", next.clock / 60, next.clock % 60)
        });
    }

    // -- the board's armed line --------------------------------------------
    if moved!(sel, screen) {
        let armed = menu.setup(&data);
        set_text(&mut q_text, nodes.armed, || {
            format!(
                "{}   /   {}   /   {}",
                tasking_name(&menu),
                match armed.map {
                    MapKind::Space => "SPACE",
                    MapKind::Terrain => "SIERRAS",
                },
                if armed.hard_mode { "HARD" } else { "STANDARD" },
            )
        });
        set(
            &mut q_colour,
            nodes.armed,
            TextColor(if armed.hard_mode {
                pal::RED
            } else {
                pal::WHITE
            }),
        );
    }

    // -- the solo brief and the audio levels --------------------------------
    if moved!(sel) {
        for (i, line) in brief_lines(&menu).into_iter().enumerate() {
            set_text(&mut q_text, nodes.brief[i], || line.to_owned());
        }
        for (i, &node) in nodes.volume.iter().enumerate() {
            let level = usize::from(menu.volume[i]).min(10);
            set_text(&mut q_text, node, || {
                // A bar rather than a number: it is a level, and a level reads
                // faster as a length.
                let mut bar = "|".repeat(level);
                bar.push_str(&".".repeat(10 - level));
                bar
            });
        }
    }

    // -- the armory's detail ------------------------------------------------
    if moved!(sel, credits) {
        let i = usize::from(menu.item).min(ARMORY.len() - 1);
        let rung = &ARMORY[i];
        let st = stock(i, &data);
        set_text(&mut q_text, nodes.detail[0], || rung.name.to_owned());
        set_text(&mut q_text, nodes.detail[1], || {
            if rung.cost == 0 {
                "ISSUED".to_owned()
            } else {
                thousands(rung.cost)
            }
        });
        let (status, colour) = match st {
            Stock::Held => ("HELD", pal::PHOSPHOR),
            Stock::Available => ("AVAILABLE", pal::PHOSPHOR),
            Stock::Short => ("SHORT OF BALANCE", pal::ORANGE),
            Stock::Locked => ("LOCKED", pal::RED),
        };
        set_text(&mut q_text, nodes.detail[2], || {
            format!("{}   {}", rung.class, status)
        });
        set(&mut q_colour, nodes.detail[2], TextColor(colour));
        set_text(&mut q_text, nodes.detail[3], || {
            if st == Stock::Locked {
                format!(
                    "{}\n\nCLEAR {} FIRST.",
                    rung.blurb,
                    TIERS[rung.tier as usize - 1]
                )
            } else {
                rung.blurb.to_owned()
            }
        });
    }

    // -- control highlights -------------------------------------------------
    //
    // Guarded by one packed key rather than by fourteen comparisons, and the
    // pass writes only the controls whose state byte moved — the trick
    // `hud.rs::sync_pips` plays on a pip row, at a larger scale.
    if moved!(screen, focus, sel, credits) {
        let list = page_controls(&nodes, Screen::ALL[next.screen as usize]);
        let states: Vec<(usize, u8)> = nodes
            .controls
            .iter()
            .enumerate()
            .map(|(i, def)| {
                #[allow(clippy::cast_possible_truncation)]
                let slot = list.iter().position(|&c| c == i as u16);
                let mut st = 0;
                match slot {
                    None => st |= ST_DISABLED,
                    Some(s) => {
                        if s == usize::from(next.focus) {
                            st |= ST_FOCUS;
                        }
                        let (sel, dis, held) = control_state(def, &menu, &data);
                        if sel {
                            st |= ST_SELECTED;
                        }
                        if dis {
                            st |= ST_DISABLED;
                        }
                        if held {
                            st |= ST_HELD;
                        }
                    }
                }
                (i, st)
            })
            .collect();

        for (i, st) in states {
            if nodes.applied[i] == st {
                continue;
            }
            nodes.applied[i] = st;
            paint_control(&nodes.controls[i], st, &mut q_bg, &mut q_colour);
        }
    }
}

/// Which page the last applied model left on screen, if it can be named.
///
/// `None` means "no idea — hide them all", and it is the answer whenever the
/// previous model had the menu **closed**, because a closed menu's model is a
/// constant and its `screen` field says [`Screen::Boot`] no matter what was
/// showing when it closed.
///
/// # The bug this is
///
/// Hiding `prev.screen` unconditionally hid `Boot`, so the page that was
/// actually displayed when the menu closed stayed displayed, *underneath* the
/// one that came up on the way back in. Two pages printed over each other, and
/// the leftover was usually the boot self test or the crew room — which reads
/// exactly like "the client is stuck on the starting screen".
///
/// It also explains a pointer that locks with a menu apparently on screen:
/// `input.rs`'s `grab_cursor` keys off [`LobbyOpen`], which correctly says the
/// lobby is *closed*, so a click in flight takes the pointer lock as it should
/// — while a stale page is still drawn over the game.
///
/// The networked flow made it obvious because it is the first one that closes
/// the menu from a page other than the one it reopens on, but launching a
/// skirmish from `SOLO OPERATIONS` and pressing `ESC` did it too.
fn page_to_hide(prev: Option<MenuModel>) -> Option<usize> {
    match prev {
        Some(p) if p.open => Some(p.screen as usize),
        _ => None,
    }
}

/// Writes the pilot's own pages from [`LobbyData`].
///
/// The mission board's header, the service record, the standings, the circuits'
/// best times and the operations' state. All of it used to be baked into the
/// tree at startup from [`LobbyData::placeholder`] and never touched again,
/// which was fine while the numbers could not change; now they arrive over the
/// network a second after the page does, and this is what puts them on it.
// One query per component type written, on the same terms as
// `write_network_pages`: the alternative is four systems reading the same
// resource behind four copies of the same guard.
fn write_dossier(
    nodes: &MenuNodes,
    data: &LobbyData,
    account: &Account,
    q_node: &mut Query<&mut Node>,
    q_text: &mut Query<&mut Text>,
    q_colour: &mut Query<&mut TextColor>,
) {
    let d = &nodes.dossier;
    let p = &data.pilot;
    let hours = format!("{}:{:02}", p.flight_minutes / 60, p.flight_minutes % 60);
    let name = p.callsign.to_ascii_uppercase();

    // -- the mission board's header -----------------------------------------
    set_text(q_text, d.board_name, || name.clone());
    set_text(q_text, d.board_line, || {
        format!(
            "{}    {} KILLS    {} HRS",
            p.rank.to_ascii_uppercase(),
            thousands(p.kills),
            hours
        )
    });

    // -- the service record -------------------------------------------------
    set_text(q_text, d.record_name, || name.clone());
    set_text(q_text, d.record_line, || {
        format!(
            "{}    K/D {}    ENLISTED {}",
            p.rank.to_ascii_uppercase(),
            p.kdr,
            p.enlisted
        )
    });
    #[allow(clippy::cast_precision_loss)]
    let exchange = format!("{:.2}", p.kills as f32 / p.deaths.max(1) as f32);
    let stats = [
        hours,
        thousands(p.kills),
        exchange,
        thousands(p.matches_won + p.matches_lost),
        thousands(p.bots_killed),
        thousands(p.campaign_boss_kills),
    ];
    for (node, value) in d.stats.iter().zip(stats) {
        set_text(q_text, *node, || value);
    }
    let marks = (p.kills / 10).min(KILL_MARKS as u32) as usize;
    for (i, &mark) in d.marks.iter().enumerate() {
        set_display(q_node, mark, i < marks);
    }

    // -- the standings ------------------------------------------------------
    for (i, row) in d.standings.iter().enumerate() {
        let entry = data.standings.get(i);
        set_display(q_node, row.root, entry.is_some());
        let Some(s) = entry else { continue };
        let colour = if s.you {
            pal::AMBER
        } else {
            pal::rgba(0xea_f6_ff, 0.75)
        };
        set_text(q_text, row.position, || format!("{:>2}", i + 1));
        set_text(q_text, row.callsign, || s.callsign.to_ascii_uppercase());
        set_text(q_text, row.rank, || s.rank.to_ascii_uppercase());
        set_text(q_text, row.kills, || format!("{:>6}", thousands(s.kills)));
        set_text(q_text, row.wins, || format!("{:>5}", s.wins));
        set(q_colour, row.callsign, TextColor(colour));
        set(q_colour, row.kills, TextColor(colour));
        set(q_colour, row.wins, TextColor(colour));
    }

    // -- the circuits and the operations ------------------------------------
    for (i, &control) in d.trial_rows.iter().enumerate() {
        if control == usize::MAX {
            continue;
        }
        set_text(q_text, nodes.controls[control].value, || {
            format!(
                "{:>2} CP     {}",
                [12, 14, 16, 18][i],
                fmt_trial(p.trial_best[i])
            )
        });
    }
    for (i, &control) in d.mission_rows.iter().enumerate() {
        if control == usize::MAX {
            continue;
        }
        set_text(q_text, nodes.controls[control].value, || {
            match p.campaign_lives[i] {
                Some(l) => format!("{l} OF 3 LIVES"),
                None => "NOT FLOWN".to_owned(),
            }
        });
    }

    // -- the armory ladder --------------------------------------------------
    //
    // The rungs' right-hand column and the tier headings are the pilot's own
    // ladder, not a fixed table: a fresh account holds nothing, and this page
    // read as though it held the first three tiers for as long as it was built
    // once from the placeholder and never written again.
    for (i, &control) in d.rung_rows.iter().enumerate() {
        if control == usize::MAX {
            continue;
        }
        set_text(q_text, nodes.controls[control].value, || {
            match stock(i, data) {
                Stock::Held => "HELD".to_owned(),
                Stock::Locked => "LOCKED".to_owned(),
                _ => thousands(ARMORY[i].cost),
            }
        });
    }
    for (tier, &caption) in d.tier_captions.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let open = tier_open(tier as u8, p.owned);
        set(
            q_colour,
            caption,
            TextColor(if open { dim(0.55) } else { pal::DEAD }),
        );
    }

    // -- who is signed in ---------------------------------------------------
    set_text(q_text, d.identity, || match &account.username {
        Some(who) => who.to_ascii_uppercase(),
        None => "GUEST".to_owned(),
    });
    set(
        q_colour,
        d.identity,
        TextColor(if account.signed_in() {
            pal::PHOSPHOR
        } else {
            pal::AMBER
        }),
    );
}

/// Writes what is typed into [`Screen::Auth`]'s two fields.
///
/// Separate from [`write_dossier`] and behind its own guard, because it moves on
/// every keystroke while the dossier moves once a refresh — and because the
/// passphrase must reach exactly one node and nothing else. It is never in
/// [`MenuModel`], never in a log line, and never drawn: [`AuthForm::masked`] is
/// what comes out of here.
fn write_auth_fields(nodes: &MenuNodes, menu: &Menu, q_text: &mut Query<&mut Text>) {
    let caret = |s: String| if s.is_empty() { "_".to_owned() } else { s };
    set_text(
        q_text,
        nodes.dossier.fields[Field::Callsign as usize],
        || caret(menu.form.callsign.to_ascii_uppercase()),
    );
    set_text(q_text, nodes.dossier.fields[Field::Secret as usize], || {
        caret(menu.form.masked())
    });
}

/// Writes the four network pages from [`NetSession`].
///
/// Split out of [`drive_menu`] because it is the one block that reads a
/// non-`Copy` resource, and because it is long: a room browser, a crew room and
/// a link panel is three pages' worth of text behind one comparison.
///
/// Every write goes through [`set_text`]/[`set`], which take the `Mut` only
/// when the value actually differs — so walking onto the crew room with nothing
/// changed but the page costs a handful of comparisons rather than thirty
/// component writes.
fn write_network_pages(
    nodes: &MenuNodes,
    session: &NetSession,
    config: &NetConfig,
    q_node: &mut Query<&mut Node>,
    q_text: &mut Query<&mut Text>,
    q_bg: &mut Query<&mut BackgroundColor>,
) {
    let net = &nodes.net;

    // -- the link panel and the endpoint row --------------------------------
    set_text(q_text, net.link_values[0], || {
        config.endpoint_label().to_ascii_uppercase()
    });
    set_text(q_text, net.link_values[1], || {
        // "Guest" is what the server calls a connection with no token. This
        // reads `NetConfig::token` rather than `Account`, deliberately: it is
        // the `DATA LINK` panel, and what it must report is what *the socket*
        // is presenting — which is the token as of the last handshake, not the
        // one a sign-in acquired a moment ago. The two are reconciled by
        // `api::push_token_to_socket`, and until the reconnect lands this row
        // is right to say the link is still a guest's.
        let who = config.callsign.to_ascii_uppercase();
        if config.token.is_some() {
            format!("{who}  AUTH")
        } else {
            format!("{who}  GUEST")
        }
    });
    if net.endpoint_row != usize::MAX {
        set_text(q_text, nodes.controls[net.endpoint_row].value, || {
            config.endpoint_label().to_ascii_uppercase()
        });
    }

    // -- the room browser ---------------------------------------------------
    for (i, &row) in net.sortie_rows.iter().enumerate() {
        if row == usize::MAX {
            continue;
        }
        let def = &nodes.controls[row];
        match session.rooms.get(i) {
            Some(room) => {
                set_text(q_text, def.label, || room.code.to_ascii_uppercase());
                set_text(q_text, def.value, || {
                    format!(
                        "{:<16} {}/{}",
                        room.host_name.to_ascii_uppercase(),
                        room.player_count,
                        SEAT_ROWS
                    )
                });
            }
            None => {
                set_text(q_text, def.label, || "- - - -".to_owned());
                set_text(q_text, def.value, String::new);
            }
        }
    }

    // -- the crew room ------------------------------------------------------
    set_text(q_text, net.room_code, || {
        if session.code.is_empty() {
            "- - - -".to_owned()
        } else {
            session.code.clone()
        }
    });
    set_text(q_text, net.room_access, || {
        if session.private { "PRIVATE" } else { "OPEN" }.to_owned()
    });
    for (i, seat) in net.seats.iter().enumerate() {
        let row = session.players.get(i);
        set_display(q_node, seat.root, row.is_some());
        let Some(p) = row else { continue };
        set_text(q_text, seat.name, || {
            let mut name = p.name.to_ascii_uppercase();
            if p.is_bot {
                name.push_str("  [AI]");
            }
            name
        });
        set_text(q_text, seat.lead, || {
            if p.host { "LEAD" } else { "" }.to_owned()
        });
        // Teams are `null` until the host presses start, and an unassigned seat
        // reads as neither colour rather than defaulting to team 0's.
        set(
            q_bg,
            seat.tick,
            BackgroundColor(match p.team {
                Some(0) => pal::CYAN,
                Some(_) => pal::RED,
                None => pal::rgba(pal::GRID, 0.8),
            }),
        );
    }

    // The one row whose *label* changes: the flight lead launches, everyone
    // else stands by, and the row says which of the two you are before it is
    // pressed rather than after.
    if net.launch_row != usize::MAX {
        set_text(q_text, nodes.controls[net.launch_row].label, || {
            if session.host { "LAUNCH" } else { "STANDBY" }.to_owned()
        });
    }
}

/// Whether a control reads as selected, unavailable, or already held.
fn control_state(def: &ControlDef, menu: &Menu, data: &LobbyData) -> (bool, bool, bool) {
    match def.action {
        // Only a *rail* key lights up for the page you are on. A link in the
        // page body is a door, not a state, and the first pass had SOLO's two
        // doors reading as though they were already chosen.
        Action::Go(s) => (
            if def.screen == Screen::Boot {
                menu.screen.rail() == s.rail()
            } else {
                menu.screen == s
            },
            false,
            false,
        ),
        Action::SetSolo(p) => (menu.solo == p, false, false),
        Action::SetTrial(t) => (menu.trial == t, trial_locked(t), false),
        Action::SetMission(m) => (menu.mission == m, mission_locked(m), false),
        Action::SetMap(m) => (menu.map == m, false, false),
        Action::SetPrivate(v) => (menu.private == v, false, false),
        Action::SetAutoBot(v) => (menu.auto_bot == v, false, false),
        Action::SetScheme(s) => (menu.scheme == s, false, false),
        // A row past the end of the list is an empty slot: it reads dead and
        // `JOIN` refuses it. That is why the browser can be a fixed pool
        // without ever offering a room that is not there.
        Action::SetSortie(s) => (menu.sortie == s, s >= menu.net.rooms, false),
        // Only the flight lead may launch, and the row says so either way — see
        // the label swap in `drive_menu`.
        Action::LaunchNet => (false, !menu.net.host, false),
        Action::JoinSortie => (false, menu.net.rooms == 0, false),
        Action::CycleEndpoint | Action::Transmit | Action::Refresh | Action::LeaveRoom => {
            (false, false, false)
        }
        Action::SetHull(i) => (menu.hull == i, false, false),
        Action::SetAccent(i) => (menu.accent == i, false, false),
        Action::SetTrail(i) => (menu.trail == i, false, false),
        Action::SetItem(i) => {
            let i = usize::from(i);
            let st = stock(i, data);
            (
                usize::from(menu.item) == i,
                st == Stock::Locked,
                st == Stock::Held,
            )
        }
        Action::Toggle(flag) => (menu.flags[flag as usize], false, false),
        Action::Requisition => {
            let i = usize::from(menu.item);
            let st = stock(i, data);
            // Dead for a rung the server has never heard of, as well as for one
            // that cannot be afforded — so the row says "no" before it is
            // pressed rather than after. See `api::UNLOCKS`.
            (
                false,
                st != Stock::Available || unlock_key(i).is_none(),
                false,
            )
        }
        // A field is never "selected"; the cursor resting on it *is* the caret,
        // which is the whole of this form's input model.
        Action::Edit(_) => (false, false, false),
        // Both are dead while a request is in flight, so a second `ENTER` on a
        // slow link cannot queue a second sign-in behind the first.
        Action::SignIn | Action::Enlist => (false, menu.acct.busy, false),
        Action::SignOut => (false, !menu.acct.signed_in, false),
        Action::RefreshData => (false, menu.acct.busy, false),
        Action::Execute | Action::Volume(_) => (false, false, false),
    }
}

/// Writes one control's colours.
///
/// Four writes at most, and no border or background at rest: the cursor tick
/// and the text colour carry the whole state. That is the decluttering rule in
/// code — a highlighted row is brighter, not boxed.
fn paint_control(
    def: &ControlDef,
    st: u8,
    q_bg: &mut Query<&mut BackgroundColor>,
    q_colour: &mut Query<&mut TextColor>,
) {
    let focus = st & ST_FOCUS != 0;
    let selected = st & ST_SELECTED != 0;
    let disabled = st & ST_DISABLED != 0;
    let held = st & ST_HELD != 0;

    // Alpha is deceptive here: `bevy_ui` blends in **linear** space, so 10% of
    // a bright green over near-black lands around 35% in sRGB, and the phosphor
    // bleed then lifts it further. The first pass used 0.10/0.13 and produced a
    // solid bar rather than a wash.
    let (wash, text, tick) = if disabled {
        (Color::NONE, pal::DEAD, pal::rgba(0x16_22_2c, 0.8))
    } else if selected && focus {
        (pal::rgba(0xff_c4_51, 0.028), pal::AMBER, pal::AMBER)
    } else if selected {
        (Color::NONE, pal::AMBER, pal::AMBER)
    } else if focus {
        (pal::rgba(0x46_ff_9b, 0.022), pal::WHITE, pal::PHOSPHOR_LAMP)
    } else if held {
        (
            Color::NONE,
            pal::rgba(0x46_ff_9b, 0.5),
            pal::rgba(0x46_ff_9b, 0.3),
        )
    } else {
        (Color::NONE, dim(0.75), pal::rgba(pal::GRID, 0.6))
    };

    set(q_bg, def.root, BackgroundColor(wash));
    set(q_bg, def.tick, BackgroundColor(tick));
    set(q_colour, def.label, TextColor(text));
    if def.value != Entity::PLACEHOLDER {
        set(
            q_colour,
            def.value,
            TextColor(if disabled {
                pal::DEAD
            } else if selected {
                pal::AMBER
            } else {
                dim(0.5)
            }),
        );
    }
}

/// The annunciator strip's colour and value.
///
/// `cockpit.rs`'s `TGT`/`MSL` lamps at menu scale: the node is always there and
/// only its colour moves. Nothing appears or disappears, so no lamp can cause
/// an archetype move or a relayout.
fn lamp_states(
    menu: &Menu,
    data: &LobbyData,
    m: MenuModel,
) -> [(Color, &'static str); LAMPS.len()] {
    let link = match m.link {
        1 => (pal::AMBER, "CONNECTING"),
        2 => (pal::PHOSPHOR, "ONLINE"),
        3 => (pal::AMBER, "RETRY"),
        4 => (pal::RED, "FAILED"),
        // An unlit lamp, but the word beside it still has to be readable — the
        // pilot needs to know the link is down, not merely fail to see that it
        // is up.
        _ => (dim(0.4), "OFFLINE"),
    };
    // The lamp says, in [`DataSource`]'s own words, where the numbers on the
    // page came from: `CACHED` for this file's invented ones, `PUBLIC` for the
    // leaderboard with no account behind it, `LIVE` for a signed-in pilot's,
    // and `STALE` for real numbers a later refresh failed to renew. The two
    // that are not the server's current answer blink with the caution phase, so
    // a screenshot cannot quietly present them as if they were.
    let feed = if data.source.is_suspect() {
        (
            if m.caution { pal::AMBER } else { dim(0.4) },
            data.source.label(),
        )
    } else if data.source == DataSource::Public {
        (pal::CYAN, data.source.label())
    } else {
        (pal::PHOSPHOR, data.source.label())
    };
    let adv = if menu.hard {
        (pal::RED, "HARD")
    } else {
        (pal::rgba(0x46_ff_9b, 0.6), "STD")
    };
    [link, feed, adv]
}

/// What the armed selection is called on the board.
fn tasking_name(menu: &Menu) -> String {
    match menu.screen {
        Screen::Trials => format!("CIRCUIT {}", menu.trial + 1),
        Screen::Campaign => format!("OPERATION {}", menu.mission + 1),
        Screen::Net | Screen::Create | Screen::Browser | Screen::Waiting => {
            "MULTIPLAYER".to_owned()
        }
        _ => match menu.solo {
            SoloPick::Tutorial => "TUTORIAL",
            SoloPick::Train => "TRAIN",
            SoloPick::Skirmish => "SKIRMISH",
        }
        .to_owned(),
    }
}

/// The three values in [`Screen::Solo`]'s brief.
fn brief_lines(menu: &Menu) -> [&'static str; 3] {
    match menu.solo {
        SoloPick::Tutorial => ["TUTORIAL", "NONE", "UNLIMITED"],
        SoloPick::Train => ["DEFEAT ONE ENEMY", "1 BOT", "3:00"],
        SoloPick::Skirmish => ["TEAM DEATHMATCH", "5 BOTS / 4 ALLIED", "5:00"],
    }
}

// ---------------------------------------------------------------------------
// Write helpers
// ---------------------------------------------------------------------------
//
// Copies of `hud.rs`'s, which are private to that module. Each takes the `Mut`
// only inside a branch that has already decided a write is needed; `set_if_neq`
// is a second line of defence rather than the mechanism.

fn set<T: Component<Mutability = bevy::ecs::component::Mutable> + PartialEq>(
    q: &mut Query<&mut T>,
    entity: Entity,
    value: T,
) {
    if let Ok(mut current) = q.get_mut(entity) {
        current.set_if_neq(value);
    }
}

/// `Display::None` rather than `Visibility::Hidden`, which is where this
/// departs from `hud.rs` — see the module docs.
fn set_display(q: &mut Query<&mut Node>, entity: Entity, shown: bool) {
    if let Ok(mut node) = q.get_mut(entity) {
        let want = display_of(shown);
        if node.display != want {
            node.display = want;
        }
    }
}

fn set_width(q: &mut Query<&mut Node>, entity: Entity, pct: f32) {
    if let Ok(mut node) = q.get_mut(entity) {
        let want = percent(pct);
        if node.width != want {
            node.width = want;
        }
    }
}

/// The closure defers the `format!` until the node is known to exist and the
/// value is known to have changed.
fn set_text(q: &mut Query<&mut Text>, entity: Entity, value: impl FnOnce() -> String) {
    if let Ok(mut text) = q.get_mut(entity) {
        let value = value();
        if text.0 != value {
            text.0 = value;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet() -> (Menu, LobbyData, NetStatus) {
        let menu = Menu {
            open: true,
            screen: Screen::Main,
            pinned: true,
            ..Menu::default()
        };
        (menu, LobbyData::placeholder(), NetStatus::default())
    }

    /// An app with nothing in it but the two resources and the mirror. The
    /// system touches no entity, no asset and no window, so this needs no
    /// plugins — `App::default` brings the `Main` schedule and that is all
    /// `Update` requires.
    fn mirror_app(first_person: bool) -> App {
        let mut app = App::new();
        app.insert_resource(Menu::default())
            .insert_resource(ViewMode {
                first_person,
                seated: false,
            })
            .add_systems(Update, mirror_cockpit_flag);
        app
    }

    fn row(app: &App) -> bool {
        app.world().resource::<Menu>().flags[Flag::Cockpit as usize]
    }

    fn seat(app: &App) -> bool {
        app.world().resource::<ViewMode>().first_person
    }

    /// `COCKPIT VIEW` and the seat are one setting, and each direction was its
    /// own defect: the row used to move a bit that nothing read, so switching
    /// it on did nothing at all; and `V` used to move a view mode the row had
    /// never heard of, so the display then lied about where the player was
    /// sitting.
    #[test]
    fn the_cockpit_row_and_the_view_mode_track_each_other() {
        let mut app = mirror_app(false);
        app.update();
        assert!(!row(&app) && !seat(&app), "third person to begin with");

        // The display's switch seats the pilot.
        app.world_mut().resource_mut::<Menu>().flags[Flag::Cockpit as usize] = true;
        app.update();
        assert!(seat(&app), "the row never reached the seat");

        // ...and switching it back stands them up again.
        app.world_mut().resource_mut::<Menu>().flags[Flag::Cockpit as usize] = false;
        app.update();
        assert!(!seat(&app));

        // `V` in flight is what the row reads when it is next opened.
        app.world_mut().resource_mut::<ViewMode>().first_person = true;
        app.update();
        assert!(row(&app), "`V` never reached the row");
    }

    /// `SPACESHIPS_COCKPIT` seeds [`ViewMode`], so the first frame has to adopt
    /// it rather than stamp [`Flag::DEFAULTS`] over the top — which is what a
    /// mirror without a seeding case would do, silently disabling the one hook
    /// a visual check has for capturing this view.
    #[test]
    fn the_env_seed_survives_the_first_frame() {
        let mut app = mirror_app(true);
        app.update();
        assert!(seat(&app), "the seed was overwritten by the row's default");
        assert!(row(&app), "the row did not adopt the seed");
    }

    /// The property the whole module rests on: a page nobody is touching
    /// produces a model that compares equal, so `drive_menu` writes nothing.
    ///
    /// The sweep is deliberately excluded by advancing time only within one
    /// quantisation step — a scope arm genuinely does move, and the claim is
    /// that it moves in [`SWEEP_STEPS`] discrete jumps rather than every frame.
    #[test]
    fn a_still_page_is_still_within_a_sweep_step() {
        let (menu, data, net) = quiet();
        let step = std::f32::consts::TAU / SWEEP_RATE / f32::from(SWEEP_STEPS);
        assert_eq!(
            model(&menu, &data, &net, 0.0),
            model(&menu, &data, &net, step * 0.4),
        );
    }

    /// ...and a closed display is constant for all time, which is what makes
    /// this module free while the player is flying.
    #[test]
    fn a_closed_display_never_moves() {
        let (mut menu, data, net) = quiet();
        menu.open = false;
        let a = model(&menu, &data, &net, 0.0);
        for t in [0.001_f32, 0.5, 3.0, 60.0, 3600.0] {
            assert_eq!(a, model(&menu, &data, &net, t), "closed at t={t}");
            assert_eq!(a, MenuModel::default());
        }
    }

    /// The sweep does advance, and it wraps rather than saturating.
    #[test]
    fn the_sweep_advances_and_wraps() {
        let (menu, data, net) = quiet();
        let turn = std::f32::consts::TAU / SWEEP_RATE;
        assert_eq!(model(&menu, &data, &net, 0.0).sweep, 0);
        assert_eq!(model(&menu, &data, &net, turn / 4.0).sweep, SWEEP_STEPS / 4);
        assert_eq!(model(&menu, &data, &net, turn).sweep, 0);
        assert!(model(&menu, &data, &net, turn * 3.5).sweep < SWEEP_STEPS);
    }

    /// Moving the cursor moves the cursor field and nothing else — the
    /// guarantee that makes the control pass cheap.
    #[test]
    fn moving_the_cursor_moves_one_field() {
        let (menu, data, net) = quiet();
        let before = model(&menu, &data, &net, 0.0);
        let mut moved = Menu {
            open: true,
            screen: Screen::Main,
            pinned: true,
            ..Menu::default()
        };
        moved.focus[Screen::Main.index()] = 3;
        let after = model(&moved, &data, &net, 0.0);
        assert_ne!(before.focus, after.focus);
        assert_eq!(
            MenuModel {
                focus: before.focus,
                ..after
            },
            before,
            "the cursor must not disturb any other field"
        );
    }

    /// The self test fills in over its own duration and then stops.
    #[test]
    fn the_self_test_fills_and_stops() {
        let (mut menu, data, net) = quiet();
        menu.screen = Screen::Boot;
        assert_eq!(model(&menu, &data, &net, 0.0).boot, 0);
        assert_eq!(model(&menu, &data, &net, BOOT_STEP * 4.5).boot, 4);
        let done = BOOT_LINES as f32 * BOOT_STEP;
        assert_eq!(model(&menu, &data, &net, done).boot, BOOT_LINES as u8);
        assert_eq!(model(&menu, &data, &net, done * 4.0).boot, BOOT_LINES as u8);
    }

    /// The self test owns the whole display: no rails, and no sweep to drive.
    #[test]
    fn the_self_test_hides_the_rails_and_stills_the_scope() {
        let (mut menu, data, net) = quiet();
        menu.screen = Screen::Boot;
        for t in [0.0_f32, 0.3, 1.1] {
            let m = model(&menu, &data, &net, t);
            assert!(!m.chrome);
            assert_eq!(m.sweep, 0, "the scope is off screen during a self test");
        }
    }

    /// Every page is reachable and every page but the board has a way back, so
    /// no page can strand the player.
    #[test]
    fn every_page_has_a_way_out() {
        for s in Screen::ALL {
            if matches!(s, Screen::Boot | Screen::Main) {
                assert!(s.back().is_none());
            } else {
                assert!(s.back().is_some(), "{s:?} has no page below it");
            }
            assert!(!s.title().is_empty());
        }
        for name in [
            "boot",
            "main",
            "solo",
            "trials",
            "campaign",
            "net",
            "create",
            "browser",
            "waiting",
            "armory",
            "record",
            "standings",
            "config",
        ] {
            assert!(Screen::parse(name).is_some(), "{name} does not parse");
        }
    }

    /// `Screen::index` must agree with the discriminant, because the model
    /// stores the page as a `u8` and looks it back up in `Screen::ALL`.
    #[test]
    fn the_page_index_round_trips() {
        for (i, s) in Screen::ALL.into_iter().enumerate() {
            assert_eq!(s.index(), i);
            assert_eq!(Screen::ALL[s as usize], s);
        }
    }

    /// The ladder's shape, which is the point of the armory redesign.
    ///
    /// Not the prices — those are a balance decision and are meant to move.
    /// What must hold is that there is no cliff: every step up is under 6x the
    /// one below it, where the shop being replaced had a single 250x jump from
    /// 500 to 125,000.
    #[test]
    fn the_ladder_has_no_cliff() {
        let mut costs: Vec<u32> = ARMORY.iter().map(|r| r.cost).filter(|c| *c > 0).collect();
        costs.sort_unstable();
        for pair in costs.windows(2) {
            let (lo, hi) = (pair[0], pair[1]);
            assert!(
                hi <= lo * 6,
                "a {lo} -> {hi} step is a wall, not a rung ({}x)",
                hi / lo
            );
        }
        assert_eq!(
            *costs.last().unwrap(),
            125_000,
            "the admin ship must not move"
        );
    }

    /// Every tier is populated, and every tier but the livery one is anchored
    /// by an airframe — so a locked tier always shows something worth climbing
    /// to.
    #[test]
    fn every_tier_is_populated() {
        for t in 0..TIERS.len() {
            #[allow(clippy::cast_possible_truncation)]
            let tier = t as u8;
            let rungs: Vec<&Rung> = ARMORY.iter().filter(|r| r.tier == tier).collect();
            assert!(!rungs.is_empty(), "tier {t} is empty");
            if tier != 1 {
                assert!(
                    rungs.iter().any(|r| r.class == "SHIP"),
                    "tier {t} has no airframe to anchor it"
                );
            }
        }
        assert!(ARMORY.len() <= 32, "the ownership bitset is a u32");
    }

    /// A tier opens only when the one below it is cleared, and tier 0 is always
    /// open. This is what keeps locked rungs visible-but-unbuyable rather than
    /// hidden.
    #[test]
    fn tiers_open_in_order() {
        assert!(tier_open(0, 0));
        assert!(!tier_open(1, 0));
        assert!(tier_open(1, 0b1));
        assert!(!tier_open(2, 0b0000_1111));
        assert!(tier_open(2, 0b0011_1111));
    }

    /// The placeholder balance is mid-ladder, so one screenshot shows held,
    /// affordable, unaffordable and locked rungs at once.
    #[test]
    fn the_placeholder_shows_all_four_stock_states() {
        let data = LobbyData::placeholder();
        let seen: Vec<Stock> = (0..ARMORY.len()).map(|i| stock(i, &data)).collect();
        for want in [Stock::Held, Stock::Available, Stock::Short, Stock::Locked] {
            assert!(seen.contains(&want), "no rung is {want:?}");
        }
    }

    /// Buying a rung spends the balance and never goes negative.
    #[test]
    fn requisition_spends_the_balance() {
        let mut data = LobbyData::placeholder();
        let before = data.pilot.credits;
        let i = (0..ARMORY.len())
            .find(|&i| stock(i, &data) == Stock::Available)
            .expect("the placeholder must have something to buy");
        data.pilot.credits -= ARMORY[i].cost;
        data.pilot.owned |= 1 << i;
        assert_eq!(data.pilot.credits, before - ARMORY[i].cost);
        assert_eq!(stock(i, &data), Stock::Held);
    }

    /// The lobby produces the same five fields `sim_bridge` reads from the
    /// environment, and the mode follows the page rather than a stale
    /// selection.
    #[test]
    fn the_selection_becomes_a_match_setup() {
        let data = LobbyData::placeholder();
        let mut menu = Menu {
            solo: SoloPick::Train,
            map: MapKind::Terrain,
            hard: true,
            screen: Screen::Solo,
            ..Menu::default()
        };

        let s = menu.setup(&data);
        assert_eq!(s.mode, Mode::Training);
        assert_eq!(s.map, MapKind::Terrain);
        assert!(s.hard_mode);
        assert_eq!(s.callsign, "PILOT");
        assert!(s.mode.is_solo());

        menu.screen = Screen::Trials;
        menu.trial = 2;
        assert_eq!(menu.setup(&data).mode, Mode::Trials(3));

        menu.screen = Screen::Campaign;
        menu.mission = 0;
        assert_eq!(menu.setup(&data).mode, Mode::Campaign(1));

        // A network page produces the one mode `new_match` cannot build, which
        // is why `LaunchRequest` carries `online`.
        menu.screen = Screen::Waiting;
        let s = menu.setup(&data);
        assert_eq!(s.mode, Mode::Multiplayer);
        assert!(!s.mode.is_solo());
    }

    /// `SPACESHIPS_UI` names the pages a capture script uses; a typo must still
    /// start the game.
    #[test]
    fn an_unknown_page_name_is_not_fatal() {
        assert!(Screen::parse("nonsense").is_none());
        assert_eq!(Screen::parse("SHOP"), Some(Screen::Armory));
        assert_eq!(Screen::parse("leaderboard"), Some(Screen::Standings));
    }

    /// Grouping is done by hand because `format!` has no flag for it.
    #[test]
    fn thousands_groups_from_the_right() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(12_480), "12,480");
        assert_eq!(thousands(125_000), "125,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    /// A trial best reads as a lap time, and an unflown circuit reads as blank
    /// rather than as zero.
    #[test]
    fn trial_times_read_as_lap_times() {
        assert_eq!(fmt_trial(Some(92.4)), "1:32.40");
        assert_eq!(fmt_trial(Some(118.7)), "1:58.70");
        assert_eq!(fmt_trial(Some(9.05)), "0:09.05");
        assert_eq!(fmt_trial(None), "-:--.--");
    }

    /// The selection key has to change for *every* selection, or a page will
    /// fail to repaint when one of them moves.
    #[test]
    fn the_selection_key_notices_every_selection() {
        let data = LobbyData::placeholder();
        let base = Menu::default();
        let k = selection_key(&base, &data);

        let cases: Vec<(&str, Menu)> = vec![
            (
                "solo",
                Menu {
                    solo: SoloPick::Tutorial,
                    ..Menu::default()
                },
            ),
            (
                "trial",
                Menu {
                    trial: 3,
                    ..Menu::default()
                },
            ),
            (
                "mission",
                Menu {
                    mission: 2,
                    ..Menu::default()
                },
            ),
            (
                "map",
                Menu {
                    map: MapKind::Terrain,
                    ..Menu::default()
                },
            ),
            (
                "hard",
                Menu {
                    hard: true,
                    ..Menu::default()
                },
            ),
            (
                "private",
                Menu {
                    private: true,
                    ..Menu::default()
                },
            ),
            (
                "auto bot",
                Menu {
                    auto_bot: false,
                    ..Menu::default()
                },
            ),
            (
                "sortie",
                Menu {
                    sortie: 4,
                    ..Menu::default()
                },
            ),
            (
                "item",
                Menu {
                    item: 9,
                    ..Menu::default()
                },
            ),
            (
                "scheme",
                Menu {
                    scheme: 2,
                    ..Menu::default()
                },
            ),
            (
                "music",
                Menu {
                    volume: [1, 8],
                    ..Menu::default()
                },
            ),
            (
                "effects",
                Menu {
                    volume: [6, 1],
                    ..Menu::default()
                },
            ),
            (
                "hull",
                Menu {
                    hull: 4,
                    ..Menu::default()
                },
            ),
            (
                "accent",
                Menu {
                    accent: 2,
                    ..Menu::default()
                },
            ),
            (
                "trail",
                Menu {
                    trail: 3,
                    ..Menu::default()
                },
            ),
        ];
        for (what, m) in cases {
            assert_ne!(selection_key(&m, &data), k, "{what}");
        }
        for i in 0..Flag::ALL.len() {
            let mut flags = Flag::DEFAULTS;
            flags[i] = !flags[i];
            let m = Menu {
                flags,
                ..Menu::default()
            };
            assert_ne!(selection_key(&m, &data), k, "flag {i}");
        }
        // A rung the placeholder does *not* hold, or the bit is already set
        // and the key legitimately does not move.
        let mut bought = data.clone();
        assert_eq!(stock(8, &data), Stock::Short, "rung 8 must start unheld");
        bought.pilot.owned |= 1 << 8;
        assert_ne!(selection_key(&base, &bought), k, "ownership");
    }

    /// The packed key must fit, and the constant must agree with the pushes.
    /// `selection_key`'s own `debug_assert`s do the second half; this pins the
    /// first, which is the one that failed silently.
    #[test]
    fn the_selection_key_fits_in_a_u64() {
        const { assert!(SELECTION_BITS <= 64, "the selection key will not fit") };
        // Exercising it also runs the debug asserts inside.
        let _ = selection_key(&Menu::default(), &LobbyData::placeholder());
    }

    /// The caution phase does not tick while nothing is cautioning — the same
    /// property `hud.rs` asserts of its blink, and for the same reason: an
    /// unconditional phase would make the model differ every frame.
    #[test]
    fn the_caution_lamp_is_still_when_nothing_is_wrong() {
        let (menu, mut data, mut net) = quiet();
        data.source = DataSource::Live;
        net.state = ConnState::Online;
        for t in [0.0_f32, 0.2, 0.6, 1.4, 9.0] {
            assert!(!model(&menu, &data, &net, t).caution, "quiet at t={t}");
        }
    }

    /// ...and it does blink once something is.
    #[test]
    fn the_caution_lamp_blinks_on_placeholder_data() {
        let (menu, data, net) = quiet();
        assert_eq!(data.source, DataSource::Placeholder);
        assert!(model(&menu, &data, &net, 0.0).caution);
        assert!(!model(&menu, &data, &net, CAUTION_HALF_PERIOD).caution);
        assert!(model(&menu, &data, &net, CAUTION_HALF_PERIOD * 2.0).caution);
    }

    /// The `FEED` lamp tells the truth about where the numbers came from, so a
    /// screenshot cannot quietly present placeholders as the server's data.
    #[test]
    fn the_feed_lamp_admits_to_placeholder_data() {
        let (menu, mut data, net) = quiet();
        let m = model(&menu, &data, &net, 0.0);
        assert_eq!(lamp_states(&menu, &data, m)[1].1, "CACHED");
        data.source = DataSource::Live;
        assert_eq!(lamp_states(&menu, &data, m)[1].1, "LIVE");
    }

    /// The palette is `cockpit.rs`'s, not a second interpretation of it. These
    /// are the exact hex values in that module's `Palette::new` and `Swatch`;
    /// if one is edited there, this fails and someone has to decide whether the
    /// menu follows.
    #[test]
    fn the_palette_is_the_instrument_panels() {
        assert_eq!(pal::SCREEN, pal::rgb(0x05_08_0b), "Palette::screen");
        assert_eq!(pal::WELL, pal::rgb(0x0d_15_1c), "Palette::well");
        assert_eq!(pal::GRID, 0x1d_3a_4a, "Palette::grid");
        assert_eq!(pal::CAPTION, 0x7f_a6_c0, "Palette::caption");
        assert_eq!(pal::WHITE, pal::rgb(0xea_f6_ff), "Palette::white");
        assert_eq!(pal::PHOSPHOR, pal::rgb(0x46_ff_9b), "Swatch::blip_friendly");
        assert_eq!(pal::PHOSPHOR_LAMP, pal::rgb(0x38_ff_9b), "Swatch::tgt_on");
        assert_eq!(pal::AMBER, pal::rgb(0xff_c4_51), "ADMIN_PROFILE::accent");
        assert_eq!(pal::CYAN, pal::rgb(0x5f_d8_ff), "DEFAULT_PROFILE::accent");
        assert_eq!(pal::RED, pal::rgb(0xff_4d_4d), "Swatch::red");
        assert_eq!(pal::ORANGE, pal::rgb(0xff_9d_3d), "Swatch::orange");
        assert_eq!(pal::DEAD, pal::rgb(0x16_22_2c), "Swatch::dead");
    }

    /// The sweep is shared with `cockpit.rs`'s scope and turns at the same
    /// rate, so the menu's radar and the panel's radar are one instrument.
    #[test]
    fn the_sweep_rate_matches_the_panel() {
        // `cockpit.rs::SWEEP_RATE`, `dash.js:230`.
        assert!((SWEEP_RATE - 2.2).abs() < f32::EPSILON);
    }

    /// The tube is a post-process, and `SPACESHIPS_UI_CRT=0` must flatten it
    /// completely rather than merely turn one term down — otherwise the escape
    /// hatch cannot answer "is this the layout or the shader".
    #[test]
    fn the_tube_can_be_switched_off() {
        let size = Vec2::new(1280.0, 720.0);
        let on = Crt::new(Handle::default(), size, true);
        let off = Crt::new(Handle::default(), size, false);
        assert!(on.geometry.z > 0.0 && on.geometry.w > 0.0);
        assert_eq!(off.geometry.z, 0.0, "curvature");
        assert_eq!(off.geometry.w, 0.0, "scanlines");
        assert_eq!(off.optics, Vec4::ZERO, "fringing, vignette, bleed, flicker");
        // ...but the housing is geometry, not an effect, and stays.
        assert_eq!(off.housing.x, INSET);
        assert_eq!(off.housing.y, BEZEL);
        assert_eq!(off.housing.w, 0.0, "no spill without phosphor");
        // The size is carried either way: the shader divides by it.
        assert_eq!(on.geometry.xy(), size);
        assert_eq!(off.geometry.xy(), size);
    }

    /// A zero-sized window must not produce a divide by zero in the shader.
    #[test]
    fn the_tube_never_reports_a_zero_size() {
        let c = Crt::new(Handle::default(), Vec2::ZERO, true);
        assert!(c.geometry.x >= 1.0 && c.geometry.y >= 1.0);
    }

    // -----------------------------------------------------------------------
    // The pointer
    // -----------------------------------------------------------------------
    //
    // A screenshot cannot prove a hit test: the highlight it shows is the one
    // the *keyboard* left. So the mapping is proved here, at known cursor
    // positions, against rows placed where `build_pages` puts them.

    /// A 1280x720 window with the tube filling it, and the off-screen target at
    /// the same size — which is what it is, since `fit_tube` matches the two.
    const TUBE: Rect = Rect {
        min: Vec2::ZERO,
        max: Vec2::new(1280.0, 720.0),
    };
    const IMAGE: Vec2 = Vec2::new(1280.0, 720.0);

    /// `through_the_glass`, backwards, written from the equation rather than
    /// from the code it checks.
    ///
    /// `warp` is `c -> c * (1 + k|c|²)`, so `c = w / (1 + k|c|²)` is a
    /// contraction for the small `k` a tube uses and converges in a handful of
    /// steps. Used to ask the honest question: *where does the mouse have to
    /// be* for a given part of the page to be under it.
    fn cursor_for(menu_point: Vec2, k: f32, inset: f32) -> Vec2 {
        let w = (menu_point / IMAGE) * 2.0 - Vec2::ONE;
        let mut c = w;
        for _ in 0..64 {
            c = w / (1.0 + k * c.dot(c));
        }
        let uv = ((c * (1.0 + k) / inset) + Vec2::ONE) * 0.5;
        TUBE.min + uv * TUBE.size()
    }

    /// Three rows down the left column, at the pitch `control_row` gives an
    /// 18px row: `min_height` is `size * 1.9`, so 34 pixels tall.
    fn solo_rows() -> Vec<(u16, Rect)> {
        (0..3u16)
            .map(|i| {
                (
                    i,
                    Rect::from_center_size(
                        Vec2::new(300.0, 300.0 + f32::from(i) * 34.0),
                        Vec2::new(430.0, 34.0),
                    ),
                )
            })
            .collect()
    }

    /// The centre of the tube is the centre of the page. `warp(0) == 0`, so
    /// this is the one point the curve cannot move — and if it ever does, the
    /// inset or the pre-divide has been mis-transcribed.
    #[test]
    fn the_middle_of_the_glass_is_the_middle_of_the_page() {
        let hit = through_the_glass(TUBE.size() * 0.5, TUBE, IMAGE, CURVATURE, INSET)
            .expect("the centre of the tube is on the tube");
        assert!(
            (hit - IMAGE * 0.5).length() < 0.01,
            "the centre mapped to {hit}, not {}",
            IMAGE * 0.5
        );
    }

    /// Every row is under the cursor that the mapping says it is under.
    ///
    /// The positions come from the inverse above, so this is a round trip
    /// through the real constants: a page pixel, out to where the mouse must
    /// be on the physical screen, and back through the shader's projection to
    /// the row that owns it.
    #[test]
    fn the_pointer_lands_on_the_row_it_is_over() {
        let rows = solo_rows();
        for (i, rect) in &rows {
            let cursor = cursor_for(rect.center(), CURVATURE, INSET);
            let hit = through_the_glass(cursor, TUBE, IMAGE, CURVATURE, INSET)
                .expect("a row in the middle of the page is on the glass");
            assert_eq!(
                control_at(hit, &rows),
                Some((u8::try_from(*i).expect("three rows"), *i)),
                "the cursor at {cursor} did not land on row {i}",
            );
        }
        // ...and the gaps between the columns belong to nobody, rather than to
        // whichever row happens to share a y with them.
        let outside = cursor_for(Vec2::new(900.0, 300.0), CURVATURE, INSET);
        let hit = through_the_glass(outside, TUBE, IMAGE, CURVATURE, INSET).expect("on the glass");
        assert_eq!(control_at(hit, &rows), None);
    }

    /// Near a corner the curve moves the answer by more than a row is tall, so
    /// a linear map is not merely imprecise there — it is wrong.
    ///
    /// Two rows: one where the warp says the page is, one where a naive
    /// `uv * size` would say it is. The pointer has to choose the first.
    #[test]
    fn the_warp_is_what_decides_it_near_a_corner() {
        // Up in the top-right quarter, well out along both axes.
        let uv = Vec2::new(0.90, 0.10);
        let cursor = TUBE.min + uv * TUBE.size();
        let warped = through_the_glass(cursor, TUBE, IMAGE, CURVATURE, INSET)
            .expect("nine tenths of the way out is still on the glass");
        let naive = uv * IMAGE;

        let apart = (warped - naive).length();
        assert!(
            apart > 34.0,
            "the corner displaces by {apart}px, which a 34px row would swallow \
             — pick a point further out"
        );

        let rows = vec![
            (7u16, Rect::from_center_size(warped, Vec2::new(200.0, 34.0))),
            (8u16, Rect::from_center_size(naive, Vec2::new(200.0, 34.0))),
        ];
        assert_eq!(control_at(warped, &rows), Some((0, 7)));
    }

    /// Off the glass is not "the nearest row": it is nothing.
    ///
    /// `INSET` pulls the face in and the curve rounds the corners off, so the
    /// last few percent of the window is moulding and room. A cursor there has
    /// nothing under it, and clamping — which is right for a fragment, since it
    /// still has to be shaded — would silently arm the row nearest the edge.
    #[test]
    fn a_cursor_off_the_glass_hits_nothing() {
        let w = TUBE.size();
        for uv in [
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(0.0, 1.0),
            Vec2::new(1.0, 1.0),
            // The edge midpoints: `1 + k` puts these *just* off, which is the
            // pre-divide doing its job — the bowed sides reach the bezel.
            Vec2::new(0.5, 0.0),
            Vec2::new(1.0, 0.5),
            // And the rounded corner, which cuts in further than the sides do.
            Vec2::new(0.965, 0.035),
        ] {
            assert_eq!(
                through_the_glass(TUBE.min + uv * w, TUBE, IMAGE, CURVATURE, INSET),
                None,
                "uv {uv} should be off the glass",
            );
        }
        // ...but the same distance out along a side still is on it, so the
        // rejection is the tube's shape and not a blanket margin.
        assert!(
            through_the_glass(
                TUBE.min + Vec2::new(0.965, 0.5) * w,
                TUBE,
                IMAGE,
                CURVATURE,
                INSET
            )
            .is_some(),
            "the middle of the right-hand edge is glass, not moulding",
        );
        // A cursor outside the node entirely — the window is bigger than the
        // tube on a frame `fit_tube` has not caught up with — is off it too.
        assert_eq!(
            through_the_glass(Vec2::new(-40.0, 300.0), TUBE, IMAGE, CURVATURE, INSET),
            None
        );
    }

    /// `SPACESHIPS_UI_CRT=0` draws a flat blit, and the pointer has to go flat
    /// with it. This is why `Crt::glass` reads the uniforms back rather than
    /// naming the constants.
    #[test]
    fn a_flat_tube_maps_flat() {
        let (k, inset) = Crt::new(Handle::default(), IMAGE, false).glass();
        assert_eq!(k, 0.0);
        let uv = Vec2::new(0.25, 0.75);
        let hit = through_the_glass(TUBE.min + uv * TUBE.size(), TUBE, IMAGE, k, inset)
            .expect("a quarter of the way in is on the glass");
        // Still not `uv * IMAGE`: the face inset survives, because the housing
        // is geometry rather than an effect.
        let expected = ((uv * 2.0 - Vec2::ONE) * inset * 0.5 + Vec2::splat(0.5)) * IMAGE;
        assert!((hit - expected).length() < 0.01, "{hit} vs {expected}");
        assert_eq!(
            Crt::new(Handle::default(), IMAGE, true).glass().0,
            CURVATURE
        );
    }

    /// A tube that has not been laid out yet must not divide by zero or pick a
    /// row at random.
    #[test]
    fn a_tube_with_no_extent_resolves_nothing() {
        let empty = Rect::from_corners(Vec2::ZERO, Vec2::ZERO);
        assert_eq!(
            through_the_glass(Vec2::ZERO, empty, IMAGE, CURVATURE, INSET),
            None
        );
        assert_eq!(
            through_the_glass(Vec2::new(10.0, 10.0), TUBE, Vec2::ZERO, CURVATURE, INSET),
            None
        );
    }

    // -----------------------------------------------------------------------
    // The launch flow
    // -----------------------------------------------------------------------

    /// Runs one activation against a real `MessageWriter`, and reports what
    /// `sim_bridge.rs` would have been handed.
    fn activate(action: Action, menu: &mut Menu) -> (MatchSetup, Vec<LaunchRequest>) {
        activate_online(action, menu, &NetSession::default()).0
    }

    /// The same, plus the frames and commands the socket would have been
    /// handed. `apply` collects those into [`NetOps`] rather than writing them,
    /// precisely so a test can read them without a running app.
    /// Everything one activation produced.
    ///
    /// [`activate`] and [`activate_online`] are narrower views onto this, kept
    /// because the tests that predate the REST half read better without four
    /// ignored bindings apiece.
    struct Fired {
        launched: Vec<LaunchRequest>,
        sent: Vec<ClientMessage>,
        commands: Vec<NetCommand>,
        api: Vec<ApiRequest>,
        setup: MatchSetup,
        data: LobbyData,
    }

    fn fire_action(
        action: Action,
        menu: &mut Menu,
        session: &NetSession,
        mut data: LobbyData,
    ) -> Fired {
        use bevy::ecs::message::Messages;
        use bevy::ecs::system::SystemState;

        let mut world = World::new();
        world.init_resource::<Messages<LaunchRequest>>();
        let mut state: SystemState<MessageWriter<LaunchRequest>> = SystemState::new(&mut world);
        let mut setup = MatchSetup::default();
        let mut config = NetConfig::default();
        let callsign = data.pilot.callsign.clone();
        let mut ops = NetOps::new(session, &mut config, &callsign);
        {
            let mut launch = state.get_mut(&mut world).expect("the writer validates");
            apply(action, menu, &mut data, &mut setup, &mut launch, &mut ops);
        }
        state.apply(&mut world);
        Fired {
            launched: world
                .resource_mut::<Messages<LaunchRequest>>()
                .drain()
                .collect(),
            sent: ops.send,
            commands: ops.commands,
            api: ops.api,
            setup,
            data,
        }
    }

    fn activate_online(
        action: Action,
        menu: &mut Menu,
        session: &NetSession,
    ) -> (
        (MatchSetup, Vec<LaunchRequest>),
        Vec<ClientMessage>,
        Vec<NetCommand>,
    ) {
        let out = fire_action(action, menu, session, LobbyData::placeholder());
        ((out.setup, out.launched), out.sent, out.commands)
    }

    /// The complaint, as a test: choosing a mode has to *be* the launch.
    ///
    /// Before this, activating a row armed a preference and left the pilot to
    /// find `EXECUTE` on another page — with the match already running.
    #[test]
    fn choosing_a_mode_flies_it() {
        for (action, screen, mode) in [
            (
                Action::SetSolo(SoloPick::Skirmish),
                Screen::Solo,
                Mode::Skirmish,
            ),
            (
                Action::SetSolo(SoloPick::Tutorial),
                Screen::Solo,
                Mode::Tutorial,
            ),
            (Action::SetTrial(1), Screen::Trials, Mode::Trials(2)),
            (Action::SetMission(0), Screen::Campaign, Mode::Campaign(1)),
        ] {
            let mut menu = Menu {
                open: true,
                screen,
                ..Menu::default()
            };
            let (setup, sent) = activate(action, &mut menu);
            assert!(!menu.open, "{action:?} left the display up");
            assert_eq!(sent.len(), 1, "{action:?} did not launch");
            assert_eq!(setup.mode, mode, "{action:?}");
            assert_eq!(sent[0].setup.mode, mode);
            assert!(sent[0].setup.mode.is_solo() && !sent[0].online);
        }
    }

    /// ...and the mission board's `EXECUTE` still launches what is armed, which
    /// is the one page with no row that names a match.
    #[test]
    fn the_board_still_executes_what_is_armed() {
        let mut menu = Menu {
            open: true,
            screen: Screen::Main,
            solo: SoloPick::Train,
            ..Menu::default()
        };
        let (setup, sent) = activate(Action::Execute, &mut menu);
        assert!(!menu.open);
        assert_eq!(setup.mode, Mode::Training);
        assert_eq!(sent.len(), 1);
    }

    /// A row painted as locked must not fly when it is pressed.
    ///
    /// It never could before either — but it armed itself and `EXECUTE` then
    /// flew it, which was a slower way to reach the same wrong place. Now that
    /// the row *is* the launch, the paint and the behaviour have to agree.
    #[test]
    fn a_locked_row_refuses_rather_than_launches() {
        for (action, screen) in [
            (Action::SetTrial(2), Screen::Trials),
            (Action::SetTrial(3), Screen::Trials),
            (Action::SetMission(2), Screen::Campaign),
        ] {
            let mut menu = Menu {
                open: true,
                screen,
                ..Menu::default()
            };
            let (_, sent) = activate(action, &mut menu);
            assert!(menu.open, "{action:?} left the display");
            assert!(sent.is_empty(), "{action:?} launched a locked tasking");
            assert!(
                menu.notice.contains("FINISH THE PREVIOUS"),
                "{}",
                menu.notice
            );
        }
        // The rows that *are* open still are, so the gate is the ladder rather
        // than a blanket refusal.
        for t in 0..2u8 {
            assert!(!trial_locked(t));
        }
        assert!(!mission_locked(0) && !mission_locked(1));
    }

    // -----------------------------------------------------------------------
    // The network flow
    // -----------------------------------------------------------------------

    fn room(code: &str, host: &str, players: u32) -> spaceships_protocol::RoomSummary {
        spaceships_protocol::RoomSummary {
            code: code.to_owned(),
            player_count: players,
            host_name: host.to_owned(),
        }
    }

    fn in_room(host: bool) -> NetSession {
        NetSession {
            phase: Phase::Room,
            code: "ABCD".to_owned(),
            host,
            you: Some(1),
            ..NetSession::default()
        }
    }

    /// Nothing goes out before the handshake finishes.
    ///
    /// `flush_outbox` drops a frame written while the socket is opening —
    /// exactly as the JS `readyState` guard does — so a `create` written
    /// straight from the button press is a button that does nothing on the
    /// first press and works on the second. This is why `Menu::pending` exists.
    #[test]
    fn a_lobby_request_waits_for_the_socket() {
        for (action, want) in [
            (Action::Transmit, Pending::Create),
            (Action::Refresh, Pending::List),
        ] {
            let mut menu = Menu {
                open: true,
                screen: Screen::Create,
                ..Menu::default()
            };
            let (_, sent, cmds) = activate_online(action, &mut menu, &NetSession::default());
            assert!(sent.is_empty(), "{action:?} wrote a frame too early");
            assert_eq!(
                cmds,
                vec![NetCommand::Connect],
                "{action:?} asked for no link"
            );
            assert_eq!(menu.pending, Some(want));
        }
    }

    /// ...and `JOIN` carries the code of the row under the cursor, not the
    /// index — the browser is a fixed pool and its rows move under it.
    #[test]
    fn joining_takes_the_selected_rooms_code() {
        let session = NetSession {
            rooms: vec![room("KILO", "HALCYON", 4), room("ZULU", "SABLE", 2)],
            ..NetSession::default()
        };
        let mut menu = Menu {
            open: true,
            screen: Screen::Browser,
            sortie: 1,
            ..Menu::default()
        };
        let (_, sent, _) = activate_online(Action::JoinSortie, &mut menu, &session);
        assert!(sent.is_empty(), "join must wait for the link");
        assert_eq!(menu.pending, Some(Pending::Join("ZULU".to_owned())));

        // An empty slot refuses instead of joining whatever is at that index.
        let mut menu = Menu {
            open: true,
            screen: Screen::Browser,
            sortie: 4,
            ..Menu::default()
        };
        let (_, sent, _) = activate_online(Action::JoinSortie, &mut menu, &session);
        assert!(sent.is_empty());
        assert_eq!(menu.pending, None);
        assert_eq!(menu.notice, "GAME NOT FOUND");
    }

    /// The browser page's controls, in the order the cursor visits them: six
    /// sortie rows, `REFRESH`, `JOIN`.
    fn browser_controls() -> Vec<ControlDef> {
        let def = |action| ControlDef {
            screen: Screen::Browser,
            action,
            root: Entity::PLACEHOLDER,
            tick: Entity::PLACEHOLDER,
            label: Entity::PLACEHOLDER,
            value: Entity::PLACEHOLDER,
        };
        #[allow(clippy::cast_possible_truncation)]
        let mut out: Vec<ControlDef> = (0..SORTIE_ROWS as u8)
            .map(|i| def(Action::SetSortie(i)))
            .collect();
        out.push(def(Action::Refresh));
        out.push(def(Action::JoinSortie));
        out
    }

    /// **The regression.** Arrowing down to `JOIN` must not re-arm the
    /// selection to the last empty row it passed.
    ///
    /// Reported as *"I can see my friend's lobby but I can't physically join
    /// it"*: one room on row 0, `JOIN` seven rows below it, and every empty row
    /// between them arming itself as the cursor went past. `JOIN` then looked
    /// up row 5 of a one-room list and refused. See [`arms_on_focus`].
    ///
    /// This walks the cursor exactly as `read_input` does — the same
    /// `arms_on_focus` gate, the same `preview` — because the seam that failed
    /// is the arming rule, not the join itself.
    #[test]
    fn arrowing_down_to_join_keeps_the_room_that_is_there() {
        let session = NetSession {
            rooms: vec![room("KILO", "HALCYON", 4)],
            ..NetSession::default()
        };
        let data = LobbyData::placeholder();
        let controls = browser_controls();
        let mut menu = Menu {
            open: true,
            screen: Screen::Browser,
            ..Menu::default()
        };
        menu.watch(NetView::of(&session));

        // Step onto every row in turn, all the way to `JOIN`.
        for (slot, def) in controls.iter().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            {
                menu.focus[Screen::Browser.index()] = slot as u8;
            }
            if arms_on_focus(def, &menu, &data) {
                preview(def.action, &mut menu);
            }
        }

        assert_eq!(
            menu.sortie, 0,
            "the cursor armed an empty row on its way to JOIN"
        );
        let (_, sent, _) = activate_online(Action::JoinSortie, &mut menu, &session);
        assert!(sent.is_empty(), "join waits for the link");
        assert_eq!(
            menu.pending,
            Some(Pending::Join("KILO".to_owned())),
            "JOIN did not carry the only room on screen"
        );
    }

    /// A row that is really there still arms on the way past, so the fix is a
    /// gate rather than a blanket refusal to preview.
    #[test]
    fn a_live_browser_row_still_arms_itself() {
        let session = NetSession {
            rooms: vec![room("KILO", "HALCYON", 4), room("ZULU", "SABLE", 2)],
            ..NetSession::default()
        };
        let data = LobbyData::placeholder();
        let controls = browser_controls();
        let mut menu = Menu {
            open: true,
            screen: Screen::Browser,
            ..Menu::default()
        };
        menu.watch(NetView::of(&session));

        for def in &controls {
            if arms_on_focus(def, &menu, &data) {
                preview(def.action, &mut menu);
            }
        }
        assert_eq!(menu.sortie, 1, "the last *live* row is the armed one");

        let (_, _, _) = activate_online(Action::JoinSortie, &mut menu, &session);
        assert_eq!(menu.pending, Some(Pending::Join("ZULU".to_owned())));
    }

    /// `ENTER` on an empty slot is the same mistake by another route.
    #[test]
    fn selecting_an_empty_slot_refuses_rather_than_arming_it() {
        let session = NetSession {
            rooms: vec![room("KILO", "HALCYON", 4)],
            ..NetSession::default()
        };
        let mut menu = Menu {
            open: true,
            screen: Screen::Browser,
            ..Menu::default()
        };
        let (_, _, _) = activate_online(Action::SetSortie(3), &mut menu, &session);
        assert_eq!(
            menu.sortie, 0,
            "an empty slot must not become the selection"
        );
        assert_eq!(menu.notice, "GAME NOT FOUND");

        let (_, _, _) = activate_online(Action::SetSortie(0), &mut menu, &session);
        assert_eq!(menu.sortie, 0);
        assert_eq!(menu.notice, "ORDER SELECTED");
    }

    /// The server's side of the same failure: a list that comes back shorter
    /// must not leave the selection past its end.
    #[test]
    fn a_shrinking_room_list_pulls_the_selection_back() {
        let mut menu = Menu {
            sortie: 4,
            ..Menu::default()
        };
        // Five rooms, then two: the host of the one at index 4 pressed start.
        let five = NetSession {
            rooms: (0..5).map(|i| room(&format!("R{i}"), "HOST", 1)).collect(),
            ..NetSession::default()
        };
        menu.watch(NetView::of(&five));
        assert_eq!(menu.sortie, 4, "still a real row");

        let two = NetSession {
            rooms: (0..2).map(|i| room(&format!("R{i}"), "HOST", 1)).collect(),
            rev: 1,
            ..NetSession::default()
        };
        menu.watch(NetView::of(&two));
        assert_eq!(menu.sortie, 0, "the selection was left past the end");

        // An unchanged view is not a reason to move the selection.
        menu.sortie = 1;
        menu.watch(NetView::of(&two));
        assert_eq!(menu.sortie, 1);
    }

    // -----------------------------------------------------------------------
    // The account
    // -----------------------------------------------------------------------

    fn auth_menu() -> Menu {
        Menu {
            open: true,
            screen: Screen::Auth,
            ..Menu::default()
        }
    }

    /// The only text input in the client, and the rules it enforces are the
    /// server's own (`registerPilot` in `server/db.js`).
    #[test]
    fn a_callsign_field_takes_letters_and_digits_and_nothing_else() {
        let mut form = AuthForm::default();
        assert!(form.push(Field::Callsign, "Mav3rick"));
        assert_eq!(form.callsign, "Mav3rick");
        // Everything the server would reject is dropped rather than sent and
        // refused: no spaces, no punctuation, no control characters.
        assert!(!form.push(Field::Callsign, " -_/\n\t"));
        assert_eq!(form.callsign, "Mav3rick");

        // ...and it stops at the server's limit rather than building a request
        // that cannot succeed.
        form.push(Field::Callsign, &"x".repeat(100));
        assert_eq!(form.callsign.len(), CALLSIGN_MAX);
        assert!(!form.push(Field::Callsign, "y"), "full is full");
    }

    /// A passphrase is whatever the pilot chose. The only rule is that it is
    /// printable — a control character there would be a header or a JSON
    /// escape, never something anyone typed.
    #[test]
    fn a_passphrase_field_takes_anything_printable_including_spaces() {
        let mut form = AuthForm::default();
        assert!(form.push(Field::Secret, "correct horse!"));
        assert_eq!(form.secret, "correct horse!");
        assert!(!form.push(Field::Secret, "\n\r\t\u{7}"));
        assert_eq!(form.secret, "correct horse!");
        form.push(Field::Secret, &"z".repeat(200));
        assert_eq!(form.secret.chars().count(), PASSWORD_MAX);
    }

    /// It is never drawn, and the masking is what a screenshot of a paused
    /// lobby shows instead.
    #[test]
    fn the_passphrase_is_masked_and_never_the_text() {
        let mut form = AuthForm::default();
        form.push(Field::Secret, "hunter2");
        assert_eq!(form.masked(), "*******");
        assert!(!form.masked().contains("hunter"));
    }

    #[test]
    fn backspace_takes_the_field_the_cursor_is_on() {
        let mut form = AuthForm::default();
        form.push(Field::Callsign, "ACE");
        form.push(Field::Secret, "abcdef");
        assert!(form.backspace(Field::Callsign));
        assert_eq!(form.callsign, "AC");
        assert_eq!(form.secret, "abcdef", "the other field is untouched");
        // Backspacing an empty field is not a change, so it must not mark the
        // menu dirty.
        form.callsign.clear();
        assert!(!form.backspace(Field::Callsign));
    }

    /// The server's rules, checked here so a typo costs no round trip — and
    /// worded so the pilot knows which of the two is wrong.
    #[test]
    fn the_form_refuses_credentials_the_server_would() {
        let short = AuthForm {
            callsign: "AB".to_owned(),
            secret: "longenough".to_owned(),
            rev: 0,
        };
        assert!(short.complaint().unwrap().contains("CALLSIGN"));

        let weak = AuthForm {
            callsign: "MAVERICK".to_owned(),
            secret: "12345".to_owned(),
            rev: 0,
        };
        assert!(weak.complaint().unwrap().contains("PASSWORD"));

        let good = AuthForm {
            callsign: "MAVERICK".to_owned(),
            secret: "123456".to_owned(),
            rev: 0,
        };
        assert_eq!(good.complaint(), None);
    }

    /// Signing in asks `api.rs` and hands the passphrase over — and the menu
    /// stops holding it the instant it does.
    #[test]
    fn identifying_sends_the_credentials_and_forgets_the_passphrase() {
        let mut menu = auth_menu();
        menu.form.push(Field::Callsign, "MAVERICK");
        menu.form.push(Field::Secret, "hunter22");

        let out = fire_action(
            Action::SignIn,
            &mut menu,
            &NetSession::default(),
            LobbyData::placeholder(),
        );
        assert_eq!(
            out.api,
            vec![ApiRequest::SignIn {
                username: "MAVERICK".to_owned(),
                password: "hunter22".to_owned(),
            }]
        );
        assert!(
            menu.form.secret.is_empty(),
            "the passphrase outlived the request"
        );
        assert_eq!(
            menu.form.callsign, "MAVERICK",
            "the callsign is not a secret"
        );
        assert!(out.sent.is_empty(), "this is HTTP, not the socket");
    }

    #[test]
    fn enlisting_registers_before_it_signs_in() {
        let mut menu = auth_menu();
        menu.form.push(Field::Callsign, "ROOKIE");
        menu.form.push(Field::Secret, "abcdef");
        let out = fire_action(
            Action::Enlist,
            &mut menu,
            &NetSession::default(),
            LobbyData::placeholder(),
        );
        assert_eq!(
            out.api,
            vec![ApiRequest::Enlist {
                username: "ROOKIE".to_owned(),
                password: "abcdef".to_owned(),
            }]
        );
    }

    /// A round trip is not instant, and a second `ENTER` while the first is in
    /// flight must not send the credentials twice.
    #[test]
    fn a_request_in_flight_blocks_a_second_one() {
        let mut menu = auth_menu();
        menu.form.push(Field::Callsign, "MAVERICK");
        menu.form.push(Field::Secret, "hunter22");
        menu.acct.busy = true;

        let out = fire_action(
            Action::SignIn,
            &mut menu,
            &NetSession::default(),
            LobbyData::placeholder(),
        );
        assert!(out.api.is_empty());
        assert_eq!(menu.notice, "WORKING");
        assert_eq!(menu.form.secret, "hunter22", "and nothing was consumed");

        // The row reads dead while it is busy, so the refusal is visible before
        // the key is pressed rather than after.
        let def = ControlDef {
            screen: Screen::Auth,
            action: Action::SignIn,
            root: Entity::PLACEHOLDER,
            tick: Entity::PLACEHOLDER,
            label: Entity::PLACEHOLDER,
            value: Entity::PLACEHOLDER,
        };
        let data = LobbyData::placeholder();
        assert!(control_state(&def, &menu, &data).1);
    }

    #[test]
    fn a_bad_form_never_reaches_the_network() {
        let mut menu = auth_menu();
        menu.form.push(Field::Callsign, "AB");
        menu.form.push(Field::Secret, "abcdef");
        let out = fire_action(
            Action::SignIn,
            &mut menu,
            &NetSession::default(),
            LobbyData::placeholder(),
        );
        assert!(out.api.is_empty());
        assert!(menu.notice.contains("CALLSIGN"), "{}", menu.notice);
        assert_eq!(menu.form.secret, "abcdef", "nothing was consumed");
    }

    /// Signing out is only offered when there is somebody to sign out.
    #[test]
    fn signing_out_needs_a_pilot() {
        let mut menu = auth_menu();
        let out = fire_action(
            Action::SignOut,
            &mut menu,
            &NetSession::default(),
            LobbyData::placeholder(),
        );
        assert!(out.api.is_empty());
        assert_eq!(menu.notice, "NO PILOT SIGNED IN");

        menu.acct.signed_in = true;
        let out = fire_action(
            Action::SignOut,
            &mut menu,
            &NetSession::default(),
            LobbyData::placeholder(),
        );
        assert_eq!(out.api, vec![ApiRequest::SignOut]);
    }

    /// **Requisition spends real credits, so it must not be optimistic.**
    ///
    /// It used to move `LobbyData` itself — fine while the balance was invented
    /// here, and wrong the moment it is the server's: the next refresh would
    /// have silently un-bought whatever the pilot thought they had.
    #[test]
    fn requisition_asks_the_server_and_moves_nothing_itself() {
        let mut data = LobbyData::placeholder();
        // The hull colour rung: tier 1, affordable, and one of the five the
        // database actually has a column for. Looked up rather than written as
        // a literal, so re-ordering `ARMORY` moves the test with it.
        let rung = crate::api::UNLOCKS
            .iter()
            .find(|(_, key, _)| *key == "hull")
            .expect("the hull colour is one of the five")
            .0;
        data.pilot.credits = 10_000;
        data.pilot.owned = 1;
        let before = data.pilot.credits;

        let mut menu = Menu {
            open: true,
            screen: Screen::Armory,
            #[allow(clippy::cast_possible_truncation)]
            item: rung as u8,
            ..Menu::default()
        };
        let out = fire_action(
            Action::Requisition,
            &mut menu,
            &NetSession::default(),
            data.clone(),
        );
        assert_eq!(out.api, vec![ApiRequest::Unlock("hull")]);
        assert_eq!(
            out.data.pilot.credits, before,
            "the balance moved before the server agreed"
        );
        assert_eq!(out.data.pilot.owned, 1, "and so did the ladder");
    }

    /// Eleven of the sixteen rungs are a planned economy with no column in
    /// `pilots.db`. Charging for one would be spending real credits on a bit
    /// that exists only in this process.
    #[test]
    fn a_rung_the_server_has_never_heard_of_refuses() {
        let mut data = LobbyData::placeholder();
        data.pilot.credits = 1_000_000;
        // Everything below tier 3 held, so the MK.III is genuinely available.
        data.pilot.owned = 0b1111_1111;
        let mut menu = Menu {
            open: true,
            screen: Screen::Armory,
            item: 8,
            ..Menu::default()
        };
        assert_eq!(unlock_key(8), None);
        let out = fire_action(
            Action::Requisition,
            &mut menu,
            &NetSession::default(),
            data.clone(),
        );
        assert!(out.api.is_empty());
        assert_eq!(menu.notice, "NOT IN SERVICE");
        assert_eq!(out.data.pilot.credits, 1_000_000);

        // ...and the row says so before it is pressed.
        let def = ControlDef {
            screen: Screen::Armory,
            action: Action::Requisition,
            root: Entity::PLACEHOLDER,
            tick: Entity::PLACEHOLDER,
            label: Entity::PLACEHOLDER,
            value: Entity::PLACEHOLDER,
        };
        assert!(control_state(&def, &menu, &data).1, "it should read dead");
    }

    /// Walking onto a page that shows the server's numbers asks for them —
    /// unforced, so `api.rs`'s cooldown collapses a tour of all four into one
    /// round trip.
    #[test]
    fn the_dossier_pages_ask_for_their_data() {
        for screen in [
            Screen::Record,
            Screen::Standings,
            Screen::Armory,
            Screen::Auth,
        ] {
            let session = NetSession::default();
            let mut config = NetConfig::default();
            let mut menu = Menu {
                screen,
                ..Menu::default()
            };
            let mut ops = NetOps::new(&session, &mut config, "PILOT");
            on_screen_change(Screen::Main, &mut menu, &mut ops);
            assert_eq!(
                ops.api,
                vec![ApiRequest::Refresh { force: false }],
                "{screen:?} did not ask"
            );
            assert!(
                ops.commands.is_empty(),
                "{screen:?} opened a socket it does not need"
            );
        }
    }

    /// A guest opening only the pages that need no account issues no request at
    /// all — the lobby must be silent unless it is asked for something.
    #[test]
    fn the_pages_a_guest_lives_on_ask_for_nothing() {
        for screen in [Screen::Main, Screen::Solo, Screen::Trials, Screen::Config] {
            let session = NetSession::default();
            let mut config = NetConfig::default();
            let mut menu = Menu {
                screen,
                ..Menu::default()
            };
            let mut ops = NetOps::new(&session, &mut config, "PILOT");
            on_screen_change(Screen::Main, &mut menu, &mut ops);
            assert!(ops.api.is_empty(), "{screen:?} made a request");
        }
    }

    /// The `FEED` lamp is the one thing on the display that must never flatter
    /// itself: it says where the numbers came from, in `DataSource`'s words.
    #[test]
    fn the_feed_lamp_names_the_source_it_is_showing() {
        let (menu, mut data, net) = quiet();
        let m = model(&menu, &data, &net, 0.0);
        for (source, want) in [
            (DataSource::Placeholder, "CACHED"),
            (DataSource::Public, "PUBLIC"),
            (DataSource::Live, "LIVE"),
            (DataSource::Stale, "STALE"),
        ] {
            data.source = source;
            assert_eq!(lamp_states(&menu, &data, m)[1].1, want, "{source:?}");
        }
    }

    /// ...and the caution lamp blinks for both of the two that are not the
    /// server's current answer, not just for the placeholder.
    #[test]
    fn stale_data_cautions_as_loudly_as_no_data() {
        let (menu, mut data, mut net) = quiet();
        net.state = ConnState::Online;
        for (source, want) in [
            (DataSource::Placeholder, true),
            (DataSource::Stale, true),
            (DataSource::Public, false),
            (DataSource::Live, false),
        ] {
            data.source = source;
            // Half a caution period in, so the square wave is at its lit phase.
            let m = model(&menu, &data, &net, 0.0);
            assert_eq!(m.caution, want, "{source:?}");
        }
    }

    /// The auth page is a page, not a modal: it has a rail key, a way back, and
    /// a name `SPACESHIPS_UI` can open it by.
    #[test]
    fn the_auth_page_is_wired_in_like_any_other() {
        assert_eq!(Screen::Auth.back(), Some(Screen::Record));
        assert_eq!(Screen::Auth.rail(), Screen::Record.rail());
        assert_eq!(Screen::parse("auth"), Some(Screen::Auth));
        assert_eq!(Screen::parse("login"), Some(Screen::Auth));
        assert!(!Screen::Auth.is_network(), "it is HTTP, not the socket");
        // Every page is in `ALL`, and `index()` is that position — the focus
        // and page arrays are sized from it.
        for (i, s) in Screen::ALL.iter().enumerate() {
            assert_eq!(s.index(), i, "{s:?}");
        }
    }

    /// Typing a passphrase must not press every hotkey in it.
    ///
    /// `hello` used to toggle fullscreen twice (`input.rs` reads `L`) and
    /// `viper` used to seat the pilot in the cockpit (`cockpit.rs` reads `V`),
    /// because both read `ButtonInput<KeyCode>` directly and neither knows a
    /// lobby exists. See [`swallow_typing`].
    #[test]
    fn typing_a_passphrase_does_not_press_the_hotkeys_in_it() {
        use bevy::ecs::system::SystemState;

        fn run(menu: Menu, pressed: &[KeyCode]) -> Vec<KeyCode> {
            let mut world = World::new();
            world.insert_resource(menu);
            let mut keys = ButtonInput::<KeyCode>::default();
            for key in pressed {
                keys.press(*key);
            }
            world.insert_resource(keys);
            let mut state: SystemState<(Res<Menu>, ResMut<ButtonInput<KeyCode>>)> =
                SystemState::new(&mut world);
            {
                let (menu, keys) = state.get_mut(&mut world).expect("params validate");
                swallow_typing(menu, keys);
            }
            state.apply(&mut world);
            let mut left: Vec<KeyCode> = world
                .resource::<ButtonInput<KeyCode>>()
                .get_just_pressed()
                .copied()
                .collect();
            left.sort_by_key(|k| format!("{k:?}"));
            left
        }

        // `hello` and `viper`, plus the keys the menu itself needs.
        let typed = [
            KeyCode::KeyH,
            KeyCode::KeyL,
            KeyCode::KeyV,
            KeyCode::KeyO,
            KeyCode::Enter,
            KeyCode::Escape,
            KeyCode::ArrowDown,
        ];

        let typing = Menu {
            open: true,
            screen: Screen::Auth,
            typing: true,
            ..Menu::default()
        };
        assert_eq!(
            run(typing, &typed),
            vec![KeyCode::ArrowDown, KeyCode::Enter, KeyCode::Escape],
            "a character reached a hotkey, or navigation was swallowed"
        );

        // With the cursor anywhere else, the keyboard is the aircraft's again.
        let flying = Menu {
            open: true,
            screen: Screen::Auth,
            typing: false,
            ..Menu::default()
        };
        assert_eq!(run(flying, &typed).len(), typed.len());

        // ...and a closed menu never touches the keyboard at all, which is
        // every frame of every match.
        let closed = Menu {
            open: false,
            typing: true,
            ..Menu::default()
        };
        assert_eq!(run(closed, &typed).len(), typed.len());
    }

    /// Only the flight lead may start, and the server ignores a `start` from
    /// anyone else — so the row must too, rather than appearing to work.
    #[test]
    fn only_the_flight_lead_launches() {
        let mut menu = Menu {
            open: true,
            screen: Screen::Waiting,
            ..Menu::default()
        };
        let (_, sent, _) = activate_online(Action::LaunchNet, &mut menu, &in_room(true));
        assert_eq!(sent, vec![ClientMessage::Start]);

        let (_, sent, _) = activate_online(Action::LaunchNet, &mut menu, &in_room(false));
        assert!(sent.is_empty());
        assert_eq!(menu.notice, "ONLY THE HOST CAN START");
    }

    /// Walking out of the crew room gives the seat up. Without this the server
    /// keeps the player in `room.players` until the socket closes, and everyone
    /// else sees a pilot who is not there.
    #[test]
    fn leaving_the_crew_room_gives_the_seat_up() {
        // The `LEAVE` row.
        let mut menu = Menu {
            open: true,
            screen: Screen::Waiting,
            ..Menu::default()
        };
        let (_, sent, _) = activate_online(Action::LeaveRoom, &mut menu, &in_room(false));
        assert_eq!(sent, vec![ClientMessage::Leave]);
        assert_eq!(menu.screen, Screen::Net);

        // ...and so does `ESC`, which does not go through an `Action` at all.
        let session = in_room(false);
        let mut config = NetConfig::default();
        let mut menu = Menu {
            open: true,
            screen: Screen::Main,
            ..Menu::default()
        };
        let mut ops = NetOps::new(&session, &mut config, "PILOT");
        on_screen_change(Screen::Waiting, &mut menu, &mut ops);
        assert_eq!(ops.send, vec![ClientMessage::Leave]);
    }

    /// Opening the network rail opens a socket, and the browser asks for a
    /// fresh list rather than showing whatever was cached when it was last
    /// looked at.
    #[test]
    fn the_network_pages_open_the_link() {
        let session = NetSession::default();
        let mut config = NetConfig::default();

        let mut menu = Menu {
            screen: Screen::Net,
            ..Menu::default()
        };
        let mut ops = NetOps::new(&session, &mut config, "MAVERICK");
        on_screen_change(Screen::Main, &mut menu, &mut ops);
        assert_eq!(ops.commands, vec![NetCommand::Connect]);
        // The callsign has to be in place *before* the handshake: `net.rs`
        // sends `name` from the open, not from here.
        assert_eq!(ops.config.callsign, "MAVERICK");

        let mut menu = Menu {
            screen: Screen::Browser,
            ..Menu::default()
        };
        let mut ops = NetOps::new(&session, &mut config, "MAVERICK");
        on_screen_change(Screen::Net, &mut menu, &mut ops);
        assert_eq!(menu.pending, Some(Pending::List));
    }

    /// A row past the end of the live list reads dead, so the fixed pool never
    /// offers a sortie that is not there.
    #[test]
    fn empty_browser_rows_read_dead() {
        let mut menu = Menu::default();
        menu.net.rooms = 2;
        let def = |action| ControlDef {
            screen: Screen::Browser,
            action,
            root: Entity::PLACEHOLDER,
            tick: Entity::PLACEHOLDER,
            label: Entity::PLACEHOLDER,
            value: Entity::PLACEHOLDER,
        };
        let data = LobbyData::placeholder();
        for i in 0..SORTIE_ROWS as u8 {
            let (_, disabled, _) = control_state(&def(Action::SetSortie(i)), &menu, &data);
            assert_eq!(disabled, i >= 2, "row {i}");
        }
        // ...and `JOIN` itself is dead while there is nothing to join.
        let (_, disabled, _) = control_state(&def(Action::JoinSortie), &menu, &data);
        assert!(!disabled);
        menu.net.rooms = 0;
        let (_, disabled, _) = control_state(&def(Action::JoinSortie), &menu, &data);
        assert!(disabled);
    }

    /// The performance rule, extended to the socket.
    ///
    /// A live match bumps `NetSession::rev` on every `players` broadcast and
    /// every second of `match-state`. If that reached the model while the menu
    /// was closed, `drive_menu` would take a `Mut` on the whole tree once a
    /// second through an entire match — which is the per-frame relayout this
    /// module exists to avoid, arriving by a new route.
    #[test]
    fn the_socket_cannot_wake_a_closed_menu() {
        let (mut menu, data, net) = quiet();
        menu.open = false;
        let before = model(&menu, &data, &net, 0.0);

        menu.net = NetView {
            rev: 4_211,
            phase: 3,
            rooms: 5,
            seats: 8,
            host: true,
        };
        assert_eq!(before, model(&menu, &data, &net, 0.0));

        // Open it and the same change does move the model, or the pages would
        // never repaint at all.
        menu.open = true;
        let closed_view = Menu {
            net: NetView::default(),
            ..Menu::default()
        };
        assert_ne!(
            model(&menu, &data, &net, 0.0),
            model(
                &Menu {
                    open: true,
                    screen: menu.screen,
                    pinned: menu.pinned,
                    ..closed_view
                },
                &data,
                &net,
                0.0
            )
        );
    }

    /// The page that was on screen when the menu closed has to be hidden when
    /// it opens again — and a closed menu's model cannot say which one that
    /// was, because it is a constant.
    ///
    /// Getting this wrong left two pages printed over each other: the one the
    /// menu reopened on, and whatever was showing when it closed. It reads as
    /// the client being stuck on the page it started from, and it puts a dead
    /// page over a live match — where the pointer is correctly locked, because
    /// as far as `LobbyOpen` is concerned the lobby is shut.
    #[test]
    fn reopening_the_menu_hides_whatever_was_left_on_screen() {
        // First frame ever: nothing is known, so everything is hidden.
        assert_eq!(page_to_hide(None), None);

        // Menu open, page changes: hide just the one being left.
        let open_on = |screen: Screen| MenuModel {
            open: true,
            screen: screen.index() as u8,
            ..MenuModel::default()
        };
        assert_eq!(
            page_to_hide(Some(open_on(Screen::Waiting))),
            Some(Screen::Waiting.index())
        );

        // Menu closed: the model says `Boot` whatever was really showing, so
        // the only safe answer is all of them.
        let closed = MenuModel {
            open: false,
            ..MenuModel::default()
        };
        assert_eq!(closed.screen, 0, "a closed model reports the first page");
        assert_eq!(page_to_hide(Some(closed)), None);
    }
}
