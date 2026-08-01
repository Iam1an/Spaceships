//! The in-game HUD, in `bevy_ui`.
//!
//! A port of the `#healthbar` / `#boostbar` / `#heatbar` / `#chargebar` /
//! `#missilehud` / `#flarehud` / `#reticle` / `#missile-lock-warning` /
//! `#hit-vignette` / `#deathbanner` / `#matchhud` block of `public/index.html`,
//! driven by [`sim::world::HudState`] instead of by closure variables in
//! `main.js`.
//!
//! # The bug this is escaping
//!
//! The DOM HUD is the single most expensive thing in the JS frame callback, and
//! it does not look like it. Profiling found `LayoutCount == frame count` in
//! all seven runs — one **forced synchronous layout and style recalculation
//! every frame**, 0.21–0.23 ms, roughly 35–40% of the whole callback, and
//! invisible to ordinary rAF profiling because it is attributed to the browser
//! rather than to the script.
//!
//! Two things cause it. `main.js:1980` rebuilds a `linear-gradient(...)` string
//! and assigns it to `style.background` **every frame**, whether or not the
//! player's hit points changed:
//!
//! ```text
//! hpFill.style.background =
//!   `linear-gradient(180deg, hsl(${hue}, 80%, 60%) 0%, hsl(${hue}, 70%, 38%) 100%)`;
//! ```
//!
//! and the floating target boxes write eight style properties per remote player
//! per frame. Eight players is 64 writes plus one forced layout, every frame —
//! which is why the lag the user reports shows up specifically in multiplayer.
//!
//! **Only the appearance is ported. The update pattern is not.**
//!
//! # How this module avoids it
//!
//! The tree is built **once**, in [`spawn_hud`], and is never rebuilt, never
//! re-parented, and never has components inserted or removed at runtime. Every
//! state the CSS expresses as a class — `.overheated`, `.overload`, `.locked`,
//! `.empty` — is expressed here as a *value change on a component that is
//! already there*, so no state transition can cause an archetype move.
//!
//! Every frame [`sync_hud`] reduces the simulation's [`HudState`] to a
//! [`HudModel`]: a `Copy`, `Eq` struct of about twenty integers and booleans,
//! with every float **quantised** to the precision the pixels can actually
//! show (bar widths to a tenth of a percent, exactly the `.toFixed(1)` the JS
//! already rounds to). It compares that against the model it applied last time:
//!
//! - equal — which is the common case, since a ship flying level with full
//!   health changes none of these values — and it returns immediately, having
//!   touched no entity at all. Bevy's change detection triggers on `DerefMut`,
//!   so *not dereferencing* is what keeps the node clean, and a clean node
//!   costs nothing downstream in layout, extraction, or the render phase;
//! - different, and it writes **only the fields that differ**, each behind its
//!   own comparison. Losing a hit point rewrites the health fill's width, its
//!   gradient, and its label — and nothing else in the HUD.
//!
//! The pip rows get the same treatment one level finer: firing a missile
//! compares each pip's own emptiness against its previous emptiness, so it
//! writes the one pip that changed rather than all four. That is the direct
//! answer to the "8 writes per player per frame" pattern.
//!
//! The two genuinely time-driven effects — the 4 Hz missile-lock blink and the
//! overload glow pulse — are folded into the same model as a quantised `bool`
//! and a quantised `u8`, so they flow through the same single comparison, and
//! they are **forced to zero whenever their condition is false**. That matters:
//! a phase that ticked unconditionally would make the model differ every frame
//! and quietly reintroduce exactly the bug above.
//!
//! # The world-space layer
//!
//! `.target-box`, `.target-label`, `.lead-marker` and the reticle itself are
//! not screen furniture: they sit over a point in the 3D scene and therefore
//! move every frame by nature. [`sync_world_markers`] draws them, and it is the
//! one place in this module where a per-frame write is legitimate — so it is
//! also the place where the discipline above matters most:
//!
//! - **The pool is fixed.** [`TARGET_POOL`] slots are built in [`spawn_hud`]
//!   and are never spawned, despawned or re-parented again. A slot is a box, a
//!   label and a lead ring; a frame with three targets leaves five slots
//!   untouched, and a frame with none touches nothing at all.
//! - **Only the position is written per frame**, and it is written as a
//!   [`UiTransform`] rather than as `Node::left`/`top`. That distinction is the
//!   whole point: `Node` is synced into taffy and dirties the layout subtree,
//!   whereas `UiTransform` is consumed by the geometry pass that
//!   `ui_layout_system` runs regardless. Moving a marker therefore costs no
//!   relayout — the direct answer to the forced synchronous layout above.
//! - **Everything else goes through the same diff as the flat HUD.** Each slot
//!   has its own `Copy + Eq` [`MarkerModel`] holding the target's id, hit
//!   points and ring state; the label string, the ring's solid/scattered
//!   swap, the box colour and the visibility flag are written only when that
//!   model moves. A target that is merely *moving* rewrites one `UiTransform`
//!   and nothing else.
//! - **Screen positions are quantised to whole pixels**, so a target that is
//!   holding still on screen — which is what a co-operating wingman or a
//!   parked bot does — produces no write at all.
//! - **An off-screen target costs one hidden flag.** A slot that was already
//!   hidden is not touched, and a contact too far away to engage does not have
//!   its bracket moved at all — only its ring.
//!
//! The one thing that is *not* free is the occlusion sweep that stops a bracket
//! drawing through the moon; [`sync_world_markers`] says what it costs and why
//! it runs last.
//!
//! # What it is not
//!
//! `bevy_ui` has no CSS transitions, no `backdrop-filter`, and no
//! `mix-blend-mode`; where the CSS relies on those the port lands on the end
//! state rather than the animation, and the deviations are noted at each site.
//! It also has no *dashed* border, which is what `.lead-marker` uses to say
//! "the assist has this target but your nose is not on it" — see
//! [`lead_marker`] for how that state is drawn instead.

use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;
use bevy::text::{FontSource, FontWeight, Justify, LetterSpacing};

use sim::rules::Rules;
use sim::world::{is_boss_hitbox, EntityId, Frame, GunMode, HudState, ShipFlags, SimEvent};
use spaceships_sim as sim;

use crate::scene::ShipRoot;
use crate::sim_bridge::{Roster, SimFrame, SimSet, LOCAL_ID};

/// Wires the HUD in: one tree at startup, one diffing system per frame.
pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AppliedHud>()
            .init_resource::<AppliedMarkers>()
            .init_resource::<KillFeed>()
            .init_resource::<TargetLock>()
            .add_systems(Startup, spawn_hud)
            // `SimSet` lives in `FixedUpdate` and this is `Update`, so the
            // ordering is nominal — the point is documentary, matching
            // `scene.rs`: the HUD reads whatever `SimFrame` the most recent
            // fixed tick left behind, and never runs before one exists.
            .add_systems(Update, sync_hud.after(SimSet))
            // The killfeed is the one part of the HUD driven by *events*
            // rather than by state, and an event lives for exactly one tick.
            // Read in `Update` it would double-count a kill whenever the
            // display beats the tick rate and miss one whenever it does not,
            // so it is read where it is published: once per tick.
            .add_systems(FixedUpdate, collect_kills.after(SimSet))
            // The world-space markers need the *interpolated* ship poses and
            // the chase camera's settled transform, neither of which exists
            // before transform propagation — see [`sync_world_markers`].
            .add_systems(
                PostUpdate,
                sync_world_markers.after(TransformSystems::Propagate),
            );
    }
}

// ---------------------------------------------------------------------------
// Design tokens
// ---------------------------------------------------------------------------

/// `rgba()` as the CSS writes it: 8-bit channels, float alpha.
const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
    Color::srgba(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a)
}

/// `#rrggbb` as the CSS writes it.
const fn rgb(r: u8, g: u8, b: u8) -> Color {
    rgba(r, g, b, 1.0)
}

/// `--glass-bg`.
const GLASS_BG: Color = rgba(6, 12, 24, 0.65);
/// The colour half of `--glass-border`. The `1px` half is a [`Node::border`].
const GLASS_BORDER: Color = rgba(102, 221, 255, 0.2);
/// `--color-blue`.
const BLUE: Color = rgb(0x66, 0xdd, 0xff);
/// `--color-gold`.
const GOLD: Color = rgb(0xff, 0xe0, 0x7a);
/// `--color-red`.
const RED: Color = rgb(0xff, 0x55, 0x66);
/// `--color-red-bright`, the one the reticle, the lock warning and the death
/// banner use. Distinct from `--color-red`.
const RED_BRIGHT: Color = rgb(0xff, 0x33, 0x33);
// `#matchhud`'s own `color: #e8f4ff` has no constant here: all three of its
// cells set their own colour, so nothing would ever inherit it.

/// The reticle's cyan, `rgba(102,221,255,0.8)`.
const RETICLE_CYAN: Color = rgba(102, 221, 255, 0.8);

/// The bars' well, `rgba(0,0,0,0.6)`. `#healthbar` uses `0.7`.
const METER_WELL: Color = rgba(0, 0, 0, 0.6);
/// `#healthbar`'s slightly darker well.
const HEALTH_WELL: Color = rgba(0, 0, 0, 0.7);
/// `.meterbar` border.
const METER_BORDER: Color = rgba(255, 255, 255, 0.1);
/// `#healthbar` border.
const HEALTH_BORDER: Color = rgba(255, 255, 255, 0.15);
/// `#boostbar` border.
const BOOST_BORDER: Color = rgba(52, 152, 219, 0.3);
/// `#heatbar` border.
const HEAT_BORDER: Color = rgba(230, 126, 34, 0.3);
/// `.overheated` / `.overload` border, `#e74c3c`.
const ALERT_BORDER: Color = rgb(0xe7, 0x4c, 0x3c);
/// `#chargebar.full` border, `rgba(255,255,255,0.8)`.
const CHARGE_FULL_BORDER: Color = rgba(255, 255, 255, 0.8);

/// Missile pip fill, `#e67e22`.
const MSL_PIP: Color = rgb(0xe6, 0x7e, 0x22);
/// Missile pip when spent, `rgba(230,126,34,0.1)`.
const MSL_PIP_EMPTY: Color = rgba(230, 126, 34, 0.1);
/// Missile pip outline when spent, `rgba(230,126,34,0.4)`.
const MSL_PIP_EMPTY_BORDER: Color = rgba(230, 126, 34, 0.4);
/// Flare pip fill, `#f1c40f`.
const FLA_PIP: Color = rgb(0xf1, 0xc4, 0x0f);
/// Flare pip when spent, `rgba(241,196,15,0.1)`.
const FLA_PIP_EMPTY: Color = rgba(241, 196, 15, 0.1);
/// Flare pip outline when spent, `rgba(241,196,15,0.4)`.
const FLA_PIP_EMPTY_BORDER: Color = rgba(241, 196, 15, 0.4);

/// The label colour shared by `.meterbar-label`, `.msl-label` and `.fla-label`.
const LABEL_WHITE: Color = rgba(255, 255, 255, 0.9);

/// Orbitron, the face the JS HUD is set in.
///
/// The JS pulls it from Google Fonts with a `<link>` in `index.html`. The TTF is
/// now vendored at `public/fonts/` (SIL OFL, so redistributable), which is what
/// closed this gap — before that every string fell back to the `default_font`
/// feature's embedded FiraMono subset, a monospace that reads acceptably for
/// telemetry but is not the same face.
///
/// Held as a resource because a `TextFont` needs a live `Handle<Font>` and
/// [`hud_font`] is called from seven places during layout.
#[derive(Resource)]
struct HudFont(Handle<Font>);

/// Path under the asset root (`public/`). `build-wasm.sh` copies this into the
/// web build's assets alongside the models and sounds.
const FONT_PATH: &str = "fonts/Orbitron-VariableFont_wght.ttf";

/// The font every HUD string is set in.
///
/// Every text node reads from this one place, which is what made the Orbitron
/// swap a single change rather than seven.
fn hud_font(font: &HudFont, size: f32, weight: u16) -> TextFont {
    TextFont {
        font: FontSource::Handle(font.0.clone()),
        font_size: FontSize::Px(size),
        weight: FontWeight(weight),
        ..default()
    }
}

// ---------------------------------------------------------------------------
// Layout constants, straight off the CSS
// ---------------------------------------------------------------------------

/// `width: min(400px, 90vw)`, shared by all four bars.
const BAR_WIDTH: f32 = 400.0;
/// `#healthbar { bottom: 32px; height: 24px }`.
const HEALTH_BOTTOM: f32 = 32.0;
const HEALTH_HEIGHT: f32 = 24.0;
/// `#boostbar { bottom: 64px }`, `.meterbar { height: 12px }`.
const BOOST_BOTTOM: f32 = 64.0;
/// `#heatbar { bottom: 84px }`.
const HEAT_BOTTOM: f32 = 84.0;
const METER_HEIGHT: f32 = 12.0;
/// `#chargebar { bottom: 124px; height: 8px }`.
const CHARGE_BOTTOM: f32 = 124.0;
const CHARGE_HEIGHT: f32 = 8.0;
/// `#missilehud` / `#flarehud { bottom: 104px }`.
const PIP_ROW_BOTTOM: f32 = 104.0;
/// `.msl-pip` / `.fla-pip { width: 16px; height: 12px }`.
const PIP_WIDTH: f32 = 16.0;
const PIP_HEIGHT: f32 = 12.0;

/// Missile pips drawn. From the rules rather than from the markup's four
/// hardcoded `<span>`s — `rules.rs` is where a carried-count lives.
const MISSILE_PIPS: usize = Rules::DEFAULT.weapons.missile_max as usize;
/// Flare pips drawn.
const FLARE_PIPS: usize = Rules::DEFAULT.weapons.flare_max as usize;
/// Denominator of the health readout, `100`.
const MAX_HP: i32 = Rules::DEFAULT.ship.max_hp;

/// `#missile-lock-warning`'s string.
///
/// The markup reads `⚠ MISSILE LOCK ⚠`. The `default_font` feature's embedded
/// FiraMono subset is documented as ASCII-only, so U+26A0 renders as a tofu
/// box — visibly worse than no glyph. Restore the warning signs together with
/// the Orbitron swap described on [`hud_font`]; both are blocked on the same
/// missing font file.
const LOCK_WARNING_TEXT: &str = ">> MISSILE LOCK <<";

/// `z-index: 4` — `#hit-vignette` and `#matchhud`.
const Z_OVERLAY: i32 = 4;
/// `z-index: 5` — `#missile-lock-warning`.
const Z_WARNING: i32 = 5;

// --- the world-space layer -------------------------------------------------

/// Target slots built at startup.
///
/// Eight, because eight is the number the module header names as the profiling
/// case that motivated this whole port — "eight style writes per remote player
/// per frame", at eight players. A ninth simultaneously-visible enemy simply
/// does not get a box; it does not get a *slower frame*, which is the property
/// worth having. Boss hitboxes are excluded from targeting outright (there are
/// twenty of them and they are one ship), so the campaign cannot exhaust it.
const TARGET_POOL: usize = 8;

/// `.target-box`, tightened.
///
/// The CSS is 64px square with 14px arms. At 64 the bracket is wider than the
/// ship inside it at any range worth shooting at, and two enemies flying
/// together put their brackets through each other — which is what a skirmish
/// spawn looks like the moment it starts. 44 is the same drawing at the size
/// the thing it is bracketing actually occupies.
const BOX_SIZE: f32 = 44.0;
/// The length of one corner bracket arm — the `14px` colour stop in each of
/// `.target-box`'s eight gradients, scaled with the box.
const BOX_CORNER: f32 = 10.0;
/// Bracket thickness — the `2px` in `background-size: 100% 2px`.
const BOX_STROKE: f32 = 2.0;
/// `.target-label { left: 70px }`, moved in with the box's edge.
const LABEL_LEFT: f32 = 50.0;

/// `.lead-marker { width: 16px; height: 16px }`.
const LEAD_SIZE: f32 = 16.0;
/// Dashes in the scattered ring. See [`lead_marker`].
const LEAD_DASHES: usize = 8;
/// Diameter of one scattered-ring dash, in pixels.
const LEAD_DASH: f32 = 3.5;

/// How near the aim point a target has to project before the lead ring snaps
/// solid and the reticle locks, in pixels.
///
/// `main.js:1928` — `r.lead.classList.toggle('aligned', screenDist < 22)`, and
/// `main.js:1929` reuses the same number for `#reticle.locked`. It is a screen
/// distance rather than an angle in the JS and stays one here, so the feel does
/// not change with the field of view.
const ALIGN_PX: f32 = 22.0;

/// How far outside the viewport a marker may sit before it is hidden.
///
/// `main.js:1913` — `sx < -32 || sx > W + 32 || ...`, i.e. half a target box,
/// so a box does not pop as its centre crosses the edge.
const OFFSCREEN_MARGIN: f32 = BOX_SIZE / 2.0;

/// Beyond this, a ship gets no HUD marker at all. `MARKER_VISIBLE_DIST`,
/// `main.js:1003`.
const MARKER_VISIBLE_DIST: f32 = 1500.0;

/// Beyond this a contact keeps its ring but loses its bracket and its name.
///
/// The JS draws the full box out to [`MARKER_VISIBLE_DIST`], which puts a
/// 44-pixel bracket and a callsign on a ship half a kilometre past the range
/// anything you own can reach it at — clutter that says nothing actionable.
/// Inside this range the target is one the aim assist will work on and the guns
/// will reach; outside it, the ring alone says "someone is over there".
///
/// It is [`sim::rules::AimAssistRules::range`] rather than a number invented
/// here, so "the bracket means you can engage" cannot drift away from what
/// engaging actually costs.
const ENGAGE_DIST: f32 = Rules::DEFAULT.aim_assist.range as f32;

/// How far down the gun line the reticle sits when there is nothing to shoot.
///
/// `BEAM_RANGE`, `main.js:1045`. The JS raycasts along the aim vector and puts
/// the reticle on whatever it *hits* (`main.js:1832`), and that detail turns
/// out to be load-bearing rather than cosmetic. **The gun line does not project
/// to a single screen point.** The camera sits eleven units behind the muzzle
/// and five above it, so it is not on the line; a target 250 units down it and
/// one 1000 units down it land about a degree — twenty-odd pixels — apart.
///
/// Two consequences, handled separately:
///
/// - **The alignment test cannot use one fixed point.** Measured against a
///   reticle pinned at 1000, a close target you are aimed squarely at reads as
///   twenty pixels off and the ring never fills in — which is exactly the "the
///   aimlock doesnt work" report. So each contact is compared against the point
///   on the gun line *at its own range*, which is parallax-free by
///   construction. See [`sync_world_markers`].
/// - **The reticle still has to be drawn somewhere**, and with no scene raycast
///   the honest answer is "at the range of whatever you are most nearly aimed
///   at". This constant is the empty-sky fallback.
const AIM_RANGE: f32 = 1000.0;

/// Closest the aim point may be placed, so a target you are inside of cannot
/// put the reticle behind the camera.
const AIM_RANGE_MIN: f32 = 20.0;

/// Rows in the killfeed. `main.js:2039` trims to five.
const KILLFEED_ROWS: usize = 5;
/// How long a killfeed row stays up.
///
/// `main.js:2040` starts a fade at 3.6 s and removes the row 0.42 s later.
/// `bevy_ui` has no CSS animations, so the row is drawn and then dropped at the
/// moment the JS finishes fading it.
const KILL_TTL: f64 = 4.0;

/// `.kf-entry` background, `rgba(6,12,24,0.7)`.
const KF_BG: Color = rgba(6, 12, 24, 0.7);
/// `.kf-victim` colour, `#8fb6d6`.
const KF_VICTIM: Color = rgb(0x8f, 0xb6, 0xd6);
/// `.kf-icon` colour, `rgba(255,255,255,0.5)`.
const KF_ICON: Color = rgba(255, 255, 255, 0.5);
/// `.kf-icon`'s `→`. The `default_font` fallback is ASCII-only (see
/// [`LOCK_WARNING_TEXT`]), and Orbitron has no U+2192 either.
const KF_ARROW: &str = ">>";

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/// What `#chargebar`'s class list can say.
///
/// `main.js:1336` toggles `active`, `full` and `overload` independently, but
/// they are strictly nested — `overload` implies `full` implies `active` — so
/// one enum says the same thing and makes "did the state change" a single
/// comparison rather than three.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum ChargeState {
    /// No class: `opacity: 0`.
    #[default]
    Idle,
    /// `.active`.
    Active,
    /// `.active.full`.
    Full,
    /// `.active.full.overload`.
    Overload,
}

/// Everything drawable about one frame of the HUD, quantised to what a pixel
/// can show.
///
/// The whole design rests on this being `Eq`: no floats, so two frames of an
/// unchanging situation compare equal and [`sync_hud`] can bail out before
/// touching a single component. Every field is either a discrete simulation
/// value (hit points, missiles remaining) or a float rounded to the precision
/// the JS already rounded to when it wrote a percentage string.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct HudModel {
    /// Whether there is a local ship at all. False before the simulation has
    /// spawned one, and the whole tree is hidden.
    present: bool,
    /// Whether the local ship is alive; drives `#deathbanner`.
    alive: bool,

    /// Hit points, for the `"h / 100"` readout.
    hp: i32,
    /// Health bar width in tenths of a percent — the `.toFixed(1)` of
    /// `main.js:1978`.
    hp_mil: u16,
    /// `Math.round(pct * 120)` from `main.js:1979`, the hue of the health
    /// gradient. Tracked separately from `hp_mil` so the gradient is rebuilt
    /// only when the *colour* moves, not merely when the width does.
    hp_hue: u16,

    /// Boost bar width, tenths of a percent.
    boost_mil: u16,
    /// Gun bar width, tenths of a percent.
    heat_mil: u16,
    /// `#heatbar.overheated`: `ammo < cost`, i.e. the selected gun cannot fire.
    overheated: bool,

    /// Charge bar width, tenths of a percent.
    charge_mil: u16,
    /// `#chargebar`'s class list.
    charge: ChargeState,

    /// Missiles remaining.
    missiles: u8,
    /// Flares remaining.
    flares: u8,

    /// `#reticle.locked`.
    reticle_locked: bool,
    /// Whether `#missile-lock-warning` is shown at all.
    lock_warning: bool,
    /// The lit half of the 4 Hz `msl-lock-blink` square wave. **Forced false
    /// when `lock_warning` is false**, so an unlocked HUD has a constant model.
    lock_blink: bool,
    /// The `chargebar-overload-pulse` / `heatpulse` phase, quantised to
    /// [`PULSE_STEPS`]. **Forced zero when nothing is pulsing.**
    pulse: u8,

    /// `#hit-vignette`'s opacity, in 64ths.
    vignette: u8,

    /// Whether `#matchhud` is shown.
    match_on: bool,
    /// `#team0score`.
    team0: u32,
    /// `#team1score`.
    team1: u32,
    /// Whole seconds on the clock, `Math.ceil` as `fmtTime` does.
    clock: u32,

    /// `#killfeed`, newest row first.
    kills: [KillRowModel; KILLFEED_ROWS],
}

/// One `#killfeed` row, as the tree needs it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct KillRowModel {
    /// Whether the row is up at all — false once [`KILL_TTL`] has passed.
    shown: bool,
    /// Which kill this is. Monotonic, so a row that scrolled down one place
    /// still compares equal to itself and is not rewritten.
    serial: u32,
    /// Who scored it.
    killer: EntityId,
    /// Who died.
    victim: EntityId,
}

/// The model [`sync_hud`] last wrote to the tree. `None` until the first frame,
/// which is what makes that frame write everything.
#[derive(Resource, Default)]
struct AppliedHud(Option<HudModel>);

/// One world-space target slot, as the tree needs it.
///
/// Deliberately **not** carrying the screen position. Position is the one thing
/// that genuinely changes every frame, and folding it in here would make every
/// field's comparison fire whenever a target so much as drifted a pixel. It is
/// written separately, straight onto a [`UiTransform`], where `set_if_neq`
/// catches the stationary case on its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct MarkerModel {
    /// Whether this slot is drawing at all. False leaves every other field
    /// stale, and nothing reads them.
    shown: bool,
    /// Whether the bracket and the callsign are drawn as well as the ring —
    /// that is, whether the contact is inside [`ENGAGE_DIST`].
    boxed: bool,
    /// Which ship the slot is tracking. Zero is no ship: ids start at
    /// [`LOCAL_ID`], which is 1.
    id: EntityId,
    /// The target's hit points, for the label.
    hp: i32,
    /// `.lead-marker.aligned` — the ring is solid rather than scattered. At
    /// most one slot has this set: it is *the* lock, not a threshold several
    /// targets can pass at once.
    aligned: bool,
    /// Whether aim assist is currently holding *this* target
    /// ([`HudState::assist_target`]), which brightens the bracket.
    assisted: bool,
}

/// What [`sync_world_markers`] last wrote to each slot.
#[derive(Resource, Default)]
struct AppliedMarkers([MarkerModel; TARGET_POOL]);

/// Whether the player's aim is on a target this frame.
///
/// Written by [`sync_world_markers`], which is the only system that knows where
/// anything projects to, and read by [`sync_hud`] for `#reticle.locked`. A
/// resource rather than a return value because the two run in different
/// schedules; the lock therefore lights one frame after the alignment, which no
/// one can see and which is the same latency the marker itself has.
#[derive(Resource, Default)]
struct TargetLock(bool);

/// The killfeed's backing store: five rows, newest first.
///
/// Filled by [`collect_kills`] from [`SimEvent::ShipDestroyed`] and read by
/// [`model`]. Nothing here is per-frame — a row is written when someone dies
/// and read until it expires.
#[derive(Resource, Default)]
struct KillFeed {
    /// The rows themselves, index 0 newest.
    rows: [KillRowModel; KILLFEED_ROWS],
    /// Simulation time each row was pushed at, in [`Frame::time`]'s clock.
    at: [f64; KILLFEED_ROWS],
    /// Kills seen so far, which is where [`KillRowModel::serial`] comes from.
    serial: u32,
}

impl KillFeed {
    /// Pushes a row on top, scrolling the rest down and dropping the oldest.
    fn push(&mut self, killer: EntityId, victim: EntityId, now: f64) {
        self.rows.rotate_right(1);
        self.at.rotate_right(1);
        self.serial += 1;
        self.rows[0] = KillRowModel {
            shown: true,
            serial: self.serial,
            killer,
            victim,
        };
        self.at[0] = now;
    }

    /// The rows as of `now`, with expired ones marked hidden.
    fn model(&self, now: f64) -> [KillRowModel; KILLFEED_ROWS] {
        let mut out = self.rows;
        for (row, at) in out.iter_mut().zip(self.at) {
            // `serial == 0` is a row that has never held a kill. Both tests are
            // needed: the array starts zeroed and `now` starts at zero too.
            row.shown = row.serial != 0 && now - at < KILL_TTL;
        }
        out
    }
}

/// Steps in one half of the overload pulse.
///
/// The CSS animates `box-shadow` continuously over 0.3 s. Eight steps is under
/// a pixel of blur apiece at these radii, and caps the pulse at ~27 writes per
/// second on one node instead of one per frame — and only while the player is
/// actually overloaded, which is a second or two at a time.
const PULSE_STEPS: u8 = 8;
/// `chargebar-overload-pulse` / `heatpulse` duration, one direction. Both CSS
/// animations are `0.3s ... alternate`, so the full cycle is 0.6 s.
const PULSE_HALF_PERIOD: f32 = 0.3;
/// `msl-lock-blink` is `0.25s step-start`, on for the first half.
const BLINK_HALF_PERIOD: f32 = 0.125;
/// Quantisation of `#hit-vignette`'s opacity. The flash decays over well under
/// a second, so 64 steps is a handful of writes per hit and none afterwards.
const VIGNETTE_STEPS: f32 = 64.0;

/// What a hit is worth at **full** health, as a fraction of the vignette.
///
/// A deliberate departure from `main.js:1988`, which sets `opacity` to the raw
/// `vignetteAlpha` and so draws an identical flash whether the hit took you
/// from 100 to 90 or from 20 to 10. Scaling the flash by how much hull is
/// already gone makes the effect say something the health bar cannot say in
/// peripheral vision: the same shot reads as a tap when you are fresh and as a
/// scream when you are nearly dead. At zero hull the gain is 1 and the rim is
/// at full strength.
const VIGNETTE_HEALTHY_GAIN: f32 = 0.45;

/// Reduces one simulation frame to the pixels it implies.
///
/// Pure, and deliberately free of Bevy — which is what lets the tests below
/// assert the no-change property directly.
///
/// `locked` comes from [`TargetLock`] rather than from the frame: whether the
/// player's aim is on someone is a question about *projection*, and only
/// [`sync_world_markers`] can answer it.
fn model(frame: &Frame, time: f32, seated: bool, locked: bool, feed: &KillFeed) -> HudModel {
    // Seated in the cockpit, the 3D instrument panel *is* the HUD, so the flat
    // overlay stands down entirely. `main.js:1` does the same with
    // `document.body.classList.toggle('cockpit-view', fp)` — without it the
    // bottom bars sit on top of the panel.
    if seated {
        return HudModel::default();
    }
    // `ShipFlags::LOCAL` is set by `sim_bridge::ship_view` for the ship whose
    // id matches `World::local_id`. Before the first tick there is no frame and
    // no ship, and the HUD stays hidden rather than drawing a dead one.
    let Some(me) = frame
        .ships
        .iter()
        .find(|s| s.flags.contains(ShipFlags::LOCAL))
    else {
        return HudModel::default();
    };

    let hud: &HudState = &frame.hud;
    let alive = me.flags.contains(ShipFlags::ALIVE);

    let hp01 = hud.hp01.clamp(0.0, 1.0);
    let charge01 = hud.charge01.clamp(0.0, 1.0);

    // `heatbar.classList.toggle('overheated', ammo < (gunMode === 'beam' ? 3 : 1))`
    // — `main.js:1348`. `HudState` carries the fraction, not the count, so the
    // count comes back out of it. Both costs come from `rules.rs`; neither is
    // written down here.
    let weapons = &Rules::DEFAULT.weapons;
    let cost = match hud.gun_mode {
        GunMode::Bullet => weapons.bullet_ammo_cost,
        GunMode::Beam => weapons.beam_ammo_cost,
    };
    let ammo = f64::from(hud.ammo01.clamp(0.0, 1.0)) * weapons.max_ammo;
    let overheated = alive && ammo < cost;

    // `main.js:1336`. The JS also lights `.active` while `braking` even at zero
    // charge; `HudState` has no braking flag (see the gap noted in the module
    // header of `sim_bridge`), so charge alone drives it. The difference is one
    // frame of an empty 8px bar at the very start of a brake.
    //
    // `overcharge01` is documented as "1 is taking damage", i.e. it is the
    // overcharge timer over `brake_overcharge_damage_delay`. The warning
    // threshold is `brake_overcharge_warn` on that same clock, so it lands at
    // the ratio of the two rather than at a number invented here.
    let ship = &Rules::DEFAULT.ship;
    let warn_at = (ship.brake_overcharge_warn / ship.brake_overcharge_damage_delay) as f32;
    let charge = if charge01 >= 1.0 && hud.overcharge01 >= warn_at {
        ChargeState::Overload
    } else if charge01 >= 1.0 {
        ChargeState::Full
    } else if charge01 > 0.0 {
        ChargeState::Active
    } else {
        ChargeState::Idle
    };

    let pulsing = overheated || charge == ChargeState::Overload;
    let lock_warning = hud.missile_lock_warning && alive;

    HudModel {
        present: true,
        alive,

        hp: hud.hp.max(0),
        hp_mil: mil(hp01),
        // `Math.round(pct * 120)`: pure green at full, pure red at zero.
        hp_hue: (hp01 * 120.0).round() as u16,

        boost_mil: mil(hud.boost01.clamp(0.0, 1.0)),
        heat_mil: mil(hud.ammo01.clamp(0.0, 1.0)),
        overheated,

        charge_mil: mil(charge01),
        charge,

        missiles: hud.missiles,
        flares: hud.flares,

        // `main.js:1929` — `anyVisible && bestAlignment < 22`, which is a
        // question about where things land on screen and so is answered by
        // `sync_world_markers`. This deliberately does *not* read
        // `HudState::assist_target`: the assist holds anything inside a 53
        // degree cone, which would leave the reticle red almost permanently.
        // `assist_target` earns its keep on the target bracket instead — see
        // `MarkerModel::assisted`.
        reticle_locked: locked && alive,
        lock_warning,
        // Both of these are zero unless their effect is running. That is not a
        // micro-optimisation: a phase that advanced unconditionally would make
        // every frame's model differ from the last and defeat the entire
        // early-out below.
        lock_blink: lock_warning && phase(time, BLINK_HALF_PERIOD).is_multiple_of(2),
        pulse: if pulsing {
            triangle(time, PULSE_HALF_PERIOD, PULSE_STEPS)
        } else {
            0
        },

        // `camTel.hitFlash = vignetteAlpha` (`main.js:1942`) — the ship view's
        // hit flash drives the vignette, so the HUD reads it off the local
        // `ShipView` rather than needing its own decay. The hull term is the
        // departure; see [`VIGNETTE_HEALTHY_GAIN`].
        vignette: (me.hit_flash.clamp(0.0, 1.0)
            * (VIGNETTE_HEALTHY_GAIN + (1.0 - VIGNETTE_HEALTHY_GAIN) * (1.0 - hp01))
            * VIGNETTE_STEPS)
            .round() as u8,

        // `match_timer` alone is not the test: it holds the match's full
        // duration before the clock starts and in modes that have no clock,
        // which drew the multiplayer scoreline over the campaign.
        match_on: hud.match_active,
        team0: hud.team_kills[0],
        team1: hud.team_kills[1],
        clock: hud.match_timer.max(0.0).ceil() as u32,

        // On the simulation's clock, not the render clock: the rows are pushed
        // from a tick and this is what keeps the two agreeing about "4 seconds
        // ago" across a hitch.
        kills: feed.model(frame.time),
    }
}

/// `0..1` to tenths of a percent — the precision `(x * 100).toFixed(1)` keeps.
///
/// This is the quantisation that makes the whole scheme work: a bar that is
/// full, or empty, or simply not moving fast enough to shift by a thousandth
/// of its width, compares equal to last frame and is not written.
fn mil(v: f32) -> u16 {
    (v * 1000.0).round().clamp(0.0, 1000.0) as u16
}

/// Which half-period `time` falls in. Used for square waves.
fn phase(time: f32, half_period: f32) -> u64 {
    (time.max(0.0) / half_period) as u64
}

/// A `0..=steps` triangle wave — the `alternate` of a CSS keyframe animation.
fn triangle(time: f32, half_period: f32, steps: u8) -> u8 {
    let t = time.max(0.0) / half_period;
    let up = phase(time, half_period).is_multiple_of(2);
    let frac = t.fract();
    let frac = if up { frac } else { 1.0 - frac };
    (frac * f32::from(steps)).round().min(f32::from(steps)) as u8
}

/// `fmtTime` from `main.js:3388`: `m:ss`, rounded up, never negative.
fn fmt_clock(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

// ---------------------------------------------------------------------------
// The tree
// ---------------------------------------------------------------------------

/// Every entity [`sync_hud`] may write to.
///
/// Held by id rather than found by marker query. A marker query would be
/// perfectly fast, but this makes the "which nodes exist" question answerable
/// by reading one struct, and makes it structurally impossible to write to a
/// node that was not built at startup.
#[derive(Resource)]
struct HudNodes {
    root: Entity,

    health_fill: Entity,
    health_text: Entity,

    boost_fill: Entity,

    heat_frame: Entity,
    heat_fill: Entity,

    charge_frame: Entity,
    charge_fill: Entity,

    missile_pips: [Entity; MISSILE_PIPS],
    flare_pips: [Entity; FLARE_PIPS],

    /// The node that carries the reticle's *position*. Split from the ring
    /// below because `sync_hud` owns the ring's `.locked` scale and
    /// `sync_world_markers` owns the position, and a `UiTransform` holds both
    /// scale and translation — one component, two writers, one clobbering the
    /// other. Two nodes, one writer each.
    reticle_anchor: Entity,
    reticle: Entity,
    reticle_ticks: [Entity; 2],

    lock_warning: Entity,
    vignette: Entity,
    death_banner: Entity,

    match_panel: Entity,
    team0: Entity,
    team1: Entity,
    clock: Entity,

    markers: [MarkerNodes; TARGET_POOL],
    killfeed: [KillRowNodes; KILLFEED_ROWS],
}

/// One world-space target slot's entities.
#[derive(Clone, Copy)]
struct MarkerNodes {
    /// `.target-box`. Carries the slot's position and its visibility; the
    /// label rides along as a child, which is what `main.js:679`
    /// (`box.appendChild(label)`) does and what keeps the label to one write.
    boxes: Entity,
    /// The four corner brackets, whose colour says whether aim assist has
    /// picked this target.
    corners: [Entity; 4],
    /// `.target-label`.
    label: Entity,
    /// `.lead-marker`, positioned separately because it is 16px where the box
    /// is 64 and the two therefore need different offsets from the same point.
    lead: Entity,
    /// The `.aligned` ring: solid, filled.
    lead_solid: Entity,
    /// The scattered ring that stands in for `border-style: dashed`.
    lead_dashed: Entity,
}

impl Default for MarkerNodes {
    fn default() -> Self {
        MarkerNodes {
            boxes: Entity::PLACEHOLDER,
            corners: [Entity::PLACEHOLDER; 4],
            label: Entity::PLACEHOLDER,
            lead: Entity::PLACEHOLDER,
            lead_solid: Entity::PLACEHOLDER,
            lead_dashed: Entity::PLACEHOLDER,
        }
    }
}

/// One `.kf-entry`'s entities.
#[derive(Clone, Copy)]
struct KillRowNodes {
    /// The row itself, hidden when the entry has expired.
    row: Entity,
    /// `.kf-killer`.
    killer: Entity,
    /// `.kf-victim`.
    victim: Entity,
}

impl Default for KillRowNodes {
    fn default() -> Self {
        KillRowNodes {
            row: Entity::PLACEHOLDER,
            killer: Entity::PLACEHOLDER,
            victim: Entity::PLACEHOLDER,
        }
    }
}

/// Builds the HUD. Runs once.
///
/// No system after this one spawns, despawns, re-parents, inserts or removes
/// anything. Everything a CSS class would toggle is present from the start with
/// its inactive value — a zero-alpha border, a zero-alpha shadow, a hidden
/// node — so a state change is only ever a write to an existing component and
/// never an archetype move.
#[expect(
    clippy::too_many_lines,
    reason = "one contiguous declaration of the tree; splitting it into a \
              dozen single-use builders would hide the layout rather than \
              clarify it"
)]
fn spawn_hud(mut commands: Commands, assets: Res<AssetServer>) {
    let font = HudFont(assets.load(FONT_PATH));
    // No camera is spawned here. `bevy_ui` renders root nodes to the default UI
    // camera, which `DefaultUiCamera` resolves as the highest-order camera
    // targeting the primary window — `camera.rs`'s `Camera3d`. Adding a
    // `Camera2d` would give the window a second camera and make that choice
    // ambiguous.
    let mut health_fill = Entity::PLACEHOLDER;
    let mut health_text = Entity::PLACEHOLDER;
    let mut boost_fill = Entity::PLACEHOLDER;
    let mut heat_frame = Entity::PLACEHOLDER;
    let mut heat_fill = Entity::PLACEHOLDER;
    let mut charge_frame = Entity::PLACEHOLDER;
    let mut charge_fill = Entity::PLACEHOLDER;
    let mut missile_pips = [Entity::PLACEHOLDER; MISSILE_PIPS];
    let mut flare_pips = [Entity::PLACEHOLDER; FLARE_PIPS];
    let mut reticle_anchor = Entity::PLACEHOLDER;
    let mut reticle = Entity::PLACEHOLDER;
    let mut reticle_ticks = [Entity::PLACEHOLDER; 2];
    let mut markers = [MarkerNodes::default(); TARGET_POOL];
    let mut killfeed = [KillRowNodes::default(); KILLFEED_ROWS];
    let mut lock_warning = Entity::PLACEHOLDER;
    let mut vignette = Entity::PLACEHOLDER;
    let mut death_banner = Entity::PLACEHOLDER;
    let mut match_panel = Entity::PLACEHOLDER;
    let mut team0 = Entity::PLACEHOLDER;
    let mut team1 = Entity::PLACEHOLDER;
    let mut clock = Entity::PLACEHOLDER;

    let root = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(0),
                top: px(0),
                width: percent(100),
                height: percent(100),
                ..default()
            },
            // Hidden until the simulation produces a local ship, which is what
            // `display:none` does for every one of these elements in the markup.
            Visibility::Hidden,
        ))
        .with_children(|hud| {
            // -- #healthbar -------------------------------------------------
            hud.spawn(centred_row(HEALTH_BOTTOM)).with_children(|row| {
                row.spawn((
                    bar_frame(HEALTH_HEIGHT, HEALTH_WELL, HEALTH_BORDER, 2.0),
                    // `box-shadow: 0 8px 24px rgba(0,0,0,0.6)`.
                    BoxShadow::new(rgba(0, 0, 0, 0.6), px(0), px(8), px(0), px(24)),
                ))
                .with_children(|bar| {
                    health_fill = bar
                        .spawn((
                            fill_node(),
                            // The CSS declares a static green gradient which
                            // `main.js:1980` immediately overwrites with an
                            // hsl one every frame. This is the one the player
                            // actually sees; it is rebuilt here only when the
                            // hue moves.
                            health_gradient(120),
                        ))
                        .id();
                    // `#healthbar-text`: `inset: 0`, flex-centred both ways.
                    bar.spawn(Node {
                        position_type: PositionType::Absolute,
                        left: px(0),
                        right: px(0),
                        top: px(0),
                        bottom: px(0),
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::Center,
                        ..default()
                    })
                    .with_children(|slot| {
                        health_text = slot
                            .spawn((
                                Text::new(format!("{MAX_HP} / {MAX_HP}")),
                                hud_font(&font, 14.0, 800),
                                TextColor(Color::WHITE),
                                LetterSpacing::Px(4.0),
                                // `text-shadow: 0 2px 4px rgba(0,0,0,0.9)`.
                                // Bevy's shadow has an offset but no blur.
                                TextShadow {
                                    offset: Vec2::new(0.0, 2.0),
                                    color: rgba(0, 0, 0, 0.9),
                                },
                            ))
                            .id();
                    });
                });
            });

            // -- #boostbar --------------------------------------------------
            hud.spawn(centred_row(BOOST_BOTTOM)).with_children(|row| {
                row.spawn((
                    bar_frame(METER_HEIGHT, METER_WELL, BOOST_BORDER, 1.0),
                    meter_shadow(),
                ))
                .with_children(|bar| {
                    boost_fill = bar
                        .spawn((
                            fill_node(),
                            // `linear-gradient(90deg, #2980b9 0%, #3498db 100%)`.
                            gradient_90(rgb(0x29, 0x80, 0xb9), rgb(0x34, 0x98, 0xdb)),
                        ))
                        .id();
                    bar.spawn(meter_label_slot()).with_children(|slot| {
                        slot.spawn(meter_label(&font, "BOOST"));
                    });
                });
            });

            // -- #heatbar ---------------------------------------------------
            hud.spawn(centred_row(HEAT_BOTTOM)).with_children(|row| {
                heat_frame = row
                    .spawn((
                        bar_frame(METER_HEIGHT, METER_WELL, HEAT_BORDER, 1.0),
                        // `.overheated` swaps this drop shadow for a red glow.
                        // Spawned present so the swap is a value change.
                        meter_shadow(),
                    ))
                    .with_children(|bar| {
                        heat_fill = bar
                            .spawn((
                                fill_node(),
                                // `linear-gradient(90deg, #d35400 0%, #e67e22 100%)`.
                                gradient_90(rgb(0xd3, 0x54, 0x00), rgb(0xe6, 0x7e, 0x22)),
                            ))
                            .id();
                        bar.spawn(meter_label_slot()).with_children(|slot| {
                            slot.spawn(meter_label(&font, "GUN"));
                        });
                    })
                    .id();
            });

            // -- #chargebar -------------------------------------------------
            hud.spawn(centred_row(CHARGE_BOTTOM)).with_children(|row| {
                charge_frame = row
                    .spawn((
                        bar_frame(CHARGE_HEIGHT, METER_WELL, METER_BORDER, 1.0),
                        meter_shadow(),
                        // `opacity: 0` until `.active`. `Visibility` rather
                        // than `Display::None`: hiding by display would drop
                        // the node out of layout and force a relayout on every
                        // brake, which is the cost this port exists to avoid.
                        Visibility::Hidden,
                    ))
                    .with_children(|bar| {
                        charge_fill = bar
                            .spawn((
                                Node {
                                    // `#chargebar-fill { width: 0% }` — the one
                                    // bar that starts empty rather than full.
                                    width: percent(0),
                                    height: percent(100),
                                    ..default()
                                },
                                charge_gradient(false),
                            ))
                            .id();
                    })
                    .id();
            });

            // -- #missilehud ------------------------------------------------
            // `transform: translateX(calc(-100% - 16px))` off `left: 50%`,
            // which is "right edge 16px left of centre" — expressed here as
            // `right: 50%` plus a 16px right margin, no transform needed.
            hud.spawn(Node {
                position_type: PositionType::Absolute,
                right: percent(50),
                bottom: px(PIP_ROW_BOTTOM),
                margin: UiRect::right(px(16)),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: px(8),
                ..default()
            })
            .with_children(|row| {
                row.spawn(pip_label(&font, "MSL"));
                for slot in &mut missile_pips {
                    *slot = row.spawn(pip(MSL_PIP)).id();
                }
            });

            // -- #flarehud --------------------------------------------------
            hud.spawn(Node {
                position_type: PositionType::Absolute,
                left: percent(50),
                bottom: px(PIP_ROW_BOTTOM),
                margin: UiRect::left(px(16)),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: px(8),
                ..default()
            })
            .with_children(|row| {
                row.spawn(pip_label(&font, "FLR"));
                for slot in &mut flare_pips {
                    *slot = row.spawn(pip(FLA_PIP)).id();
                }
            });

            // -- .target-box / .target-label / .lead-marker ------------------
            // The pool. Built once, hidden, and thereafter only ever moved and
            // shown — never spawned, never despawned, never re-parented. Boxes
            // first so the lead rings and the reticle draw over them.
            for slot in &mut markers {
                *slot = spawn_marker(hud, &font);
            }

            // -- #reticle ---------------------------------------------------
            // Two nodes: an anchor that `sync_world_markers` slides onto the
            // projected aim point, and the ring itself, whose `.locked`
            // transform `sync_hud` owns. The anchor starts screen-centred, so
            // a frame before the first projection — or a frame with no local
            // ship — draws the reticle exactly where it used to be.
            reticle_anchor = hud
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: percent(50),
                        top: percent(50),
                        width: px(16),
                        height: px(16),
                        // `margin: -8px 0 0 -8px`.
                        margin: UiRect::new(px(-8), px(0), px(-8), px(0)),
                        ..default()
                    },
                    UiTransform::IDENTITY,
                ))
                .with_children(|anchor| {
                    reticle = anchor
                        .spawn((
                            Node {
                                position_type: PositionType::Absolute,
                                left: px(0),
                                top: px(0),
                                width: percent(100),
                                height: percent(100),
                                border: UiRect::all(px(1)),
                                border_radius: BorderRadius::all(percent(50)),
                                ..default()
                            },
                            BorderColor::all(RETICLE_CYAN),
                            BoxShadow::new(rgba(102, 221, 255, 0.4), px(0), px(0), px(0), px(8)),
                            // `.locked` scales to 1.2. A `UiTransform` is
                            // applied after layout, so the lock state costs no
                            // relayout — where animating `width`/`height`
                            // would.
                            UiTransform::IDENTITY,
                        ))
                        .with_children(|r| {
                            // `#reticle::before` — a 1x6 tick above the ring.
                            reticle_ticks[0] = r
                                .spawn((
                                    Node {
                                        position_type: PositionType::Absolute,
                                        left: percent(50),
                                        top: px(-8),
                                        width: px(1),
                                        height: px(6),
                                        ..default()
                                    },
                                    BackgroundColor(RETICLE_CYAN),
                                ))
                                .id();
                            // `#reticle::after` — a 6x1 tick to the left of it.
                            // The crosshair really is asymmetric in the CSS;
                            // this is not a porting slip.
                            reticle_ticks[1] = r
                                .spawn((
                                    Node {
                                        position_type: PositionType::Absolute,
                                        top: percent(50),
                                        left: px(-8),
                                        width: px(6),
                                        height: px(1),
                                        ..default()
                                    },
                                    BackgroundColor(RETICLE_CYAN),
                                ))
                                .id();
                        })
                        .id();
                })
                .id();

            // -- #killfeed --------------------------------------------------
            hud.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: px(44),
                    left: px(16),
                    max_width: vw(70),
                    flex_direction: FlexDirection::Column,
                    row_gap: px(8),
                    ..default()
                },
                ZIndex(Z_OVERLAY),
            ))
            .with_children(|feed| {
                for slot in &mut killfeed {
                    *slot = spawn_kill_row(feed, &font);
                }
            });

            // -- #deathbanner -----------------------------------------------
            hud.spawn(Node {
                position_type: PositionType::Absolute,
                left: px(0),
                right: px(0),
                top: percent(40),
                justify_content: JustifyContent::Center,
                ..default()
            })
            .with_children(|row| {
                death_banner = row
                    .spawn((
                        Node {
                            padding: UiRect::axes(px(48), px(12)),
                            border_radius: BorderRadius::all(px(16)),
                            ..default()
                        },
                        BackgroundColor(rgba(255, 0, 0, 0.1)),
                        Text::new("DESTROYED"),
                        // `clamp(36px, 8vw, 72px)`; 8vw exceeds 72 at any
                        // window wider than 900px, so this is the clamped value
                        // for every realistic size. `FontSize` has no clamp.
                        hud_font(&font, 72.0, 800),
                        TextColor(RED_BRIGHT),
                        LetterSpacing::Px(12.0),
                        Visibility::Hidden,
                    ))
                    .id();
            });

            // -- #hit-vignette ----------------------------------------------
            vignette = hud
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: px(0),
                        top: px(0),
                        width: percent(100),
                        height: percent(100),
                        ..default()
                    },
                    vignette_gradient(0.0),
                    ZIndex(Z_OVERLAY),
                    Visibility::Hidden,
                ))
                .id();

            // -- #matchhud --------------------------------------------------
            hud.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: px(0),
                    right: px(0),
                    top: px(16),
                    justify_content: JustifyContent::Center,
                    ..default()
                },
                ZIndex(Z_OVERLAY),
            ))
            .with_children(|row| {
                match_panel = row
                    .spawn((
                        Node {
                            flex_direction: FlexDirection::Row,
                            align_items: AlignItems::Center,
                            column_gap: px(24),
                            padding: UiRect::axes(px(32), px(12)),
                            border: UiRect::all(px(1)),
                            border_radius: BorderRadius::all(px(8)),
                            ..default()
                        },
                        // `--glass-bg` + `--glass-border`. `backdrop-filter:
                        // blur(16px)` has no `bevy_ui` equivalent, so the panel
                        // is flat glass rather than frosted.
                        BackgroundColor(GLASS_BG),
                        BorderColor::all(GLASS_BORDER),
                        BoxShadow::new(rgba(0, 0, 0, 0.6), px(0), px(12), px(0), px(40)),
                        Visibility::Hidden,
                    ))
                    .with_children(|panel| {
                        team0 = panel
                            .spawn(score_text(&font, BLUE, 32.0, Justify::Right))
                            .id();
                        clock = panel
                            .spawn((
                                score_text(&font, GOLD, 72.0, Justify::Center),
                                LetterSpacing::Px(2.0),
                            ))
                            .id();
                        team1 = panel
                            .spawn(score_text(&font, RED, 32.0, Justify::Left))
                            .id();
                    })
                    .id();
            });

            // -- #missile-lock-warning --------------------------------------
            hud.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: px(0),
                    right: px(0),
                    top: px(120),
                    justify_content: JustifyContent::Center,
                    ..default()
                },
                ZIndex(Z_WARNING),
            ))
            .with_children(|row| {
                lock_warning = row
                    .spawn((
                        Node {
                            padding: UiRect::axes(px(32), px(8)),
                            border_radius: BorderRadius::all(px(8)),
                            ..default()
                        },
                        BackgroundColor(rgba(255, 0, 0, 0.1)),
                        Text::new(LOCK_WARNING_TEXT),
                        hud_font(&font, 20.0, 800),
                        TextColor(RED_BRIGHT),
                        LetterSpacing::Px(8.0),
                        Visibility::Hidden,
                    ))
                    .id();
            });
        })
        .id();

    commands.insert_resource(HudNodes {
        root,
        health_fill,
        health_text,
        boost_fill,
        heat_frame,
        heat_fill,
        charge_frame,
        charge_fill,
        missile_pips,
        flare_pips,
        reticle_anchor,
        reticle,
        reticle_ticks,
        lock_warning,
        vignette,
        death_banner,
        match_panel,
        team0,
        team1,
        clock,
        markers,
        killfeed,
    });
}

/// Builds one target slot: a bracketed box with a label, and a lead ring.
///
/// Both start hidden and at the tree's origin. Neither is ever rebuilt; the
/// only thing that happens to them afterwards is a translation, a visibility
/// flag, and — when the target itself changes — a string and four colours.
fn spawn_marker(hud: &mut ChildSpawnerCommands, font: &HudFont) -> MarkerNodes {
    let mut corners = [Entity::PLACEHOLDER; 4];
    let mut label = Entity::PLACEHOLDER;

    // `.target-box`'s eight stacked linear gradients draw a 2px, 14px-long arm
    // along each end of each edge — which is four L-shaped corner brackets.
    // `bevy_ui` has no multi-stop background layers, so the brackets are four
    // 14x14 nodes wearing two borders each. Same pixels, and it means the
    // "which target is the assist on" state is four colour writes rather than
    // an eight-layer gradient string.
    let boxes = hud
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(0),
                top: px(0),
                width: px(BOX_SIZE),
                height: px(BOX_SIZE),
                ..default()
            },
            // `margin: -32px 0 0 -32px` is folded into the translation
            // `sync_world_markers` writes, so there is one source of position.
            UiTransform::IDENTITY,
            Visibility::Hidden,
        ))
        .with_children(|b| {
            // (left, right, top, bottom) insets and which two borders to draw,
            // going top-left, top-right, bottom-left, bottom-right.
            let arms = [
                (true, false, true, false),
                (false, true, true, false),
                (true, false, false, true),
                (false, true, false, true),
            ];
            for (slot, (l, r, t, bot)) in corners.iter_mut().zip(arms) {
                let inset = |on: bool| if on { px(0) } else { Val::Auto };
                let stroke = |on: bool| if on { px(BOX_STROKE) } else { px(0) };
                *slot = b
                    .spawn((
                        Node {
                            position_type: PositionType::Absolute,
                            left: inset(l),
                            right: inset(r),
                            top: inset(t),
                            bottom: inset(bot),
                            width: px(BOX_CORNER),
                            height: px(BOX_CORNER),
                            border: UiRect {
                                left: stroke(l),
                                right: stroke(r),
                                top: stroke(t),
                                bottom: stroke(bot),
                            },
                            ..default()
                        },
                        BorderColor::all(RED),
                        // `filter: drop-shadow(0 0 6px rgba(255,85,102,0.6))`.
                        BoxShadow::new(rgba(255, 85, 102, 0.6), px(0), px(0), px(0), px(6)),
                    ))
                    .id();
            }

            label = b
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: px(LABEL_LEFT),
                        top: px(0),
                        ..default()
                    },
                    Text::new(String::new()),
                    hud_font(font, 11.0, 600),
                    TextColor(RED),
                    LetterSpacing::Px(1.0),
                    TextLayout::new(Justify::Left, LineBreak::NoWrap),
                    TextShadow {
                        offset: Vec2::new(0.0, 1.0),
                        color: rgba(0, 0, 0, 0.9),
                    },
                ))
                .id();
        })
        .id();

    let (lead, lead_solid, lead_dashed) = lead_marker(hud);

    MarkerNodes {
        boxes,
        corners,
        label,
        lead,
        lead_solid,
        lead_dashed,
    }
}

/// `.lead-marker`, in both of its states.
///
/// The CSS is a 16px circle with a **dashed** 2px border that goes **solid and
/// filled** when the shot is lined up:
///
/// ```text
/// .lead-marker         { border: 2px dashed var(--color-red); border-radius: 50% }
/// .lead-marker.aligned { background: rgba(255,85,102,0.4); border-style: solid }
/// ```
///
/// `bevy_ui` has one border style and it is solid, so the scattered state is
/// drawn rather than styled: eight dots on the same 16px circle, which is what
/// a 2px dash pattern resolves to at that diameter anyway. Both rings are built
/// here and the swap is two `Visibility` writes — no archetype move, no
/// relayout, and no per-frame cost, since it only happens when the shot goes
/// from lined up to not.
///
/// Returns `(root, solid, scattered)`.
fn lead_marker(hud: &mut ChildSpawnerCommands) -> (Entity, Entity, Entity) {
    let mut solid = Entity::PLACEHOLDER;
    let mut dashed = Entity::PLACEHOLDER;

    let root = hud
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(0),
                top: px(0),
                width: px(LEAD_SIZE),
                height: px(LEAD_SIZE),
                ..default()
            },
            UiTransform::IDENTITY,
            Visibility::Hidden,
        ))
        .with_children(|m| {
            solid = m
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: px(0),
                        top: px(0),
                        width: percent(100),
                        height: percent(100),
                        border: UiRect::all(px(2)),
                        border_radius: BorderRadius::all(percent(50)),
                        ..default()
                    },
                    BorderColor::all(RED),
                    BackgroundColor(rgba(255, 85, 102, 0.4)),
                    BoxShadow::new(rgba(255, 85, 102, 0.5), px(0), px(0), px(0), px(10)),
                    Visibility::Hidden,
                ))
                .id();

            dashed = m
                .spawn(Node {
                    position_type: PositionType::Absolute,
                    left: px(0),
                    top: px(0),
                    width: percent(100),
                    height: percent(100),
                    ..default()
                })
                .with_children(|ring| {
                    let r = LEAD_SIZE / 2.0 - 1.0;
                    for i in 0..LEAD_DASHES {
                        #[expect(
                            clippy::cast_precision_loss,
                            reason = "eight, as a float, is eight"
                        )]
                        let a = std::f32::consts::TAU * i as f32 / LEAD_DASHES as f32;
                        ring.spawn((
                            Node {
                                position_type: PositionType::Absolute,
                                left: px(LEAD_SIZE / 2.0 + r * a.cos() - LEAD_DASH / 2.0),
                                top: px(LEAD_SIZE / 2.0 + r * a.sin() - LEAD_DASH / 2.0),
                                width: px(LEAD_DASH),
                                height: px(LEAD_DASH),
                                border_radius: BorderRadius::all(percent(50)),
                                ..default()
                            },
                            BackgroundColor(RED),
                        ));
                    }
                })
                .id();
        })
        .id();

    (root, solid, dashed)
}

/// One `.kf-entry`: killer, arrow, victim, on a glass slab with a blue spine.
fn spawn_kill_row(feed: &mut ChildSpawnerCommands, font: &HudFont) -> KillRowNodes {
    let mut killer = Entity::PLACEHOLDER;
    let mut victim = Entity::PLACEHOLDER;

    let row = feed
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: px(10),
                padding: UiRect::axes(px(16), px(8)),
                // `border: 1px solid ...; border-left: 4px solid --color-blue`.
                border: UiRect {
                    left: px(4),
                    right: px(1),
                    top: px(1),
                    bottom: px(1),
                },
                border_radius: BorderRadius::all(px(8)),
                ..default()
            },
            BackgroundColor(KF_BG),
            BorderColor {
                left: BLUE,
                right: GLASS_BORDER,
                top: GLASS_BORDER,
                bottom: GLASS_BORDER,
            },
            BoxShadow::new(rgba(0, 0, 0, 0.5), px(0), px(8), px(0), px(24)),
            // `animation: kf-in` has no `bevy_ui` equivalent; the row lands on
            // its end state, which is the same rule the rest of this module
            // follows for CSS animations.
            Visibility::Hidden,
        ))
        .with_children(|r| {
            killer = r.spawn(kill_text(font, BLUE)).id();
            r.spawn((
                Text::new(KF_ARROW.to_owned()),
                hud_font(font, 10.0, 600),
                TextColor(KF_ICON),
            ));
            victim = r.spawn(kill_text(font, KF_VICTIM)).id();
        })
        .id();

    KillRowNodes {
        row,
        killer,
        victim,
    }
}

/// A `.kf-killer` / `.kf-victim` cell. `text-transform: uppercase` is applied
/// when the string is written, since `bevy_text` has no such property.
fn kill_text(font: &HudFont, colour: Color) -> impl Bundle {
    (
        Text::new(String::new()),
        hud_font(font, 11.0, 600),
        TextColor(colour),
        LetterSpacing::Px(1.0),
        TextLayout::new(Justify::Left, LineBreak::NoWrap),
    )
}

// --- tree helpers ----------------------------------------------------------

/// A full-width row pinned `bottom` pixels off the floor, centring its child.
///
/// CSS centres these with `left: 50%; transform: translateX(-50%)`, which does
/// not survive `width: min(400px, 90vw)` cleanly — the -50% is of the resolved
/// width. A centring row gets the same result for any width and costs one node
/// that is never written to again.
fn centred_row(bottom: f32) -> Node {
    Node {
        position_type: PositionType::Absolute,
        left: px(0),
        right: px(0),
        bottom: px(bottom),
        justify_content: JustifyContent::Center,
        ..default()
    }
}

/// The well of a bar: `min(400px, 90vw)` wide, rounded, clipping its fill.
fn bar_frame(height: f32, well: Color, border: Color, border_px: f32) -> impl Bundle {
    (
        Node {
            width: px(BAR_WIDTH),
            max_width: vw(90),
            height: px(height),
            border: UiRect::all(px(border_px)),
            border_radius: BorderRadius::all(px(4)),
            // `overflow: hidden`, which is what lets the fill be a plain
            // percentage-width child with rounded corners.
            overflow: Overflow::clip(),
            ..default()
        },
        BackgroundColor(well),
        BorderColor::all(border),
    )
}

/// A bar's fill, full width to start with — as `#healthbar-fill`,
/// `#boostbar-fill` and `#heatbar-fill` all are.
fn fill_node() -> Node {
    Node {
        width: percent(100),
        height: percent(100),
        ..default()
    }
}

/// `box-shadow: 0 4px 12px rgba(0,0,0,0.5)`, shared by `.meterbar` and
/// `#chargebar`.
fn meter_shadow() -> BoxShadow {
    BoxShadow::new(rgba(0, 0, 0, 0.5), px(0), px(4), px(0), px(12))
}

/// The box `.meterbar-label` sits in: 12px from the left edge, full height, its
/// child centred against the cross axis.
///
/// The label has to be a *child* of this rather than living on it. A `Node`
/// that carries `Text` lays its glyphs out at its own origin, and its
/// `align_items` / `justify_content` govern its children — of which a text node
/// has none. Putting both on one entity silently top-left-aligns the text,
/// which is a mistake that looks fine on a 12px bar and obvious on a 24px one.
fn meter_label_slot() -> Node {
    Node {
        position_type: PositionType::Absolute,
        left: px(12),
        top: px(0),
        bottom: px(0),
        align_items: AlignItems::Center,
        ..default()
    }
}

/// `.meterbar-label` — "BOOST", "GUN". Drawn over the fill.
fn meter_label(font: &HudFont, text: &str) -> impl Bundle {
    (
        Text::new(text.to_owned()),
        hud_font(font, 10.0, 800),
        TextColor(LABEL_WHITE),
        LetterSpacing::Px(2.0),
        TextShadow {
            offset: Vec2::new(0.0, 1.0),
            color: rgba(0, 0, 0, 0.9),
        },
    )
}

/// `.msl-label` / `.fla-label`.
fn pip_label(font: &HudFont, text: &str) -> impl Bundle {
    (
        Text::new(text.to_owned()),
        hud_font(font, 11.0, 800),
        TextColor(LABEL_WHITE),
        LetterSpacing::Px(2.0),
        TextShadow {
            offset: Vec2::new(0.0, 1.0),
            color: rgba(0, 0, 0, 0.8),
        },
    )
}

/// One missile or flare pip, in its full state.
///
/// The border is spawned at width 1 and colour `NONE` even though a full pip
/// has no border in the CSS: with `BoxSizing::BorderBox` a 1px border does not
/// change the pip's 16x12 footprint, so `.empty` can be reached by writing two
/// colours and never by changing the layout. Same reasoning for the shadow.
///
/// `transform: skew(-10deg)` is dropped — `UiTransform` has translation, scale
/// and rotation but no skew, and the pips are 16px wide.
fn pip(colour: Color) -> impl Bundle {
    (
        Node {
            width: px(PIP_WIDTH),
            height: px(PIP_HEIGHT),
            border: UiRect::all(px(1)),
            border_radius: BorderRadius::all(px(2)),
            ..default()
        },
        BackgroundColor(colour),
        BorderColor::all(Color::NONE),
        BoxShadow::new(colour.with_alpha(0.8), px(0), px(0), px(0), px(12)),
    )
}

/// A `#matchhud` cell: fixed minimum width, its own colour and alignment.
fn score_text(font: &HudFont, colour: Color, min_width: f32, justify: Justify) -> impl Bundle {
    (
        Node {
            min_width: px(min_width),
            ..default()
        },
        Text::new("0"),
        // `font-size: clamp(16px, 2.5vw, 22px)`; 2.5vw passes 22px at 880px
        // wide, so this is the clamped value in practice.
        hud_font(font, 22.0, 800),
        TextColor(colour),
        TextLayout::new(justify, LineBreak::NoWrap),
    )
}

// --- gradients -------------------------------------------------------------

/// `linear-gradient(90deg, from 0%, to 100%)`.
fn gradient_90(from: Color, to: Color) -> BackgroundGradient {
    BackgroundGradient::from(LinearGradient {
        angle: LinearGradient::TO_RIGHT,
        stops: vec![
            ColorStop::new(from, percent(0)),
            ColorStop::new(to, percent(100)),
        ],
        ..default()
    })
}

/// `main.js:1980`'s health gradient, at a given hue.
///
/// `linear-gradient(180deg, hsl(h,80%,60%) 0%, hsl(h,70%,38%) 100%)`. 180deg in
/// CSS is top-to-bottom, which is `LinearGradient::TO_BOTTOM`.
fn health_gradient(hue: u16) -> BackgroundGradient {
    let h = f32::from(hue);
    BackgroundGradient::from(LinearGradient {
        angle: LinearGradient::TO_BOTTOM,
        stops: vec![
            ColorStop::new(Color::hsl(h, 0.8, 0.6), percent(0)),
            ColorStop::new(Color::hsl(h, 0.7, 0.38), percent(100)),
        ],
        ..default()
    })
}

/// `#chargebar-fill`'s gradient, normal or `.overload`.
fn charge_gradient(overload: bool) -> BackgroundGradient {
    if overload {
        // `linear-gradient(90deg, #c0392b 0%, #e74c3c 100%)`.
        gradient_90(rgb(0xc0, 0x39, 0x2b), rgb(0xe7, 0x4c, 0x3c))
    } else {
        // `linear-gradient(90deg, #e67e22 0%, #f1c40f 100%)`.
        gradient_90(rgb(0xe6, 0x7e, 0x22), rgb(0xf1, 0xc4, 0x0f))
    }
}

/// `#hit-vignette`'s radial gradient at a given opacity.
///
/// The CSS is
///
/// ```text
/// background: radial-gradient(ellipse at center,
///     rgba(255,0,0,0) 40%, rgba(200,0,0,0.5) 80%, rgba(160,0,0,0.9) 110%);
/// mix-blend-mode: multiply;
/// ```
///
/// and the blend mode is doing most of the work. Multiply *only ever darkens*:
/// against a black backdrop the result is black, so what the player actually
/// sees is the edges of a dark space scene losing their green and blue and
/// going red-black. `bevy_ui` has no blend modes, and reproducing those stop
/// alphas with straight source-over compositing floods the whole screen with
/// flat red — which is not a dimmer version of the effect, it is the opposite
/// of it, since it *adds* light where multiply removes it.
///
/// So the colours here are deliberately not the CSS's. They are much darker
/// reds at lower alpha, chosen so that ordinary alpha compositing lands on the
/// same place multiply does over this scene: a red-black rim that darkens
/// towards the corners and leaves the centre clear.
///
/// **The stop positions are not the CSS's either, and that is the fix.** CSS's
/// bare `ellipse at center` is *farthest-corner*: the ellipse is grown until it
/// passes through the corner, so its `100%` is out past the screen and the
/// edges of the viewport sit at about 71%. `bevy_ui`'s `FarthestCorner`
/// resolves to `(half_width, half_height)` instead — an ellipse through the
/// *side midpoints* — so every stop lands a factor of 1.41 further in than the
/// CSS meant it to. Transcribing 40/80/110 against that put half the screen
/// past the second stop and both far corners past the third, which is why a
/// single hit washed the whole viewport flat red rather than darkening its rim.
///
/// The stops below are re-derived for Bevy's ellipse: nothing at all inside
/// 70%, the ramp entirely in the outer margin, and 100% *is* the edge of the
/// screen — the last stop only ever reaches the four corners, which sit at
/// 141%. The peak alpha is capped well short of opaque, because this effect
/// must never be able to hide what is shooting at you. Checked on screen at
/// the worst case the model can produce: a hit landing at 40 HP, which is the
/// full-strength frame of the flash and still leaves the centre clean.
fn vignette_gradient(alpha: f32) -> BackgroundGradient {
    BackgroundGradient::from(RadialGradient {
        shape: RadialGradientShape::FarthestCorner,
        position: UiPosition::CENTER,
        stops: vec![
            ColorStop::new(rgba(140, 0, 0, 0.0), percent(70)),
            ColorStop::new(rgba(120, 0, 0, 0.12 * alpha), percent(90)),
            ColorStop::new(rgba(96, 0, 0, 0.22 * alpha), percent(100)),
            ColorStop::new(rgba(70, 0, 0, 0.38 * alpha), percent(130)),
        ],
        ..default()
    })
}

// ---------------------------------------------------------------------------
// The diff
// ---------------------------------------------------------------------------

/// Every component type [`sync_hud`] writes, in one system parameter.
///
/// Bevy caps a system at sixteen parameters, and one query per writable
/// component plus the resources the diff reads is past it. Grouping the writes
/// is also the honest description of what they are: the tree, handed to the
/// diff — and because they are separate fields, two of them can still be
/// borrowed at once, which [`sync_pips`] needs.
#[derive(bevy::ecs::system::SystemParam)]
struct HudWrite<'w, 's> {
    node: Query<'w, 's, &'static mut Node>,
    vis: Query<'w, 's, &'static mut Visibility>,
    bg: Query<'w, 's, &'static mut BackgroundColor>,
    border: Query<'w, 's, &'static mut BorderColor>,
    grad: Query<'w, 's, &'static mut BackgroundGradient>,
    shadow: Query<'w, 's, &'static mut BoxShadow>,
    text: Query<'w, 's, &'static mut Text>,
    text_colour: Query<'w, 's, &'static mut TextColor>,
    xform: Query<'w, 's, &'static mut UiTransform>,
}

/// Writes the frame's HUD, and nothing that did not change.
///
/// The shape to keep: build the model, compare it whole, and return before
/// acquiring a single `Mut` if it matches. Everything after the early-out is
/// guarded by its own field comparison, so the cost of a frame is proportional
/// to what moved rather than to how many nodes exist.
fn sync_hud(
    frame: Res<SimFrame>,
    time: Res<Time>,
    view: Res<crate::cockpit::ViewMode>,
    lobby: Option<Res<crate::ui::LobbyOpen>>,
    nodes: Option<Res<HudNodes>>,
    roster: Res<Roster>,
    feed: Res<KillFeed>,
    lock: Res<TargetLock>,
    mut applied: ResMut<AppliedHud>,
    mut w: HudWrite,
) {
    let Some(nodes) = nodes else { return };

    // The lobby covers the screen, so the flight overlay stands down behind it
    // for the same reason it stands down in the cockpit: something else is
    // already the interface.
    let hidden = view.seated || lobby.is_some_and(|l| l.0);
    let next = model(&frame.0, time.elapsed_secs(), hidden, lock.0, &feed);
    let prev = applied.0;

    // The early-out. On a frame where nothing the player can see has changed —
    // the overwhelming majority of frames — this system ends here, having read
    // two resources and compared twenty integers. No `Mut` is taken, so no
    // component is flagged changed, so `bevy_ui` skips the node in layout and
    // the render world skips it in extraction.
    if prev == Some(next) {
        return;
    }
    applied.0 = Some(next);

    /// True if this is the first write, or if any named field moved.
    macro_rules! moved {
        ($($field:ident),+ $(,)?) => {
            prev.is_none_or(|p| $(p.$field != next.$field)||+)
        };
    }

    // -- master visibility --------------------------------------------------
    if moved!(present) {
        set_visible(&mut w.vis, nodes.root, next.present);
    }
    if !next.present {
        // Nothing below is meaningful without a ship, and leaving the tree
        // untouched means a HUD that is off costs one comparison a frame.
        return;
    }

    // -- #healthbar ---------------------------------------------------------
    if moved!(hp_mil) {
        set_width(&mut w.node, nodes.health_fill, next.hp_mil);
    }
    if moved!(hp_hue) {
        set(&mut w.grad, nodes.health_fill, health_gradient(next.hp_hue));
    }
    if moved!(hp) {
        set_text(&mut w.text, nodes.health_text, || {
            format!("{} / {MAX_HP}", next.hp)
        });
    }

    // -- #boostbar ----------------------------------------------------------
    if moved!(boost_mil) {
        set_width(&mut w.node, nodes.boost_fill, next.boost_mil);
    }

    // -- #heatbar -----------------------------------------------------------
    if moved!(heat_mil) {
        set_width(&mut w.node, nodes.heat_fill, next.heat_mil);
    }
    if moved!(overheated) {
        set(
            &mut w.border,
            nodes.heat_frame,
            BorderColor::all(if next.overheated {
                ALERT_BORDER
            } else {
                HEAT_BORDER
            }),
        );
    }
    if moved!(overheated, pulse) {
        set(
            &mut w.shadow,
            nodes.heat_frame,
            if next.overheated {
                alert_glow(next.pulse)
            } else {
                meter_shadow()
            },
        );
    }

    // -- #chargebar ---------------------------------------------------------
    if moved!(charge_mil) {
        set_width(&mut w.node, nodes.charge_fill, next.charge_mil);
    }
    if moved!(charge) {
        let overload = next.charge == ChargeState::Overload;
        set_visible(
            &mut w.vis,
            nodes.charge_frame,
            next.charge != ChargeState::Idle,
        );
        set(
            &mut w.border,
            nodes.charge_frame,
            BorderColor::all(match next.charge {
                ChargeState::Overload => ALERT_BORDER,
                ChargeState::Full => CHARGE_FULL_BORDER,
                _ => METER_BORDER,
            }),
        );
        set(&mut w.grad, nodes.charge_fill, charge_gradient(overload));
    }
    if moved!(charge, pulse) {
        set(
            &mut w.shadow,
            nodes.charge_frame,
            match next.charge {
                ChargeState::Overload => alert_glow(next.pulse),
                // `#chargebar.full { box-shadow: 0 0 16px rgba(255,217,122,0.6) }`.
                ChargeState::Full => {
                    BoxShadow::new(rgba(255, 217, 122, 0.6), px(0), px(0), px(0), px(16))
                }
                _ => meter_shadow(),
            },
        );
    }

    // -- pip rows -----------------------------------------------------------
    //
    // The pattern the DOM HUD got wrong, done right: each pip's own emptiness
    // is compared against its own previous emptiness, so firing one missile
    // writes one pip. Rewriting all four would still be cheap here — the point
    // is that the shape does not degrade as the row gets longer, which is
    // exactly how "8 writes per player" became 64.
    sync_pips(
        &mut w.bg,
        &mut w.border,
        &mut w.shadow,
        &nodes.missile_pips,
        prev.map(|p| p.missiles),
        next.missiles,
        MSL_PIP,
        MSL_PIP_EMPTY,
        MSL_PIP_EMPTY_BORDER,
    );
    sync_pips(
        &mut w.bg,
        &mut w.border,
        &mut w.shadow,
        &nodes.flare_pips,
        prev.map(|p| p.flares),
        next.flares,
        FLA_PIP,
        FLA_PIP_EMPTY,
        FLA_PIP_EMPTY_BORDER,
    );

    // -- #reticle -----------------------------------------------------------
    if moved!(reticle_locked) {
        let colour = if next.reticle_locked {
            RED_BRIGHT
        } else {
            RETICLE_CYAN
        };
        set(&mut w.border, nodes.reticle, BorderColor::all(colour));
        for tick in nodes.reticle_ticks {
            set(&mut w.bg, tick, BackgroundColor(colour));
        }
        set(
            &mut w.shadow,
            nodes.reticle,
            if next.reticle_locked {
                BoxShadow::new(RED_BRIGHT, px(0), px(0), px(0), px(12))
            } else {
                BoxShadow::new(rgba(102, 221, 255, 0.4), px(0), px(0), px(0), px(8))
            },
        );
        set(
            &mut w.xform,
            nodes.reticle,
            if next.reticle_locked {
                UiTransform::from_scale(Vec2::splat(1.2))
            } else {
                UiTransform::IDENTITY
            },
        );
    }

    // -- #missile-lock-warning ----------------------------------------------
    if moved!(lock_warning) {
        set_visible(&mut w.vis, nodes.lock_warning, next.lock_warning);
    }
    if moved!(lock_blink) {
        // `@keyframes msl-lock-blink { 0%,49% { opacity: 1 } 50%,100% { 0.1 } }`
        // — a square wave, so two writes per 250 ms rather than sixty, and none
        // at all when nothing has a lock on you.
        set(
            &mut w.text_colour,
            nodes.lock_warning,
            TextColor(RED_BRIGHT.with_alpha(if next.lock_blink { 1.0 } else { 0.1 })),
        );
    }

    // -- #hit-vignette ------------------------------------------------------
    if moved!(vignette) {
        set_visible(&mut w.vis, nodes.vignette, next.vignette > 0);
        if next.vignette > 0 {
            set(
                &mut w.grad,
                nodes.vignette,
                vignette_gradient(f32::from(next.vignette) / VIGNETTE_STEPS),
            );
        }
    }

    // -- #deathbanner -------------------------------------------------------
    if moved!(alive) {
        set_visible(&mut w.vis, nodes.death_banner, !next.alive);
    }

    // -- #matchhud ----------------------------------------------------------
    if moved!(match_on) {
        set_visible(&mut w.vis, nodes.match_panel, next.match_on);
    }
    if next.match_on {
        if moved!(team0) {
            set_text(&mut w.text, nodes.team0, || next.team0.to_string());
        }
        if moved!(team1) {
            set_text(&mut w.text, nodes.team1, || next.team1.to_string());
        }
        if moved!(clock) {
            set_text(&mut w.text, nodes.clock, || fmt_clock(next.clock));
        }
    }

    // -- #killfeed ----------------------------------------------------------
    //
    // Per row, not per feed: a kill scrolls every row down one place, but a row
    // whose `serial` did not change is the same row and keeps its strings. The
    // only rows rewritten are the new one at the top and whichever one fell off
    // the bottom.
    for (i, slot) in nodes.killfeed.iter().enumerate() {
        let row = next.kills[i];
        if prev.is_some_and(|p| p.kills[i] == row) {
            continue;
        }
        set_visible(&mut w.vis, slot.row, row.shown);
        if !row.shown {
            continue;
        }
        // `text-transform: uppercase`, which `bevy_text` has no property for.
        set_text(&mut w.text, slot.killer, || {
            roster.callsign(row.killer).to_uppercase()
        });
        set_text(&mut w.text, slot.victim, || {
            roster.callsign(row.victim).to_uppercase()
        });
        // `.kf-you` — the row is about you, on one side or the other.
        set(
            &mut w.text_colour,
            slot.killer,
            TextColor(if row.killer == LOCAL_ID { GOLD } else { BLUE }),
        );
        set(
            &mut w.text_colour,
            slot.victim,
            TextColor(if row.victim == LOCAL_ID {
                RED
            } else {
                KF_VICTIM
            }),
        );
    }
}

// ---------------------------------------------------------------------------
// The world-space layer
// ---------------------------------------------------------------------------

/// Records a kill for the feed. One tick's events, once.
///
/// `main.js:3252` and `:3305` both skip the entry when there is no killer —
/// flying into a rock is not a frag — and so does this. Boss hitboxes are
/// skipped as victims for the same reason they are skipped as targets: the
/// capital ship is twenty ships and destroying it would print twenty rows.
fn collect_kills(frame: Res<SimFrame>, mut feed: ResMut<KillFeed>) {
    for event in &frame.0.events {
        if let SimEvent::ShipDestroyed {
            id,
            killer: Some(killer),
            ..
        } = *event
        {
            if is_boss_hitbox(id) {
                continue;
            }
            feed.push(killer, id, frame.0.time);
        }
    }
}

/// One eligible target: who it is, and where it landed on screen.
#[derive(Clone, Copy)]
struct Contact {
    id: EntityId,
    hp: i32,
    /// Viewport position, in logical pixels, rounded to the pixel it will be
    /// drawn on. Rounding here is what makes a target that is holding still on
    /// screen cost zero writes.
    at: Vec2,
    /// How far away it is, in world units. Both the bracket cutoff and the
    /// reticle's own depth read this.
    range: f32,
    /// Pixels between [`Contact::at`] and the gun line at [`Contact::range`].
    /// [`f32::INFINITY`] for a contact too far away to engage, which is what
    /// keeps it out of the lock without a second test.
    align: f32,
    /// Inside [`ENGAGE_DIST`]: draws a bracket and a name, not just a ring.
    boxed: bool,
}

/// Whether a sphere blocks the view from `from` to `to`.
///
/// `raySphereDist` (`main.js:1080`) solved for the near root of the ray-sphere
/// quadratic and compared it against the target's distance. This asks the same
/// question as a point-segment distance, which needs no square root and no
/// discriminant: the segment is clipped to `0..=1` first, so a sphere behind
/// the viewer or beyond the target cannot block, and a target *inside* a rock
/// reads as blocked — the same resolution `aim_assist.rs` documents for the
/// identical case.
fn occluded_by(from: Vec3, to: Vec3, centre: Vec3, radius: f32) -> bool {
    let seg = to - from;
    let len_sq = seg.length_squared();
    if len_sq <= f32::EPSILON {
        return false;
    }
    let t = ((centre - from).dot(seg) / len_sq).clamp(0.0, 1.0);
    from.lerp(to, t).distance_squared(centre) < radius * radius
}

/// Draws the floating part of the HUD: the target brackets, their labels, the
/// lead rings, and the reticle's position.
///
/// # Where the numbers come from
///
/// Every rule here is `main.js:1843`–`:1931`, in order: skip the dead, skip
/// teammates, skip anything past [`MARKER_VISIBLE_DIST`], project, skip
/// anything behind the camera or off the edge, drop anything with a rock or the
/// moon in the way, then draw.
///
/// # Three deliberate differences from the JS
///
/// 1. **Occlusion is tested from the camera, not from the ship.**
///    `main.js:1876` casts from `ship.position`, because it is asking a
///    targeting question. The complaint this answers is a *visual* one —
///    brackets drawn over the front of the moon with the ship they belong to
///    behind it — and the camera is the only viewpoint that defines "in front
///    of". They are eleven units apart, so the two answers differ only for a
///    rock close enough to fill the screen anyway.
/// 2. **A contact past [`ENGAGE_DIST`] keeps its ring and loses its bracket.**
///    See that constant. The JS draws the full box out to 1500.
/// 3. **Exactly one ring is ever solid.** `main.js:1928` sets `.aligned` on
///    every target inside 22 pixels, so a pair flying in formation both fill
///    in and neither is "the" target. The solid ring is the lock, so it goes to
///    the single best-aligned contact and to nothing else.
///
/// # Cost
///
/// The occlusion test is the one piece of real per-frame arithmetic in this
/// module, and it is deliberately last: it runs only for contacts that already
/// survived the flag, team, range, projection and off-screen tests, so it is a
/// handful of targets against sixty rocks — a few hundred multiply-adds, no
/// allocation, no square roots, and nothing that touches the ECS. That is a
/// different order of thing from the forced layout this module exists to
/// delete, but it is not free, and it is the first thing to move behind a
/// `ShipFlags` bit if `sim` ever offers one.
///
/// # Poses
///
/// Both the ships and the camera are read from [`GlobalTransform`], never from
/// [`Frame`]: `SimFrame`'s own documentation is explicit that anything a marker
/// positions itself from wants the interpolated pose `scene.rs` writes, not the
/// last tick's `ShipView::pos`, or the marker judders at the tick rate while
/// the ship it is on does not. The frame is still read for identity, hit points
/// and flags, which are discrete and have no in-between value.
///
/// # Ordering, and the frame it costs
///
/// This has to run after `scene.rs` has interpolated the ships and after
/// `camera.rs` has settled the chase camera — which means after
/// `TransformSystems::Propagate`, and `bevy_ui`'s layout runs *before* that. So
/// a `UiTransform` written here is consumed by the next frame's layout, and the
/// markers trail the scene by one frame — a few pixels in a hard turn. Removing
/// it means running between the camera and `UiSystems::Layout`, which needs an
/// ordering handle on `camera.rs`'s `follow`; that system is private and
/// `camera.rs` is not this module's file.
#[expect(
    clippy::too_many_arguments,
    reason = "one query per component type written, plus the two transform \
              sources; splitting it would split the per-slot diff that makes \
              it cheap"
)]
fn sync_world_markers(
    frame: Res<SimFrame>,
    roster: Res<Roster>,
    setup: Res<crate::sim_bridge::MatchSetup>,
    nodes: Option<Res<HudNodes>>,
    mut applied: ResMut<AppliedMarkers>,
    mut lock: ResMut<TargetLock>,
    cameras: Query<(&Camera, &GlobalTransform, Option<&RenderLayers>), With<Camera3d>>,
    ships: Query<(&ShipRoot, &GlobalTransform)>,
    mut q_vis: Query<&mut Visibility>,
    mut q_xform: Query<&mut UiTransform>,
    mut q_text: Query<&mut Text>,
    mut q_border: Query<&mut BorderColor>,
) {
    let Some(nodes) = nodes else { return };

    // `ui.rs` puts a second `Camera3d` in the world for the menu's ship
    // preview. It renders to an image and is confined to its own layer, so the
    // camera that can see the match is the one that draws layer 0 — which is
    // also the exact question being asked, and survives `ui.rs` changing its
    // order or its target.
    let scene_layer = RenderLayers::layer(0);
    let camera = cameras
        .iter()
        .find(|(_, _, layers)| layers.is_none_or(|l: &RenderLayers| l.intersects(&scene_layer)));
    let Some((camera, cam_tf, _)) = camera else {
        hide_all(&mut applied, &mut q_vis, &nodes);
        lock.0 = false;
        return;
    };
    let Some(viewport) = camera.logical_viewport_size() else {
        hide_all(&mut applied, &mut q_vis, &nodes);
        lock.0 = false;
        return;
    };

    // The local ship, from the interpolated transform rather than the frame.
    let Some((_, me)) = ships.iter().find(|(root, _)| root.0 == LOCAL_ID) else {
        hide_all(&mut applied, &mut q_vis, &nodes);
        lock.0 = false;
        return;
    };

    // The gun line: `main.js:1830`'s muzzle and nose. The nose is local +Z,
    // which is `camera.rs`'s convention and `sim`'s. Where a point on it lands
    // on screen depends on how far down it sits — see [`AIM_RANGE`] — so the
    // line is carried around and sampled per target rather than reduced to one
    // point here.
    let muzzle = me.translation();
    let aim_fwd = me.rotation() * Vec3::Z;
    let centre = viewport / 2.0;
    let aim_at = |range: f32| {
        camera
            .world_to_viewport(cam_tf, muzzle + aim_fwd * range.max(AIM_RANGE_MIN))
            .ok()
    };

    // -- eligible targets ---------------------------------------------------
    //
    // Walked in `Frame::ships` order, which is `World::ships` order and is
    // stable across ticks — so a target keeps the same slot, and its label is
    // written once rather than every time the set changes shape.
    let my_team = frame
        .0
        .ships
        .iter()
        .find(|s| s.flags.contains(ShipFlags::LOCAL))
        .map_or(-1, |s| s.team);
    let assist = frame.0.hud.assist_target;

    // The occluders, straight off the frame plus the one piece of fixed world
    // geometry. `Frame` carries the asteroids' *mesh* size; the collision
    // radius is that scaled by `collision_radius_scale`, which is the same
    // conversion `sim::asteroids` does when it builds a rock, read from
    // `rules.rs` rather than written down twice.
    let rock_scale = Rules::DEFAULT.world.asteroid_field.collision_radius_scale as f32;
    // The moon is an obstacle on the space map and does not exist on terrain.
    // `World::obstacles` holds it, but that lives on `SimWorld`, which
    // `sim_bridge` documents as never read by a rendering system — so it comes
    // from the rules and the map, exactly as `scene.rs` builds its mesh.
    let moon = (setup.map == sim::world::MapKind::Space).then(|| {
        let p = Rules::DEFAULT.world.moon_pos;
        (
            Vec3::new(p.x as f32, p.y as f32, p.z as f32),
            Rules::DEFAULT.world.moon_radius as f32,
        )
    });
    let eye = cam_tf.translation();

    let mut contacts = [None; TARGET_POOL];
    let mut found = 0;
    for view in &frame.0.ships {
        if found == TARGET_POOL {
            break;
        }
        if view.flags.contains(ShipFlags::LOCAL)
            || !view.flags.contains(ShipFlags::ALIVE)
            || view.flags.contains(ShipFlags::BOSS_HITBOX)
        {
            continue;
        }
        // `main.js:1864`. `team` is `-1` for unassigned, and two unassigned
        // ships in a free-for-all are not on a team together — which is why
        // this is not a bare equality test.
        if my_team >= 0 && view.team == my_team {
            continue;
        }
        let Some((_, tf)) = ships.iter().find(|(root, _)| root.0 == view.id) else {
            continue;
        };
        let pos = tf.translation();
        let range = me.translation().distance(pos);
        if range > MARKER_VISIBLE_DIST {
            continue;
        }
        // Anything behind the camera fails here rather than projecting to a
        // mirrored point in front of it: `world_to_viewport` rejects an NDC z
        // outside the frustum, which is exactly the `projTmp.z > 1 || < -1`
        // test at `main.js:1910`.
        let Ok(at) = camera.world_to_viewport(cam_tf, pos) else {
            continue;
        };
        if at.x < -OFFSCREEN_MARGIN
            || at.y < -OFFSCREEN_MARGIN
            || at.x > viewport.x + OFFSCREEN_MARGIN
            || at.y > viewport.y + OFFSCREEN_MARGIN
        {
            continue;
        }
        // Last, because it is the only expensive test and everything above has
        // already thrown most candidates away. `main.js:1876`: asteroids first,
        // then the moon, and the first hit ends it.
        let blocked = frame
            .0
            .asteroids
            .iter()
            .any(|r| occluded_by(eye, pos, Vec3::from(r.pos), r.size * rock_scale))
            || moon.is_some_and(|(c, r)| occluded_by(eye, pos, c, r));
        if blocked {
            continue;
        }

        let at = at.round();
        let boxed = range <= ENGAGE_DIST;
        contacts[found] = Some(Contact {
            id: view.id,
            hp: view.hp,
            at,
            range,
            // Against the gun line sampled at *this* target's range, so the
            // test is the same whether the target is at 200 units or 900. Out
            // of engagement range there is no lock to be had, so the ring can
            // never fill in and the contact cannot hold the reticle.
            align: match aim_at(range) {
                Some(aim) if boxed => at.distance(aim),
                _ => f32::INFINITY,
            },
            boxed,
        });
        found += 1;
    }

    // `main.js:1927` — the *best* alignment among the targets actually on
    // screen, so one enemy behind you cannot hold the lock open for another.
    // Unlike the JS this also picks a single winner: the solid ring is the
    // lock, and two targets cannot both be it. Ties go to the earlier slot,
    // which is `Frame::ships` order — the same insertion-order tie-break
    // `aim_assist.rs` and `bot.rs` document.
    let best = contacts
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.map(|c| (i, c)))
        .min_by(|a, b| a.1.align.total_cmp(&b.1.align));
    let locked_on = best.filter(|(_, c)| c.align < ALIGN_PX).map(|(i, _)| i);
    lock.0 = locked_on.is_some();

    // -- the reticle --------------------------------------------------------
    //
    // Drawn at the range of whatever the nose is nearest to, which is the
    // closest thing available to `main.js:1832`'s raycast: as you swing onto a
    // target the reticle settles onto *it* rather than onto a point a kilometre
    // past it. With nothing in front of you it falls back to [`AIM_RANGE`], and
    // if even that fails to project the anchor is left where it was — which is
    // screen centre, the position it is laid out at.
    let aim = best
        .and_then(|(_, c)| aim_at(c.range))
        .or_else(|| aim_at(AIM_RANGE));
    if let Some(aim) = aim {
        let aim = aim.round();
        // The anchor is laid out screen-centred, so the translation is the
        // offset from centre.
        set(
            &mut q_xform,
            nodes.reticle_anchor,
            UiTransform::from_translation(Val2::px(aim.x - centre.x, aim.y - centre.y)),
        );
    }

    // -- the slots ----------------------------------------------------------
    for (i, ((slot, contact), was_slot)) in nodes
        .markers
        .iter()
        .zip(contacts)
        .zip(&mut applied.0)
        .enumerate()
    {
        let was = *was_slot;
        let Some(c) = contact else {
            // Nothing to track. An already-hidden slot is left completely
            // alone — no transform, no text, no visibility write.
            if was.shown {
                set_visible(&mut q_vis, slot.boxes, false);
                set_visible(&mut q_vis, slot.lead, false);
                *was_slot = MarkerModel::default();
            }
            continue;
        };

        // The position, and only the position, every frame. `.target-box` has
        // `margin: -32px 0 0 -32px` and `.lead-marker` `-8px 0 0 -8px`; both
        // are folded in here so the node's *centre* lands on the target. The
        // box is written only while it is up — a distant contact is a ring and
        // nothing else, and its bracket does not need moving.
        if c.boxed {
            set(
                &mut q_xform,
                slot.boxes,
                UiTransform::from_translation(Val2::px(
                    c.at.x - BOX_SIZE / 2.0,
                    c.at.y - BOX_SIZE / 2.0,
                )),
            );
        }
        set(
            &mut q_xform,
            slot.lead,
            UiTransform::from_translation(Val2::px(
                c.at.x - LEAD_SIZE / 2.0,
                c.at.y - LEAD_SIZE / 2.0,
            )),
        );

        let now = MarkerModel {
            shown: true,
            boxed: c.boxed,
            id: c.id,
            hp: c.hp,
            // The lock, not a threshold: exactly one slot can carry it.
            aligned: locked_on == Some(i),
            assisted: assist >= 0 && assist == c.id,
        };
        if was == now {
            continue;
        }
        *was_slot = now;

        // A slot that was hidden has stale everything, so a first frame writes
        // the lot; after that each of these is its own comparison.
        let fresh = !was.shown;
        if fresh {
            set_visible(&mut q_vis, slot.lead, true);
        }
        if fresh || was.boxed != now.boxed {
            set_visible(&mut q_vis, slot.boxes, now.boxed);
        }
        if now.boxed && (fresh || !was.boxed || was.id != now.id || was.hp != now.hp) {
            // `main.js:1921` — `${targetName}  HP ${r.hp}`, two spaces.
            set_text(&mut q_text, slot.label, || {
                format!("{}  HP {}", roster.callsign(c.id), c.hp)
            });
        }
        if fresh || was.aligned != now.aligned {
            set_visible(&mut q_vis, slot.lead_solid, now.aligned);
            set_visible(&mut q_vis, slot.lead_dashed, !now.aligned);
        }
        if now.boxed && (fresh || !was.boxed || was.assisted != now.assisted) {
            // The one thing `HudState::assist_target` is the right source for.
            // The 22-pixel test above says "your nose is on them"; this says
            // "the assist has picked them", which is a different fact and the
            // one that tells a player why their aim is being helped.
            let colour = if now.assisted { RED_BRIGHT } else { RED };
            for corner in slot.corners {
                set(&mut q_border, corner, BorderColor::all(colour));
            }
        }
    }
}

/// Hides every marker that is currently up, and nothing else.
///
/// The frames with no camera, no viewport or no local ship — the first few, and
/// every frame in the lobby. A slot that is already down costs one comparison.
fn hide_all(applied: &mut AppliedMarkers, q_vis: &mut Query<&mut Visibility>, nodes: &HudNodes) {
    for (slot, was) in nodes.markers.iter().zip(&mut applied.0) {
        if was.shown {
            set_visible(q_vis, slot.boxes, false);
            set_visible(q_vis, slot.lead, false);
            *was = MarkerModel::default();
        }
    }
}

/// Brings one pip row in line with the count remaining, writing only the pips
/// whose state actually flipped.
///
/// Long in the arguments because the two rows differ only in their palette, and
/// one parameterised function beats two copies of the loop.
fn sync_pips(
    q_bg: &mut Query<&mut BackgroundColor>,
    q_border: &mut Query<&mut BorderColor>,
    q_shadow: &mut Query<&mut BoxShadow>,
    pips: &[Entity],
    prev: Option<u8>,
    next: u8,
    full: Color,
    empty: Color,
    empty_border: Color,
) {
    for (i, &pip) in pips.iter().enumerate() {
        let i = u8::try_from(i).unwrap_or(u8::MAX);
        let is_empty = i >= next;
        // A pip whose emptiness is unchanged is skipped outright.
        if prev.is_some_and(|p| (i >= p) == is_empty) {
            continue;
        }
        set(
            q_bg,
            pip,
            BackgroundColor(if is_empty { empty } else { full }),
        );
        set(
            q_border,
            pip,
            BorderColor::all(if is_empty { empty_border } else { Color::NONE }),
        );
        set(
            q_shadow,
            pip,
            // `.empty { box-shadow: none }`. Kept as a zero-alpha shadow rather
            // than a removed component so the pip's archetype never moves.
            BoxShadow::new(
                if is_empty {
                    Color::NONE
                } else {
                    full.with_alpha(0.8)
                },
                px(0),
                px(0),
                px(0),
                px(12),
            ),
        );
    }
}

/// The red glow shared by `.overheated` and `.overload`, at a pulse step.
///
/// Both CSS animations run `box-shadow: 0 0 8px rgba(231,76,60,0.6)` to
/// `0 0 24px rgba(231,76,60,0.9)` and back over 0.6 s.
fn alert_glow(step: u8) -> BoxShadow {
    let t = f32::from(step) / f32::from(PULSE_STEPS);
    BoxShadow::new(
        rgba(231, 76, 60, 0.6 + 0.3 * t),
        px(0),
        px(0),
        px(0),
        px(8.0 + 16.0 * t),
    )
}

// --- write helpers ---------------------------------------------------------
//
// Each takes the `Mut` only inside the branch that already decided a write is
// needed. `set_if_neq` is a second line of defence rather than the mechanism:
// it stops a redundant write from flagging the component, but it has already
// cost the lookup by the time it runs, whereas the field comparisons in
// `sync_hud` cost nothing.

/// Writes a component, leaving change detection alone if the value matches.
fn set<T: Component<Mutability = bevy::ecs::component::Mutable> + PartialEq>(
    q: &mut Query<&mut T>,
    entity: Entity,
    value: T,
) {
    if let Ok(mut current) = q.get_mut(entity) {
        current.set_if_neq(value);
    }
}

/// Sets a fill's width from a per-mille model value.
fn set_width(q: &mut Query<&mut Node>, entity: Entity, mil: u16) {
    if let Ok(mut node) = q.get_mut(entity) {
        let width = percent(f32::from(mil) / 10.0);
        if node.width != width {
            node.width = width;
        }
    }
}

/// `display: none` in the markup, `Visibility` here — see `#chargebar`.
fn set_visible(q: &mut Query<&mut Visibility>, entity: Entity, visible: bool) {
    let want = if visible {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    if let Ok(mut vis) = q.get_mut(entity) {
        vis.set_if_neq(want);
    }
}

/// Replaces a text node's string. The closure defers the `format!` until the
/// node is known to exist and the value is known to have changed — the JS
/// builds its strings unconditionally, every frame.
fn set_text(q: &mut Query<&mut Text>, entity: Entity, value: impl FnOnce() -> String) {
    if let Ok(mut text) = q.get_mut(entity) {
        let value = value();
        if text.0 != value {
            text.0 = value;
        }
    }
}

// ---------------------------------------------------------------------------
// Gaps in `HudState`
// ---------------------------------------------------------------------------
//
// Reported rather than added, since `crates/sim` is not this module's to edit.
// None of them blocks the HUD; each costs a small fidelity compromise, noted at
// the site that makes it.
//
// - **No `match_active`.** `World::match_state` has an `active: bool` that says
//   whether the clock and team kills apply at all — false in trials, campaign
//   and the tutorial — and it is not copied into `HudState`. `#matchhud` is
//   gated on `match_timer > 0.0` instead, which is right for a running
//   deathmatch and wrong for a paused or unstarted one.
// - **No braking flag.** `main.js:1336` shows `#chargebar` while `braking ||
//   brakeCharge > 0`. `HudState` has `charge01` but not the brake input, so the
//   bar appears a frame late, on the first non-zero charge.
// - **No line-of-sight query, and no world geometry.** A marker must not draw
//   through the moon, so [`sync_world_markers`] runs its own ray-sphere sweep
//   per target per frame. The asteroids come off `Frame`, but their *collision*
//   radius does not — `RockView` carries the mesh size and the scale factor has
//   to be reapplied from `rules.rs` — and the moon is not on `Frame` at all, so
//   it is rebuilt from `WorldRules` plus `MatchSetup::map`, which is a second
//   copy of a fact `World::obstacles` already holds. Either an occluded bit on
//   `ShipView`, set once per tick where `aim_assist::line_of_sight_blocked` is
//   already sweeping, or `World::obstacles` on the frame, removes both the
//   duplication and the per-frame arithmetic.
// - **`assist_target` is only an id.** `AimAssistState` computes an intercept
//   point — where a bullet fired now would actually meet the target — and
//   `HudState` keeps only the target's id. `.lead-marker` is therefore drawn on
//   the target rather than on the lead point, which is what `main.js:1922`
//   does too (`lx = sx, ly = sy`), so nothing is lost against the JS; but the
//   intercept point is strictly better information and it already exists.
// - **No scene raycast.** `main.js:1832` puts the reticle on whatever the gun
//   line actually hits. [`AIM_RANGE`] pins it at maximum range instead, which
//   differs by about a degree of parallax. A hit distance on `HudState`, or a
//   ray query on `sim`, would remove the approximation.
//
// Separately, and not a `HudState` problem: `sim_bridge::tick` does not yet
// populate `overcharge01` or `missile_lock_warning` — the projectile and
// missile systems it calls out as missing are what would fill them. The fields
// exist and are read here, so `.overload` and the lock warning light up the
// moment that lands, with no change to this file.

#[cfg(test)]
mod tests {
    use super::*;
    use sim::world::ShipView;

    /// [`model`] with the two out-of-frame inputs at rest — no target lock, no
    /// killfeed. The cases that care about either pass their own.
    fn model(frame: &Frame, time: f32, seated: bool) -> HudModel {
        super::model(frame, time, seated, false, &KillFeed::default())
    }

    /// A frame with one live local ship and a given HUD state.
    fn frame(hud: HudState) -> Frame {
        Frame {
            ships: vec![ShipView {
                id: 1,
                flags: ShipFlags::LOCAL.with(ShipFlags::ALIVE),
                ..Default::default()
            }],
            hud,
            ..Default::default()
        }
    }

    fn healthy() -> HudState {
        HudState {
            hp: 100,
            hp01: 1.0,
            ammo01: 1.0,
            boost01: 1.0,
            ..Default::default()
        }
    }

    /// The property the whole module is built around: a situation that has not
    /// changed produces a model that compares equal, so `sync_hud` writes
    /// nothing. Time advances between the two calls precisely because a
    /// time-driven phase that ticked unconditionally would break this.
    #[test]
    fn an_unchanged_frame_produces_an_unchanged_model() {
        let f = frame(healthy());
        let a = model(&f, 0.0, false);
        let b = model(&f, 1.0 / 60.0, false);
        let c = model(&f, 10.0, false);
        assert_eq!(a, b);
        assert_eq!(a, c);
    }

    /// Losing hit points moves the health fields and nothing else.
    #[test]
    fn damage_moves_only_the_health_fields() {
        let before = model(&frame(healthy()), 0.0, false);
        let after = model(
            &frame(HudState {
                hp: 60,
                hp01: 0.6,
                ..healthy()
            }),
            0.0,
            false,
        );

        assert_ne!(before.hp, after.hp);
        assert_ne!(before.hp_mil, after.hp_mil);
        assert_ne!(before.hp_hue, after.hp_hue);
        assert_eq!(
            HudModel {
                hp: before.hp,
                hp_mil: before.hp_mil,
                hp_hue: before.hp_hue,
                ..after
            },
            before,
            "damage should not disturb any other field"
        );
    }

    /// The health hue is `main.js:1979`'s: 120 (green) at full, 0 (red) at zero.
    #[test]
    fn the_health_hue_runs_green_to_red() {
        assert_eq!(model(&frame(healthy()), 0.0, false).hp_hue, 120);
        let dead = model(
            &frame(HudState {
                hp: 0,
                hp01: 0.0,
                ..healthy()
            }),
            0.0,
            false,
        );
        assert_eq!(dead.hp_hue, 0);
    }

    /// Bar widths quantise to the tenth of a percent the JS `toFixed(1)`s to,
    /// so sub-pixel drift does not produce a write.
    #[test]
    fn bar_widths_quantise_to_a_tenth_of_a_percent() {
        assert_eq!(mil(1.0), 1000);
        assert_eq!(mil(0.0), 0);
        assert_eq!(mil(0.5), 500);
        // Two boost readings a ten-thousandth apart are the same pixel.
        assert_eq!(mil(0.500_01), mil(0.500_02));
        // And one a thousandth apart is not.
        assert_ne!(mil(0.5), mil(0.501));
    }

    /// A gun with less ammo than its shot costs is "overheated". The threshold
    /// differs per gun and comes from `rules.rs`.
    #[test]
    fn overheat_follows_the_selected_gun() {
        let weapons = &Rules::DEFAULT.weapons;
        // Two rounds left: enough for a bolt, not for a beam.
        let ammo01 = (2.0 / weapons.max_ammo) as f32;

        let bullet = model(
            &frame(HudState {
                ammo01,
                gun_mode: GunMode::Bullet,
                ..healthy()
            }),
            0.0,
            false,
        );
        let beam = model(
            &frame(HudState {
                ammo01,
                gun_mode: GunMode::Beam,
                ..healthy()
            }),
            0.0,
            false,
        );

        assert!(!bullet.overheated);
        assert!(beam.overheated);
    }

    /// `#chargebar`'s three classes are strictly nested, and the overload
    /// threshold is the ratio of two rules rather than a number typed here.
    #[test]
    fn the_charge_bar_climbs_through_its_states() {
        let at = |charge01, overcharge01| {
            model(
                &frame(HudState {
                    charge01,
                    overcharge01,
                    ..healthy()
                }),
                0.0,
                false,
            )
            .charge
        };

        assert_eq!(at(0.0, 0.0), ChargeState::Idle);
        assert_eq!(at(0.4, 0.0), ChargeState::Active);
        assert_eq!(at(1.0, 0.0), ChargeState::Full);
        // `brake_overcharge_warn` / `brake_overcharge_damage_delay` = 1.0 / 2.0.
        assert_eq!(at(1.0, 0.49), ChargeState::Full);
        assert_eq!(at(1.0, 0.5), ChargeState::Overload);
    }

    /// Neither animated effect advances while its condition is false — the
    /// property that keeps the idle model constant.
    #[test]
    fn animations_are_still_while_their_condition_is_false() {
        let calm = frame(healthy());
        for t in [0.0, 0.05, 0.1, 0.3, 0.7, 5.0] {
            let m = model(&calm, t, false);
            assert_eq!(m.pulse, 0, "pulse must not advance at rest");
            assert!(!m.lock_blink, "blink must not advance at rest");
        }
    }

    /// ...and both do advance once it is true.
    #[test]
    fn the_lock_warning_blinks_when_locked() {
        let locked = frame(HudState {
            missile_lock_warning: true,
            ..healthy()
        });
        assert!(model(&locked, 0.0, false).lock_blink);
        assert!(!model(&locked, BLINK_HALF_PERIOD, false).lock_blink);
        assert!(model(&locked, BLINK_HALF_PERIOD * 2.0, false).lock_blink);
    }

    #[test]
    fn the_overload_glow_pulses_between_its_bounds() {
        let hot = frame(HudState {
            charge01: 1.0,
            overcharge01: 1.0,
            ..healthy()
        });
        assert_eq!(model(&hot, 0.0, false).pulse, 0);
        assert_eq!(model(&hot, PULSE_HALF_PERIOD, false).pulse, PULSE_STEPS);
        assert_eq!(model(&hot, PULSE_HALF_PERIOD * 2.0, false).pulse, 0);
    }

    /// The match scoreline follows `MatchState::active`, not a non-zero clock.
    ///
    /// `match_timer` carries the full match duration before the clock starts and
    /// in modes that have no clock at all, so testing it drew the multiplayer
    /// scoreline over a campaign mission.
    #[test]
    fn the_match_scoreline_hides_outside_a_timed_match() {
        let mut f = frame(healthy());
        f.ships[0].flags = ShipFlags::LOCAL.with(ShipFlags::ALIVE);

        f.hud.match_timer = 300.0;
        f.hud.match_active = false;
        assert!(!model(&f, 0.0, false).match_on, "campaign must not show it");

        f.hud.match_active = true;
        assert!(model(&f, 0.0, false).match_on, "a live match must");
    }

    /// Seated in the cockpit, the flat overlay stands down entirely — the 3D
    /// instrument panel is the HUD. Without this the bottom bars draw on top of
    /// the panel, which is what `main.js`'s `cockpit-view` body class prevents.
    #[test]
    fn the_overlay_stands_down_in_the_cockpit() {
        let mut f = frame(healthy());
        f.ships[0].flags = ShipFlags::LOCAL.with(ShipFlags::ALIVE);
        assert!(model(&f, 0.0, false).present, "third person draws the HUD");
        assert!(!model(&f, 0.0, true).present, "seated hides it");
    }

    /// A dead ship shows the banner; a frame with no local ship shows nothing.
    #[test]
    fn the_hud_hides_itself_when_there_is_no_ship() {
        assert!(!model(&Frame::new(), 0.0, false).present);

        let mut f = frame(healthy());
        f.ships[0].flags = ShipFlags::LOCAL;
        let dead = model(&f, 0.0, false);
        assert!(dead.present);
        assert!(!dead.alive);
    }

    /// The vignette rides the local ship's hit flash — the quantity
    /// `main.js:1942` copies into `camTel.hitFlash` — scaled by how much hull
    /// is already gone. No flash, no vignette, however badly hurt you are:
    /// this is a *hit* indicator, not a health bar.
    #[test]
    fn the_vignette_follows_the_hit_flash() {
        let mut f = frame(healthy());
        assert_eq!(model(&f, 0.0, false).vignette, 0);

        f.ships[0].hit_flash = 1.0;
        let fresh = model(&f, 0.0, false).vignette;
        assert_eq!(
            fresh,
            (VIGNETTE_HEALTHY_GAIN * VIGNETTE_STEPS).round() as u8
        );

        // Linear in the flash, at a fixed hull.
        f.ships[0].hit_flash = 0.5;
        assert_eq!(model(&f, 0.0, false).vignette, fresh / 2);
    }

    /// The stops of the vignette at a given opacity, as `(percent, alpha)`.
    fn vignette_stops(alpha: f32) -> Vec<(f32, f32)> {
        let BackgroundGradient(gradients) = vignette_gradient(alpha);
        let [Gradient::Radial(radial)] = &gradients[..] else {
            panic!("the vignette is one radial gradient");
        };
        radial
            .stops
            .iter()
            .map(|stop| {
                let Val::Percent(at) = stop.point else {
                    panic!("stops are authored in percent");
                };
                (at, stop.color.alpha())
            })
            .collect()
    }

    /// **`bevy_ui`'s `FarthestCorner` is not CSS's.**
    ///
    /// CSS grows the ellipse until it passes through the farthest *corner*, so
    /// `100%` is off-screen and the viewport edges sit at about 71%. Bevy
    /// resolves the same name to `(half_width, half_height)` — an ellipse
    /// through the side *midpoints* — so a stop transcribed from the CSS lands
    /// a factor of 1.41 further in than it was authored to. That is the whole
    /// reason one hit used to flood the screen, and it is invisible in the
    /// source, so it is pinned here.
    #[test]
    fn the_gradient_ellipse_runs_through_the_screen_edge_not_the_corner() {
        let size = Vec2::new(1920.0, 1080.0);
        let extent = RadialGradientShape::FarthestCorner.resolve(Vec2::ZERO, 1.0, size, size);
        assert_eq!(extent, size / 2.0, "100% is the edge midpoint");

        // So the corner is out at sqrt(2) in this ellipse's own metric, and
        // any stop beyond 100% only ever reaches the corners.
        let corner = (size / 2.0) / extent;
        assert!((corner.length() - std::f32::consts::SQRT_2).abs() < 1e-5);
    }

    /// It has to read as a rim. The centre of the screen is where the reticle,
    /// the target brackets and whatever is shooting at you all live, and the
    /// damage feedback must never be able to cover them.
    #[test]
    fn the_vignette_is_a_rim_and_leaves_the_centre_alone() {
        let stops = vignette_stops(1.0);
        let (first_at, first_alpha) = stops[0];

        assert!(first_alpha == 0.0, "the innermost stop must be clear");
        assert!(
            first_at >= 55.0,
            "the clear centre only reaches {first_at}% of the way to the edge"
        );

        // Monotonic outward, and never remotely opaque even at full strength.
        for pair in stops.windows(2) {
            assert!(pair[1].0 > pair[0].0, "stops out of order");
            assert!(pair[1].1 >= pair[0].1, "the rim lightens outward");
        }
        let peak = stops.last().expect("stops").1;
        assert!(peak <= 0.5, "peak alpha {peak} is a wash, not a rim");

        // The edge of the screen — 100%, per the test above — is the darkest
        // thing the player sees outside the four corners.
        let at_edge = stops
            .iter()
            .find(|(at, _)| *at == 100.0)
            .expect("a stop on the screen edge")
            .1;
        assert!(at_edge <= 0.35, "the screen edge sits at {at_edge}");
    }

    /// The state the report came in about: badly hurt, and still able to see.
    #[test]
    fn a_hit_at_forty_hitpoints_is_still_a_rim() {
        let mut f = frame(HudState {
            hp: 40,
            hp01: 0.4,
            ..healthy()
        });
        f.ships[0].hit_flash = 1.0;
        let alpha = f32::from(model(&f, 0.0, false).vignette) / VIGNETTE_STEPS;

        // Louder than a scratch at full health, quieter than a killing blow.
        assert!((0.6..0.9).contains(&alpha), "severity at 40 HP: {alpha}");

        let stops = vignette_stops(alpha);
        assert_eq!(stops[0].1, 0.0, "the centre is untouched");
        assert!(
            stops.last().expect("stops").1 < 0.4,
            "the corners are still translucent"
        );
    }

    /// The departure from the JS: the same hit reads harder the closer the
    /// hull is to gone, and reaches full strength only at zero.
    #[test]
    fn the_vignette_bites_harder_as_the_hull_goes() {
        let hit = |hp01: f32| {
            let mut f = frame(HudState { hp01, ..healthy() });
            f.ships[0].hit_flash = 1.0;
            model(&f, 0.0, false).vignette
        };

        assert!(hit(0.4) > hit(1.0), "a hit at 40% hull is the louder one");
        assert!(hit(0.0) > hit(0.4));
        assert_eq!(hit(0.0), VIGNETTE_STEPS as u8, "a dead hull maxes it out");
        // Still a rim and not a wash: nothing here ever exceeds full opacity,
        // and `vignette_gradient` caps the darkest stop well under it.
        assert!(hit(0.0) <= VIGNETTE_STEPS as u8);
    }

    /// `fmtTime`: whole seconds, rounded up, zero-padded.
    #[test]
    fn the_clock_matches_the_js_format() {
        assert_eq!(fmt_clock(300), "5:00");
        assert_eq!(fmt_clock(59), "0:59");
        assert_eq!(fmt_clock(61), "1:01");
        assert_eq!(fmt_clock(0), "0:00");

        // The model ceils, as `Math.ceil` does, so 4:59.2 reads "5:00".
        let m = model(
            &frame(HudState {
                match_timer: 299.2,
                match_active: true,
                ..healthy()
            }),
            0.0,
            false,
        );
        assert_eq!(fmt_clock(m.clock), "5:00");
        assert!(m.match_on);
    }

    /// The pip rows are sized from `rules.rs`, not from the markup.
    #[test]
    fn the_pip_rows_match_the_rules() {
        assert_eq!(MISSILE_PIPS, 4);
        assert_eq!(FLARE_PIPS, 3);
        assert_eq!(MAX_HP, 100);
    }

    // --- the world-space layer ---------------------------------------------

    /// The reticle's lock comes from where things land on screen, not from the
    /// aim assist's 53-degree cone — which is wide enough that reading it would
    /// leave the reticle red for most of a match.
    #[test]
    fn the_reticle_locks_on_alignment_and_not_on_the_assist_cone() {
        let held = frame(HudState {
            assist_target: 7,
            ..healthy()
        });
        assert!(
            !super::model(&held, 0.0, false, false, &KillFeed::default()).reticle_locked,
            "the assist holding a target is not a lock"
        );
        assert!(
            super::model(&held, 0.0, false, true, &KillFeed::default()).reticle_locked,
            "being lined up on one is"
        );

        // And a corpse never locks, however well aimed.
        let mut dead = frame(healthy());
        dead.ships[0].flags = ShipFlags::LOCAL;
        assert!(!super::model(&dead, 0.0, false, true, &KillFeed::default()).reticle_locked);
    }

    /// A kill lands on top and scrolls the rest down, and the ones that did not
    /// move keep their serial — which is what stops `sync_hud` rewriting four
    /// rows of text every time someone dies.
    #[test]
    fn the_killfeed_scrolls_and_leaves_settled_rows_alone() {
        let mut feed = KillFeed::default();
        feed.push(2, 3, 0.0);
        feed.push(4, 5, 1.0);

        let rows = feed.model(1.0);
        assert_eq!((rows[0].killer, rows[0].victim), (4, 5), "newest on top");
        assert_eq!((rows[1].killer, rows[1].victim), (2, 3));
        assert!(rows[0].shown && rows[1].shown);
        assert!(!rows[2].shown, "a row that never held a kill is not drawn");

        // The older row kept its identity as it scrolled, so its text is not
        // rewritten.
        assert_eq!(rows[1].serial, 1);
        assert_eq!(rows[0].serial, 2);
    }

    /// Rows expire on the simulation's clock, and expiring is the only thing
    /// about a settled feed that ever changes.
    #[test]
    fn killfeed_rows_expire() {
        let mut feed = KillFeed::default();
        feed.push(2, 3, 10.0);

        assert!(feed.model(10.0)[0].shown);
        assert!(feed.model(10.0 + KILL_TTL - 0.01)[0].shown);
        assert!(!feed.model(10.0 + KILL_TTL)[0].shown);

        // Between pushes and expiry the model is constant, so the HUD's
        // early-out still holds with a feed on screen.
        assert_eq!(feed.model(11.0), feed.model(12.0));
    }

    /// Only the oldest row falls off when a sixth kill arrives.
    #[test]
    fn the_killfeed_holds_five() {
        let mut feed = KillFeed::default();
        for i in 0..7 {
            feed.push(2, 3 + i, f64::from(i));
        }
        let rows = feed.model(6.0);
        assert_eq!(rows.len(), KILLFEED_ROWS);
        assert_eq!(rows[0].victim, 9, "the last kill is on top");
        assert_eq!(rows[4].victim, 5, "and the sixth-oldest has gone");
    }

    /// The killfeed is part of the one model the HUD compares, so a kill is a
    /// change and a quiet feed is not.
    #[test]
    fn a_kill_moves_the_model_and_nothing_else_does() {
        let f = frame(healthy());
        let mut feed = KillFeed::default();

        let quiet = super::model(&f, 0.0, false, false, &feed);
        assert_eq!(quiet, super::model(&f, 5.0, false, false, &feed));

        feed.push(2, 3, 0.0);
        let loud = super::model(&f, 0.0, false, false, &feed);
        assert_ne!(quiet, loud);
        assert_eq!(
            HudModel {
                kills: quiet.kills,
                ..loud
            },
            quiet,
            "a killfeed row should not disturb anything else"
        );
    }

    /// A hidden slot is `Default`, and `Default` is not `shown` — which is what
    /// every "leave it alone" branch in `sync_world_markers` tests.
    #[test]
    fn a_marker_slot_starts_hidden() {
        let m = MarkerModel::default();
        assert!(!m.shown);
        assert_eq!(m.id, 0, "no ship has id 0; LOCAL_ID is 1");
        assert_ne!(m.id, LOCAL_ID);
    }

    /// The alignment threshold is the JS's, and it is the same number for the
    /// lead ring and for the reticle — `main.js:1928` and `:1929`.
    #[test]
    fn the_alignment_threshold_is_the_one_from_the_js() {
        assert_eq!(ALIGN_PX, 22.0);
        assert_eq!(OFFSCREEN_MARGIN, BOX_SIZE / 2.0);
        assert_eq!(MARKER_VISIBLE_DIST, 1500.0);
        assert_eq!(KILLFEED_ROWS, 5);
    }

    /// The bracket range is the aim assist's, read from the rules rather than
    /// typed here, and it is inside the range a marker appears at at all — so
    /// there is a band where a contact is a ring and nothing more.
    #[test]
    fn the_bracket_range_comes_from_the_rules() {
        assert_eq!(ENGAGE_DIST, Rules::DEFAULT.aim_assist.range as f32);
        // Otherwise nothing is ever ring-only and the band does not exist.
        const { assert!(ENGAGE_DIST < MARKER_VISIBLE_DIST) };
    }

    /// A sphere blocks only when it is genuinely between the two points.
    #[test]
    fn occlusion_only_counts_what_is_in_the_way() {
        let eye = Vec3::ZERO;
        let target = Vec3::new(0.0, 0.0, 400.0);

        assert!(
            occluded_by(eye, target, Vec3::new(0.0, 0.0, 200.0), 80.0),
            "a rock on the line blocks"
        );
        assert!(
            !occluded_by(eye, target, Vec3::new(0.0, 200.0, 200.0), 80.0),
            "one beside the line does not"
        );
        assert!(
            !occluded_by(eye, target, Vec3::new(0.0, 0.0, 600.0), 80.0),
            "one behind the target does not"
        );
        assert!(
            !occluded_by(eye, target, Vec3::new(0.0, 0.0, -200.0), 80.0),
            "and one behind the camera does not"
        );
        assert!(
            occluded_by(eye, target, target, 20.0),
            "a target inside a rock reads as blocked, as aim_assist resolves it"
        );
    }

    /// The moon really is big enough to matter: a target directly behind it is
    /// hidden, which is the report this rule exists for.
    #[test]
    fn the_moon_hides_what_is_behind_it() {
        let moon = Rules::DEFAULT.world.moon_pos;
        let moon = Vec3::new(moon.x as f32, moon.y as f32, moon.z as f32);
        let r = Rules::DEFAULT.world.moon_radius as f32;
        assert!(r > 0.0);

        // Camera one side, target the other, moon in the middle.
        let eye = moon - Vec3::Z * 500.0;
        let behind = moon + Vec3::Z * 300.0;
        assert!(occluded_by(eye, behind, moon, r));

        // Slid clear of the line, it is visible again.
        let beside = moon + Vec3::Z * 300.0 + Vec3::X * (r * 4.0);
        assert!(!occluded_by(eye, beside, moon, r));
    }

    /// A rock's collision radius is its mesh size scaled, and the scale is the
    /// one `sim::asteroids` used when it built the rock.
    #[test]
    fn rock_radii_come_back_off_the_rules() {
        let scale = Rules::DEFAULT.world.asteroid_field.collision_radius_scale;
        assert!(scale > 0.0 && scale <= 1.0, "a sanity range, not a guess");
        // A 20-unit rock occludes at its collision radius, not at zero and not
        // at some radius invented here.
        let r = 20.0 * scale as f32;
        let eye = Vec3::ZERO;
        let target = Vec3::new(0.0, 0.0, 400.0);
        let just_inside = Vec3::new(r * 0.9, 0.0, 200.0);
        let just_outside = Vec3::new(r * 1.1, 0.0, 200.0);
        assert!(occluded_by(eye, target, just_inside, r));
        assert!(!occluded_by(eye, target, just_outside, r));
    }

    /// The scattered ring's dashes sit on the circle the solid ring's border
    /// traces, so the two states are the same size and the swap does not jump.
    #[test]
    fn the_scattered_ring_matches_the_solid_one() {
        let r = LEAD_SIZE / 2.0 - 1.0;
        for i in 0..LEAD_DASHES {
            let a = std::f32::consts::TAU * i as f32 / LEAD_DASHES as f32;
            let x = LEAD_SIZE / 2.0 + r * a.cos();
            let y = LEAD_SIZE / 2.0 + r * a.sin();
            let from_centre =
                ((x - LEAD_SIZE / 2.0).powi(2) + (y - LEAD_SIZE / 2.0).powi(2)).sqrt();
            assert!(
                (from_centre - r).abs() < 1e-4,
                "dash {i} is off the ring at {from_centre}"
            );
        }
    }
}
