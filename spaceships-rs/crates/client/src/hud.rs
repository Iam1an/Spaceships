//! The in-game HUD: a combining-glass gun sight, in `bevy_ui`.
//!
//! Drives the same six meters, reticle, warnings, killfeed and match clock the
//! `#healthbar` / `#boostbar` / `#heatbar` / `#chargebar` / `#missilehud` /
//! `#flarehud` / `#reticle` / `#missile-lock-warning` / `#hit-vignette` /
//! `#deathbanner` / `#matchhud` block of `public/index.html` did, off
//! [`sim::world::HudState`] instead of closure variables in `main.js`.
//!
//! # It is no longer a port of the CSS
//!
//! It was, and that was the complaint: `ui.rs` and `cockpit.rs` are the same
//! aircraft — instrument screens `#05080b`, gauge wells `#0d151c`, phosphor
//! green, cyan and amber, with a test in `ui.rs` pinning the shared list — and
//! this file sat on top of them as a web page. `--glass-bg` panels with
//! `blur(16px)` behind them, 8px rounded rectangles, a
//! `linear-gradient(90deg, #2980b9, #3498db)` boost bar and an hsl green-to-red
//! health bar. Glassmorphism over avionics.
//!
//! So the palette is now [`crate::ui::palette`] — literally the same module the
//! menu draws from, which is `cockpit.rs`'s `Palette` — and the *drawing* is a
//! head-up display rather than a web UI:
//!
//! ```text
//!         ┌ 420        ╭───╮        100 ┐
//!         │ 400        │ ✛ │         90 │
//!         │ 380        ╰───╯         80 │
//!         │ SPD                    HULL │
//!
//!               ▁▃▅▇ GUN   ▁▃▅ BST
//!               MSL ▮▮▮▮    FLR ▮▮▮
//! ```
//!
//! Thin strokes, no fills, no panels, no rounded boxes: everything is a line on
//! the canopy. Two vertical tapes carry speed and hull, ladders that scroll past
//! a fixed index rather than bars that grow; the four bars become segment
//! stacks; every surviving readout is drawn in phosphor, amber for a caution and
//! red for a warning, in the one hue a real combining glass has.
//!
//! **The tapes adapt to the map.** An altitude tape is meaningless in space, so
//! the block under the hull tape reads [`sim::ship::terrain_height`] as `AGL` on
//! [`MapKind::Terrain`] — with the ground-proximity `PULL UP` that only makes
//! sense there — and range to the boresight contact as `RNG` on
//! [`MapKind::Space`]. Same two nodes, one caption apart. The terrain map has no
//! renderer yet; this is designed for it and does not wait on it.
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
//! # What the tapes cost, which is the reason they are tapes
//!
//! A ladder is **built once and never rewritten**. Every tick mark and every
//! number on both tapes exists from startup at a fixed offset, and reading a
//! new speed moves the whole ladder with a single [`UiTransform`] — no text
//! written, no node resized, no relayout. The scroll is quantised to whole
//! pixels, so a ship holding a speed writes nothing at all.
//!
//! The four bars became segment stacks for the same reason. A bar's width is a
//! continuous quantity and the old port quantised it to a tenth of a percent,
//! which still moves most frames under acceleration; ten segments quantise the
//! same reading to a tenth, so a meter that has not crossed a segment boundary
//! costs zero writes rather than one `Node` write and the layout pass behind it.
//!
//! # What it is not
//!
//! `bevy_ui` has no CSS transitions and no `mix-blend-mode`; where an effect
//! relied on those it lands on the end state rather than the animation, and the
//! deviations are noted at each site. It also has no *dashed* border, which is
//! what the lead marker uses to say "the assist has this target but your nose is
//! not on it" — see [`lead_marker`] for how that state is drawn instead.

use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;
use bevy::text::{FontSource, FontWeight, Justify, LetterSpacing};

use sim::rules::Rules;
use sim::world::{
    is_boss_hitbox, EntityId, Frame, GunMode, HudState, MapKind, ShipFlags, SimEvent,
};
use spaceships_sim as sim;

use crate::scene::ShipRoot;
use crate::sim_bridge::{Roster, SimFrame, SimSet, LOCAL_ID};
use crate::ui::palette as pal;

/// Wires the HUD in: one tree at startup, one diffing system per frame.
pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AppliedHud>()
            .init_resource::<AppliedMarkers>()
            .init_resource::<KillFeed>()
            .init_resource::<Boresight>()
            .init_resource::<MatchResult>()
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
            // `MatchEnded` is an event too, and for the same reason it is read
            // per tick rather than per frame — read in `Update` a slow frame
            // would miss the only tick it is ever published on.
            .add_systems(FixedUpdate, (collect_kills, watch_match_end).after(SimSet))
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

/// `#rrggbb` at an alpha, for the two colours below that are not a swatch.
const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
    Color::srgba(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a)
}

/// The glass's own hue: everything not calling for attention is drawn in this.
///
/// `Swatch::blip_friendly`, which is `cockpit.rs`'s phosphor and the colour its
/// scope, its gauges and its captions are all built on.
const PHOSPHOR: Color = pal::PHOSPHOR;
/// A caution — hull below [`HULL_CAUTION`], a full brake charge, an empty tank.
/// `ADMIN_PROFILE::accent`.
const AMBER: Color = pal::AMBER;
/// A warning: hull below [`HULL_WARNING`], an overheated gun, an overload.
/// `Swatch::red`.
const WARN: Color = pal::RED;
/// A hostile contact. The same `Swatch::blip_hostile` the cockpit scope paints
/// an enemy blip with, so a bracket on the glass and a blip on the panel agree.
const HOSTILE: Color = pal::RED;
/// Ordnance aboard: the missile pips. `Swatch::orange` — a store you have is
/// not a caution, but it is not the glass's own hue either.
const ORDNANCE: Color = pal::ORANGE;
/// The friendly half of the scoreline. `DEFAULT_PROFILE::accent`.
const FRIENDLY: Color = pal::CYAN;

/// A legend — `SPD`, `HULL`, `GUN`, `MSL`. `Palette::caption`, dimmed.
fn legend() -> Color {
    pal::rgba(pal::CAPTION, 0.72)
}

/// The dimmest thing on the glass: an unlit segment, the far end of a tape.
///
/// A real gun sight simply does not draw an unlit segment, but then the scale
/// cannot be read at a glance — so the unlit half is the legend colour taken
/// down until it reads as structure rather than as information.
fn unlit() -> Color {
    pal::rgba(pal::CAPTION, 0.2)
}

/// A hairline: the tape spines, their end caps, the scoreline's dividers.
fn hairline() -> Color {
    PHOSPHOR.with_alpha(0.45)
}

/// Hull fraction below which the tape turns amber.
const HULL_CAUTION: f32 = 0.5;
/// ...and below which it turns red.
const HULL_WARNING: f32 = 0.25;

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

/// Height of a tape's window — how much ladder is on the glass at once.
const TAPE_H: f32 = 168.0;
/// Pixels per unit on both tapes. Two is what puts a major graduation 40 px
/// apart on the speed tape and the same 40 px apart on the hull tape, so the
/// two ladders read at one rate and the eye does not have to rescale between
/// them.
const TAPE_PPU: f32 = 2.0;
/// Width of the ladder window: long tick, gap, three digits.
const TAPE_W: f32 = 42.0;
/// Width of the current-value readout beside the index.
const TAPE_VALUE_W: f32 = 52.0;
/// Air either side of that readout.
const TAPE_VALUE_GAP: f32 = 5.0;
/// Width of a whole tape row: readout, its air, the spine, the ladder.
const TAPE_ROW_W: f32 = TAPE_VALUE_W + TAPE_VALUE_GAP * 2.0 + 1.0 + TAPE_W;
/// Length of a major graduation, and of a minor one.
const TICK_MAJOR: f32 = 9.0;
const TICK_MINOR: f32 = 5.0;
/// Height of one ladder row. Only has to clear the 10 px numerals.
const TAPE_ROW_H: f32 = 14.0;
/// The index caret: a stub through the spine at the reading line.
const CARET_W: f32 = 8.0;
const CARET_H: f32 = 2.0;
/// How far off screen centre a tape's inboard edge sits.
///
/// A percentage rather than a pixel count so the pair frames the boresight the
/// same way at any window size, which is what a combining glass does — the
/// symbology is fixed in the pilot's field of view, not in the panel.
const TAPE_INSET: f32 = 17.0;

/// Graduations on the speed ladder: a number every [`SPD_MAJOR`], a bare tick
/// every [`SPD_MINOR`].
const SPD_MAJOR: i32 = 20;
const SPD_MINOR: i32 = 10;
/// Graduations on the hull ladder.
const HULL_MAJOR: i32 = 20;
const HULL_MINOR: i32 = 5;

/// Top of the speed ladder, rounded up to a whole graduation.
///
/// Derived rather than typed: the fastest a ship can legitimately go is full
/// throttle, boosting, with a fully charged brake-release on top — the same
/// quantity `scene.rs::TOP_SPEED` computes for its teleport threshold — so a
/// rules change that raises the ceiling extends the tape instead of running the
/// needle off the end of it.
const SPD_TOP: i32 = {
    let top = Rules::DEFAULT.ship.max_throttle * Rules::DEFAULT.ship.boost_factor
        + Rules::DEFAULT.ship.brake_boost_bonus_max;
    (top as i32 / SPD_MAJOR + 1) * SPD_MAJOR
};

/// Lit segments in the `GUN` and `BST` stacks.
///
/// Ten, which is what quantises those two readings to a tenth and is why
/// neither writes anything on a frame that did not cross a boundary.
const METER_SEGS: usize = 10;
/// Ticks in the brake-charge strip. Finer than the meters because the whole
/// charge is over in [`sim::rules::ShipRules::brake_full_time`] seconds and a
/// coarse strip would jump from empty to full in three steps.
const CHARGE_SEGS: usize = 12;
/// Ticks in the `EMP` strip.
///
/// The same twelve as the brake, and the same uniform ticks, because it is the
/// same *kind* of readout — a thing filling up — and giving it a ramped stack
/// would put it in the family of `GUN` and `BST`, which are quantities you
/// spend. Twelve over a sixty-second charge is a tick every five seconds, which
/// is slow enough that a glance tells you roughly how long is left.
const EMP_SEGS: usize = 12;
/// Width of one meter segment, and the gap between two.
const SEG_W: f32 = 4.0;
const SEG_GAP: f32 = 2.0;
/// Shortest and tallest segment in a stack — the `▁▃▅▇` ramp.
const SEG_H_MIN: f32 = 4.0;
const SEG_H_MAX: f32 = 13.0;
/// The charge strip's ticks are uniform, so it cannot be mistaken for a meter.
const CHARGE_SEG_H: f32 = 6.0;

/// One missile or flare pip: a tall thin stroke, not a rounded chip.
const PIP_WIDTH: f32 = 4.0;
const PIP_HEIGHT: f32 = 13.0;

/// Distance off the floor of the four symbology rows, bottom up.
const PIP_ROW_BOTTOM: f32 = 46.0;
const METER_ROW_BOTTOM: f32 = 70.0;
const CHARGE_ROW_BOTTOM: f32 = 94.0;
/// The `EMP` strip, above the brake charge. Centred and on its own line rather
/// than beside `MSL`/`FLR`, because it is a charge and not a store: there is one
/// pulse, so a pip row would be a single square that is either on or off and
/// would say nothing about the fifty-nine seconds before it.
const EMP_ROW_BOTTOM: f32 = 114.0;
/// Gap between screen centre and the inboard edge of a paired row.
const ROW_SPLIT: f32 = 18.0;

/// Missile pips drawn. From the rules rather than from the markup's four
/// hardcoded `<span>`s — `rules.rs` is where a carried-count lives.
const MISSILE_PIPS: usize = Rules::DEFAULT.weapons.missile_max as usize;
/// Flare pips drawn.
const FLARE_PIPS: usize = Rules::DEFAULT.weapons.flare_max as usize;
/// Denominator of the health readout, `100`.
const MAX_HP: i32 = Rules::DEFAULT.ship.max_hp;

/// The lock warning. `⚠ MISSILE LOCK ⚠` in the markup; Orbitron has no U+26A0,
/// so the chevrons stand in rather than a pair of tofu boxes.
const LOCK_WARNING_TEXT: &str = ">> MISSILE LOCK <<";

/// The ground-proximity warning, terrain only.
const PULL_UP_TEXT: &str = "PULL UP";

/// What a blinded pilot is left with. See the legend's builder in
/// [`build_banners`] for why it is one word and not a countdown.
const EMP_BANNER_TEXT: &str = "-- EMP --";

/// Height above ground below which [`PULL_UP_TEXT`] lights, as a multiple of
/// the clearance that actually kills you.
///
/// `sim::ship::kill_floor` is `terrain_height + terrain_kill_clearance`, so the
/// warning is anchored to the real floor rather than to a number picked here
/// and can only ever move with it. Twelve gives about three quarters of a
/// second of notice at [`SPD_TOP`].
const GROUND_WARN_CLEARANCES: f64 = 12.0;

/// Quantisation of the altitude and range readout, in world units.
///
/// Both are continuous and neither is read to the unit — five is under a pixel
/// of digit movement and keeps a diving ship to a couple of writes a second
/// instead of one every frame, which is the discipline the module header
/// describes.
const ALT_STEP: f32 = 5.0;

/// `z-index: 4` — the vignette and the scoreline.
const Z_OVERLAY: i32 = 4;
/// `z-index: 5` — the lock warning.
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

/// The target bracket, tightened.
///
/// The CSS was 64px square with 14px arms. At 64 the bracket is wider than the
/// ship inside it at any range worth shooting at, and two enemies flying
/// together put their brackets through each other — which is what a skirmish
/// spawn looks like the moment it starts. 44 is the same drawing at the size
/// the thing it is bracketing actually occupies.
const BOX_SIZE: f32 = 44.0;
/// The length of one corner bracket arm, scaled with the box.
const BOX_CORNER: f32 = 10.0;
/// Bracket thickness.
const BOX_STROKE: f32 = 2.0;
/// Where the callsign hangs off the bracket, moved in with its edge.
const LABEL_LEFT: f32 = 50.0;

/// Diameter of the lead marker.
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

/// Diameter of the boresight's aiming circle.
///
/// Bigger than the old 16 px ring, because it is now the gun cross rather than
/// a web crosshair: the circle is the thing you fly a target into, and at 16 px
/// it was smaller than the ship it was meant to contain at gun range.
const SIGHT_D: f32 = 30.0;
/// Length of one of the four radial ticks outside the circle.
const SIGHT_TICK: f32 = 7.0;
/// The pipper: the actual aim point, at the centre of the circle.
const SIGHT_PIP: f32 = 3.0;

/// Rows in the killfeed. `main.js:2039` trims to five.
const KILLFEED_ROWS: usize = 5;
/// How long a killfeed row stays up.
///
/// `main.js:2040` starts a fade at 3.6 s and removes the row 0.42 s later.
/// `bevy_ui` has no CSS animations, so the row is drawn and then dropped at the
/// moment the JS finishes fading it.
const KILL_TTL: f64 = 4.0;

/// The `→` between killer and victim. Orbitron has no U+2192.
const KF_ARROW: &str = ">>";

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/// What the brake-charge strip can say.
///
/// `main.js:1336` toggles `active`, `full` and `overload` independently, but
/// they are strictly nested — `overload` implies `full` implies `active` — so
/// one enum says the same thing and makes "did the state change" a single
/// comparison rather than three.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum ChargeState {
    /// Not braking: the strip is not drawn.
    #[default]
    Idle,
    /// Charging.
    Active,
    /// Charged, and every extra tenth of a second is now overcharge.
    Full,
    /// Overcharged past the warning threshold; taking damage shortly.
    Overload,
}

/// How urgent a reading is. One enum for every readout that changes colour, so
/// "did the caution move" is one comparison wherever it is asked.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Alert {
    #[default]
    Ok,
    Caution,
    Warning,
}

impl Alert {
    /// The glass has three colours and this is where they are chosen.
    fn colour(self) -> Color {
        match self {
            Alert::Ok => PHOSPHOR,
            Alert::Caution => AMBER,
            Alert::Warning => WARN,
        }
    }
}

/// The block under the hull tape: `AGL` on terrain, `RNG` in space.
///
/// One structure for both because they are the same instrument — a distance you
/// are managing, with a caption saying which — and because keeping them as one
/// field means the map switch is a single comparison rather than two readouts
/// each guarding themselves against the other's map.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct AltBlock {
    /// Whether there is anything to say. False in space with no contact, which
    /// is the honest answer rather than a zero.
    shown: bool,
    /// True on [`MapKind::Terrain`], which is what picks the caption and is the
    /// *only* thing that allows [`AltBlock::warn`] to be set.
    agl: bool,
    /// The reading, in [`ALT_STEP`]s of a world unit.
    steps: u16,
    /// Ground proximity. Terrain only — there is no ground in space, and a
    /// warning about one would be a lie.
    warn: bool,
    /// The lit half of the ground-proximity blink. **Forced false when `warn`
    /// is false**, for the reason the module header gives.
    blink: bool,
}

/// Everything drawable about one frame of the HUD, quantised to what a pixel
/// can show.
///
/// The whole design rests on this being `Eq`: no floats, so two frames of an
/// unchanging situation compare equal and [`sync_hud`] can bail out before
/// touching a single component. Every field is either a discrete simulation
/// value (hit points, missiles remaining), a count of segments, or a whole
/// number of *pixels* — which is the finest a tape can move and still be seen.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct HudModel {
    /// Whether there is a local ship at all. False before the simulation has
    /// spawned one, and the whole tree is hidden.
    present: bool,
    /// Whether the local ship is alive; drives the death banner.
    alive: bool,

    /// Speed, whole units, for the readout beside the index.
    spd: u16,
    /// Where the speed ladder is scrolled to, in whole pixels off its own top.
    /// The one field the left tape moves on.
    spd_px: i16,
    /// The commanded-throttle bug, as a pixel offset down the tape window,
    /// already clamped into it.
    thr_px: i16,

    /// Hit points, for the readout beside the hull index.
    hp: i32,
    /// Where the hull ladder is scrolled to, in whole pixels.
    hull_px: i16,
    /// What colour the hull tape is drawn in.
    hull_alert: Alert,

    /// Lit segments in the `BST` stack.
    boost_seg: u8,
    /// Lit segments in the `GUN` stack.
    gun_seg: u8,
    /// `ammo < cost`, i.e. the selected gun cannot fire.
    overheated: bool,

    /// Lit ticks in the brake-charge strip.
    charge_seg: u8,
    /// What the strip is saying.
    charge: ChargeState,

    /// Missiles remaining.
    missiles: u8,
    /// Flares remaining.
    flares: u8,

    /// Lit ticks in the `EMP` strip.
    emp_seg: u8,
    /// Whether the strip is at full deflection, which recolours it. Derived from
    /// `emp_seg` and carried separately only so the *colour* change is its own
    /// comparison rather than being folded into the count.
    emp_ready: bool,
    /// Whether the glass is dark: an EMP has this ship's avionics.
    ///
    /// The one field that is not a readout. Everything above it is forced to its
    /// off value while this is set — see [`model`] — so a blind frame produces a
    /// *constant* model and the diff writes the blackout once rather than every
    /// frame of it.
    blind: bool,
    /// The lit half of the `EMP` legend's blink, on the same square wave as the
    /// missile-lock warning. **Forced false when `blind` is false.**
    blind_blink: bool,

    /// The altitude-or-range block under the hull tape.
    alt: AltBlock,

    /// Whether the boresight is on a target.
    reticle_locked: bool,
    /// Whether the missile-lock warning is shown at all.
    lock_warning: bool,
    /// The lit half of the 4 Hz lock blink. **Forced false when `lock_warning`
    /// is false**, so an unlocked HUD has a constant model.
    lock_blink: bool,
    /// The overheat / overload glow phase, quantised to [`PULSE_STEPS`].
    /// **Forced zero when nothing is pulsing.**
    pulse: u8,

    /// The hit vignette's opacity, in 64ths.
    vignette: u8,

    /// Whether the scoreline is shown.
    match_on: bool,
    /// Friendly kills.
    team0: u32,
    /// Hostile kills.
    team1: u32,
    /// Whole seconds on the clock, `Math.ceil` as `fmtTime` does.
    clock: u32,

    /// The killfeed, newest row first.
    kills: [KillRowModel; KILLFEED_ROWS],

    /// The finished match's outcome, or `None` mid-match.
    ///
    /// Outside the `present` early-out's protection on purpose — a match can
    /// end while the local ship is dead or gone, and the card still has to
    /// show.
    result: Option<Outcome>,

    /// Whether [`HudNodes::cockpit_hidden`] is hidden — true while seated.
    ///
    /// Phrased as *hidden* rather than *shown* so `Default` can stay derived:
    /// `HudModel::default()` is the no-local-ship model, where the whole tree
    /// is already hidden by `present`, and `false` there is the no-op.
    bars_hidden: bool,
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

impl HudModel {
    /// This model with the aircraft's own instruments switched off.
    ///
    /// The EMP's whole share of this file. `BACKLOG.md` §2 lists the cockpit,
    /// aim assist, the target boxes and the lock warning, and then asks about
    /// "HUD bars, if we want it to bite harder" — and the answer taken here is
    /// that **the whole glass goes**, on the grounds that it is one electrical
    /// system. A combining glass is projected by the same avionics that light
    /// the panel; leaving the airspeed tape and the hull ladder up while the
    /// cockpit behind them is pitch dark would say the damage is cosmetic, and
    /// it would leave the pilot with most of what they had.
    ///
    /// What survives, and the rule behind the list: **the airframe stops
    /// talking; the match does not.** A pulse is a thing done to one aircraft,
    /// and it has no business editing the scoreline, the kill feed, or the card
    /// that says the match is over — those are the referee, drawn on the same
    /// screen but not by this aeroplane. The hit vignette stays for a different
    /// reason: it is not an instrument at all, it is being hit, and a pilot who
    /// could no longer feel damage would be losing something §2 explicitly does
    /// not take. `DESTROYED` stays because a blackout must never be able to hide
    /// the fact that you are dead.
    ///
    /// Written as "default, plus the survivors" rather than as a list of things
    /// to zero, so a field added to [`HudModel`] later is dark during an EMP by
    /// construction. That is the safe direction to be wrong in: a new readout
    /// that should have gone out is a bug you find by adding it to this list, a
    /// new readout that stayed up is a bug nobody notices.
    fn unpowered(self) -> HudModel {
        HudModel {
            present: self.present,
            alive: self.alive,
            // Every element the cockpit stands down is also an element the pulse
            // kills, so this rides along rather than needing its own flag.
            bars_hidden: true,
            vignette: self.vignette,
            match_on: self.match_on,
            team0: self.team0,
            team1: self.team1,
            clock: self.clock,
            kills: self.kills,
            result: self.result,
            blind: self.blind,
            blind_blink: self.blind_blink,
            ..HudModel::default()
        }
    }
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
    /// The ring is solid rather than scattered. At most one slot has this set:
    /// it is *the* lock, not a threshold several targets can pass at once.
    aligned: bool,
    /// Whether aim assist is currently holding *this* target
    /// ([`HudState::assist_target`]), which brightens the bracket.
    assisted: bool,
}

/// What [`sync_world_markers`] last wrote to each slot.
#[derive(Resource, Default)]
struct AppliedMarkers([MarkerModel; TARGET_POOL]);

/// What the boresight is looking at this frame.
///
/// Written by [`sync_world_markers`], which is the only system that knows where
/// anything projects to, and read by [`sync_hud`] for the locked reticle and for
/// the `RNG` half of [`AltBlock`]. A resource rather than a return value because
/// the two run in different schedules; the lock therefore lights one frame after
/// the alignment, which no one can see and which is the same latency the marker
/// itself has.
#[derive(Resource)]
struct Boresight {
    /// Whether the player's aim is on a target.
    locked: bool,
    /// Range to the best-aligned contact, or [`f32::INFINITY`] with none.
    ///
    /// Infinity rather than `Option` so the resource stays `Copy`-cheap and the
    /// "no contact" case is one `is_finite` at the single site that reads it.
    range: f32,
}

impl Default for Boresight {
    fn default() -> Boresight {
        Boresight {
            locked: false,
            range: f32::INFINITY,
        }
    }
}

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
/// The ground-proximity warning flashes slower than the lock warning, because
/// it is telling you to do one thing rather than to look for something.
const PULL_UP_HALF_PERIOD: f32 = 0.2;
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
/// The inputs [`model`] needs that are not on the [`Frame`].
///
/// Grouped rather than passed one by one because there are now five of them and
/// a six-argument pure function is where a call site starts getting its
/// booleans the wrong way round.
#[derive(Clone, Copy)]
struct Env {
    /// The render clock, for the two square waves.
    time: f32,
    /// Whether the meters stand down: seated in the cockpit, or the lobby is up.
    seated: bool,
    /// Whether the boresight is on a target, which only
    /// [`sync_world_markers`] can answer — it is a question about *projection*.
    locked: bool,
    /// Range to that target, or [`f32::INFINITY`]. Same source, same reason.
    range: f32,
    /// Which map, which is what decides whether the block under the hull tape
    /// is an altimeter or a rangefinder.
    map: MapKind,
    /// [`crate::weapons::forced_hull`]: the screenshot hook that pins the hull.
    /// `None` in every real run.
    hull: Option<f32>,
}

impl Default for Env {
    fn default() -> Env {
        Env {
            time: 0.0,
            seated: false,
            locked: false,
            range: f32::INFINITY,
            map: MapKind::Space,
            hull: None,
        }
    }
}

fn model(frame: &Frame, env: Env, feed: &KillFeed, result: Option<Outcome>) -> HudModel {
    let Env {
        time,
        seated,
        locked,
        ..
    } = env;
    // Seated in the cockpit the 3D panel replaces the *bars*, and only the
    // bars. This used to return `HudModel::default()` and stand the whole
    // overlay down, which also took the reticle with it — so the cockpit had no
    // crosshair and there was no way to tell where the guns pointed.
    //
    // `index.html:865`–`:870` is the authority, and it is a short list:
    // `body.cockpit-view` hides `#healthbar`, `#chargebar`, `#boostbar`,
    // `#heatbar`, `#missilehud` and `#flarehud`. The reticle, the target boxes,
    // the lead marker, the missile-lock warning, the hit vignette, the kill
    // feed, the match clock and the death banner are all absent from it and all
    // stay up, because the instrument panel does not duplicate any of them.
    //
    // The values behind the hidden bars are zeroed rather than left live, which
    // is this model's usual discipline: a field that cannot be seen must not
    // vary, or the diff churns on invisible state.
    // `ShipFlags::LOCAL` is set by `sim_bridge::ship_view` for the ship whose
    // id matches `World::local_id`. Before the first tick there is no frame and
    // no ship, and the HUD stays hidden rather than drawing a dead one.
    let Some(me) = frame
        .ships
        .iter()
        .find(|s| s.flags.contains(ShipFlags::LOCAL))
    else {
        // The result card is the one thing that outlives the ship: a match can
        // end with the local pilot destroyed and awaiting respawn, and "you
        // lost" is exactly the moment you need to be told.
        return HudModel {
            result,
            ..HudModel::default()
        };
    };

    let hud: &HudState = &frame.hud;
    let alive = me.flags.contains(ShipFlags::ALIVE);

    // `SPACESHIPS_HULL` pins the hull so the damage states can be looked at.
    // Defined once, in `weapons.rs`, because the plume reads it too and a tape
    // that disagreed with the smoke coming off the wing would be worse than no
    // hook at all. `None` in every real run.
    let (hp, hp01) = match env.hull {
        Some(forced) => ((forced * MAX_HP as f32).round() as i32, forced),
        None => (hud.hp, hud.hp01.clamp(0.0, 1.0)),
    };
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

    // Both pulses live on bars the cockpit hides, so seated they must not run —
    // an advancing phase behind a hidden node is exactly the churn the model
    // exists to prevent.
    let pulsing = !seated && (overheated || charge == ChargeState::Overload);
    let lock_warning = hud.missile_lock_warning && alive;

    // The speed tape. `spd_px` is the whole reading: it is where the ladder is
    // scrolled to, so a ship holding a speed produces an identical model and
    // the tape is not touched at all.
    let speed = hud.speed.max(0.0).min(SPD_TOP as f32);
    let spd_px = tape_px(speed);
    let cmd = (hud.throttle01.clamp(0.0, 1.0) * Rules::DEFAULT.ship.max_throttle as f32)
        .min(SPD_TOP as f32);
    // Clamped into the window rather than allowed off the end of it, which is
    // what a real airspeed bug does: parked against the edge it still says
    // "commanded is above what you are doing".
    let thr_px = (f32::from((TAPE_H / 2.0) as i16 + spd_px - tape_px(cmd)))
        .clamp(CARET_H, TAPE_H - CARET_H) as i16;

    let hull_alert = if hp01 < HULL_WARNING {
        Alert::Warning
    } else if hp01 < HULL_CAUTION {
        Alert::Caution
    } else {
        Alert::Ok
    };

    // An EMP is a *power* question, so it is applied to the finished model
    // rather than threaded through every field below: see `HudModel::unpowered`.
    // The legend blinks on the missile-lock warning's square wave, because it is
    // the same kind of statement — a thing being done to you, right now — and
    // reusing the wave means the two can never beat against each other.
    let blind = hud.emp_blind > 0.0 && alive;
    let emp_seg = segments(hud.emp_charge01, EMP_SEGS);

    let model = HudModel {
        present: true,
        alive,

        bars_hidden: seated,
        result,

        // The speed tape is *not* on the cockpit's hidden list, and that is
        // deliberate rather than an oversight: `index.html:865`–`:870` hides six
        // elements and `#hud-stats`, which carried the speed readout, is not one
        // of them. A real aircraft shows airspeed on the glass and on the panel
        // both, and so does this.
        spd: speed.round() as u16,
        spd_px,
        thr_px,

        hp: if seated { 0 } else { hp.max(0) },
        hull_px: if seated {
            0
        } else {
            tape_px(hp.clamp(0, MAX_HP) as f32)
        },
        hull_alert: if seated { Alert::Ok } else { hull_alert },

        boost_seg: if seated {
            0
        } else {
            segments(hud.boost01, METER_SEGS)
        },
        gun_seg: if seated {
            0
        } else {
            segments(hud.ammo01, METER_SEGS)
        },
        overheated: overheated && !seated,

        charge_seg: if seated {
            0
        } else {
            segments(charge01, CHARGE_SEGS)
        },
        charge: if seated { ChargeState::Idle } else { charge },

        missiles: if seated { 0 } else { hud.missiles },
        flares: if seated { 0 } else { hud.flares },

        // Not zeroed while seated, for the same reason the speed tape and the
        // reticle are not: the instrument panel has no EMP gauge, so this is not
        // duplicating anything the pilot can already see from the seat.
        emp_seg,
        emp_ready: emp_seg as usize >= EMP_SEGS,
        blind,
        blind_blink: blind && phase(time, BLINK_HALF_PERIOD).is_multiple_of(2),

        alt: alt_block(me, env),

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
    };

    if blind {
        model.unpowered()
    } else {
        model
    }
}

/// A tape reading, in whole pixels down its own ladder.
///
/// This is the quantisation the two tapes rest on: a ladder that has not moved
/// by a whole pixel cannot look any different, so the model compares equal and
/// nothing is written. It is a strictly coarser filter than the old
/// tenth-of-a-percent bar width and a strictly better-founded one — a pixel is
/// a thing the display has, a per-mille is not.
fn tape_px(value: f32) -> i16 {
    (value.max(0.0) * TAPE_PPU).round() as i16
}

/// A `0..1` reading as a count of lit segments, rounded **up**.
///
/// Up rather than to nearest, because the bottom segment has to mean "there is
/// some left": a gun with four rounds in ninety is not empty, and a stack that
/// showed it as empty would be lying about the one fact the stack exists for.
fn segments(v01: f32, segs: usize) -> u8 {
    let n = (v01.clamp(0.0, 1.0) * segs as f32).ceil();
    n as u8
}

/// The block under the hull tape: height above ground, or range to target.
///
/// The map is the whole switch. On terrain this is a radar altimeter reading
/// [`sim::ship::terrain_height`] under the ship — the same height field the
/// simulation kills you against, so the number and the floor cannot disagree —
/// and it is the only configuration in which [`AltBlock::warn`] can be set. In
/// space there is no ground and an altimeter would be furniture, so the same two
/// nodes carry range to whatever the boresight is on, which is the other
/// distance a pilot manages and is already computed by [`sync_world_markers`].
fn alt_block(me: &sim::world::ShipView, env: Env) -> AltBlock {
    if env.map == MapKind::Terrain {
        let rules = &Rules::DEFAULT;
        let ground = sim::ship::terrain_height(f64::from(me.pos[0]), f64::from(me.pos[2]), rules);
        let agl = (f64::from(me.pos[1]) - ground).max(0.0);
        let warn = agl < rules.world.terrain_kill_clearance * GROUND_WARN_CLEARANCES;
        return AltBlock {
            shown: true,
            agl: true,
            steps: quantise(agl as f32),
            warn,
            // Forced still while the warning is off, for the reason the module
            // header gives: a phase that advanced regardless would make every
            // frame's model differ and delete the early-out.
            blink: warn && phase(env.time, PULL_UP_HALF_PERIOD).is_multiple_of(2),
        };
    }

    if !env.range.is_finite() {
        return AltBlock::default();
    }
    AltBlock {
        shown: true,
        agl: false,
        steps: quantise(env.range),
        warn: false,
        blink: false,
    }
}

/// A distance in [`ALT_STEP`]s, saturating rather than wrapping.
fn quantise(v: f32) -> u16 {
    (v / ALT_STEP).round().clamp(0.0, f32::from(u16::MAX)) as u16
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

    /// The six elements `body.cockpit-view` hides, in the order
    /// `index.html:865`–`:870` lists them: `#healthbar`, `#chargebar`,
    /// `#boostbar`, `#heatbar`, `#missilehud`, `#flarehud`.
    ///
    /// Held as their outer rows rather than as fills, because hiding is a
    /// property of the whole element. Everything *not* in this array — the
    /// reticle above all — stays up in the cockpit, which is what the JS does
    /// and what this port originally got wrong.
    cockpit_hidden: [Entity; 6],

    /// The speed tape. Not on the hidden list — see [`HudModel::spd`].
    spd: TapeNodes,
    /// The hull tape, which is `cockpit_hidden[0]`'s whole content.
    hull: TapeNodes,

    /// The `AGL`/`RNG` block: its own row, its caption, and its value.
    alt_row: Entity,
    alt_caption: Entity,
    alt_value: Entity,
    /// `PULL UP`, terrain only.
    pull_up: Entity,

    /// The `BST` stack, outboard segment first — index 0 is the *bottom* of the
    /// ramp, so `n` lit is a prefix of the array and the diff is a range.
    boost_segs: [Entity; METER_SEGS],
    /// The `GUN` stack, and its legend, which reddens when it cannot fire.
    gun_segs: [Entity; METER_SEGS],
    gun_label: Entity,

    /// The brake-charge strip and its ticks.
    charge_row: Entity,
    charge_segs: [Entity; CHARGE_SEGS],

    /// The `EMP` charge strip: its row, its legend, and its ticks.
    ///
    /// Not in [`Self::cockpit_hidden`] — that array mirrors the six elements
    /// `index.html:865`–`:870` names and nothing else — but it *is* switched off
    /// by an EMP, which is what its row's visibility carries.
    emp_row: Entity,
    emp_label: Entity,
    emp_segs: [Entity; EMP_SEGS],
    /// The `EMP` legend on a dark glass: the one thing the head-up display can
    /// still say while the pulse is running.
    emp_banner: Entity,

    missile_pips: [Entity; MISSILE_PIPS],
    flare_pips: [Entity; FLARE_PIPS],

    /// The node that carries the boresight's *position*. Split from the sight
    /// below because `sync_hud` owns the locked scale and `sync_world_markers`
    /// owns the position, and a `UiTransform` holds both scale and translation —
    /// one component, two writers, one clobbering the other. Two nodes, one
    /// writer each.
    reticle_anchor: Entity,
    reticle: Entity,
    /// The circle's four radial ticks and the pipper at its centre, all of
    /// which take the lock colour with it.
    reticle_marks: [Entity; 5],

    lock_warning: Entity,
    vignette: Entity,
    death_banner: Entity,
    /// The result card. One line of text, recoloured per outcome.
    result_banner: Entity,
    result_text: Entity,

    match_panel: Entity,
    team0: Entity,
    team1: Entity,
    clock: Entity,

    markers: [MarkerNodes; TARGET_POOL],
    killfeed: [KillRowNodes; KILLFEED_ROWS],
}

/// One tape's writable entities. Everything else about a tape — every tick,
/// every numeral, the spine, the caps and the caption — is built once and never
/// touched again.
#[derive(Clone, Copy)]
struct TapeNodes {
    /// The whole assembly, for the hull tape's cockpit stand-down.
    root: Entity,
    /// The ladder. Carries the scroll, and nothing else ever writes to it.
    ladder: Entity,
    /// The current-value readout beside the index.
    value: Entity,
    /// The index caret through the spine, which takes the alert colour.
    caret: Entity,
    /// The commanded-throttle bug, or [`Entity::PLACEHOLDER`] on a tape that
    /// has no commanded value.
    bug: Entity,
}

impl Default for TapeNodes {
    fn default() -> Self {
        TapeNodes {
            root: Entity::PLACEHOLDER,
            ladder: Entity::PLACEHOLDER,
            value: Entity::PLACEHOLDER,
            caret: Entity::PLACEHOLDER,
            bug: Entity::PLACEHOLDER,
        }
    }
}

/// One world-space target slot's entities.
#[derive(Clone, Copy)]
struct MarkerNodes {
    /// The corner bracket. Carries the slot's position and its visibility; the
    /// label rides along as a child, which is what `main.js:679`
    /// (`box.appendChild(label)`) does and what keeps the label to one write.
    boxes: Entity,
    /// The four corner brackets, whose colour says whether aim assist has
    /// picked this target.
    corners: [Entity; 4],
    /// The callsign and hit points.
    label: Entity,
    /// The lead marker, positioned separately because it is 16px where the
    /// bracket is 44 and the two therefore need different offsets from the same
    /// point.
    lead: Entity,
    /// The aligned ring: solid, filled.
    lead_solid: Entity,
    /// The scattered ring that stands in for a dashed border.
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

/// One killfeed row's entities.
#[derive(Clone, Copy)]
struct KillRowNodes {
    /// The row itself, hidden when the entry has expired.
    row: Entity,
    /// Who scored it.
    killer: Entity,
    /// Who died.
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
/// anything. Everything a state change could want is present from the start with
/// its inactive value — an unlit segment, a hidden warning, a caret at rest — so
/// a state change is only ever a write to an existing component and never an
/// archetype move.
///
/// The two ladders are the extreme case of that and the reason the tapes are
/// shaped the way they are: about seventy tick and numeral nodes exist here
/// from startup and **not one of them is ever written to again**. Reading a new
/// speed moves their common parent.
fn spawn_hud(mut commands: Commands, assets: Res<AssetServer>) {
    let font = HudFont(assets.load(FONT_PATH));
    // No camera is spawned here. `bevy_ui` renders root nodes to the default UI
    // camera, which `DefaultUiCamera` resolves as the highest-order camera
    // targeting the primary window — `camera.rs`'s `Camera3d`. Adding a
    // `Camera2d` would give the window a second camera and make that choice
    // ambiguous.
    let mut n = Nodes::default();

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
            // Hidden until the simulation produces a local ship.
            Visibility::Hidden,
        ))
        .with_children(|hud| {
            build_tapes(hud, &font, &mut n);
            build_meters(hud, &font, &mut n);
            build_pips(hud, &font, &mut n);

            // The target pool goes in before the boresight so the sight draws
            // over a bracket rather than under it.
            for slot in &mut n.markers {
                *slot = spawn_marker(hud, &font);
            }
            build_boresight(hud, &mut n);

            build_killfeed(hud, &font, &mut n);
            build_banners(hud, &font, &mut n);
            build_scoreline(hud, &font, &mut n);
        })
        .id();

    commands.insert_resource(n.into_nodes(root));
}

/// Scratch for [`spawn_hud`]'s builders.
///
/// One mutable struct rather than thirty `let mut ... = Entity::PLACEHOLDER`
/// threaded through closures: the tree is deep enough that passing the ids back
/// out by hand was most of the function.
struct Nodes {
    cockpit_hidden: [Entity; 6],
    spd: TapeNodes,
    hull: TapeNodes,
    alt_row: Entity,
    alt_caption: Entity,
    alt_value: Entity,
    pull_up: Entity,
    boost_segs: [Entity; METER_SEGS],
    gun_segs: [Entity; METER_SEGS],
    gun_label: Entity,
    charge_row: Entity,
    charge_segs: [Entity; CHARGE_SEGS],
    emp_row: Entity,
    emp_label: Entity,
    emp_segs: [Entity; EMP_SEGS],
    emp_banner: Entity,
    missile_pips: [Entity; MISSILE_PIPS],
    flare_pips: [Entity; FLARE_PIPS],
    reticle_anchor: Entity,
    reticle: Entity,
    reticle_marks: [Entity; 5],
    lock_warning: Entity,
    vignette: Entity,
    death_banner: Entity,
    result_banner: Entity,
    result_text: Entity,
    match_panel: Entity,
    team0: Entity,
    team1: Entity,
    clock: Entity,
    markers: [MarkerNodes; TARGET_POOL],
    killfeed: [KillRowNodes; KILLFEED_ROWS],
}

impl Default for Nodes {
    fn default() -> Nodes {
        // `Entity` has no `Default` — deliberately, since there is no sensible
        // "no entity" — so every field starts at the placeholder the builders
        // then overwrite.
        Nodes {
            cockpit_hidden: [Entity::PLACEHOLDER; 6],
            spd: TapeNodes::default(),
            hull: TapeNodes::default(),
            alt_row: Entity::PLACEHOLDER,
            alt_caption: Entity::PLACEHOLDER,
            alt_value: Entity::PLACEHOLDER,
            pull_up: Entity::PLACEHOLDER,
            boost_segs: [Entity::PLACEHOLDER; METER_SEGS],
            gun_segs: [Entity::PLACEHOLDER; METER_SEGS],
            gun_label: Entity::PLACEHOLDER,
            charge_row: Entity::PLACEHOLDER,
            charge_segs: [Entity::PLACEHOLDER; CHARGE_SEGS],
            emp_row: Entity::PLACEHOLDER,
            emp_label: Entity::PLACEHOLDER,
            emp_segs: [Entity::PLACEHOLDER; EMP_SEGS],
            emp_banner: Entity::PLACEHOLDER,
            missile_pips: [Entity::PLACEHOLDER; MISSILE_PIPS],
            flare_pips: [Entity::PLACEHOLDER; FLARE_PIPS],
            reticle_anchor: Entity::PLACEHOLDER,
            reticle: Entity::PLACEHOLDER,
            reticle_marks: [Entity::PLACEHOLDER; 5],
            lock_warning: Entity::PLACEHOLDER,
            vignette: Entity::PLACEHOLDER,
            death_banner: Entity::PLACEHOLDER,
            result_banner: Entity::PLACEHOLDER,
            result_text: Entity::PLACEHOLDER,
            match_panel: Entity::PLACEHOLDER,
            team0: Entity::PLACEHOLDER,
            team1: Entity::PLACEHOLDER,
            clock: Entity::PLACEHOLDER,
            markers: [MarkerNodes::default(); TARGET_POOL],
            killfeed: [KillRowNodes::default(); KILLFEED_ROWS],
        }
    }
}

impl Nodes {
    fn into_nodes(self, root: Entity) -> HudNodes {
        HudNodes {
            root,
            cockpit_hidden: self.cockpit_hidden,
            spd: self.spd,
            hull: self.hull,
            alt_row: self.alt_row,
            alt_caption: self.alt_caption,
            alt_value: self.alt_value,
            pull_up: self.pull_up,
            boost_segs: self.boost_segs,
            gun_segs: self.gun_segs,
            gun_label: self.gun_label,
            charge_row: self.charge_row,
            charge_segs: self.charge_segs,
            emp_row: self.emp_row,
            emp_label: self.emp_label,
            emp_segs: self.emp_segs,
            emp_banner: self.emp_banner,
            missile_pips: self.missile_pips,
            flare_pips: self.flare_pips,
            reticle_anchor: self.reticle_anchor,
            reticle: self.reticle,
            reticle_marks: self.reticle_marks,
            lock_warning: self.lock_warning,
            vignette: self.vignette,
            death_banner: self.death_banner,
            result_banner: self.result_banner,
            result_text: self.result_text,
            match_panel: self.match_panel,
            team0: self.team0,
            team1: self.team1,
            clock: self.clock,
            markers: self.markers,
            killfeed: self.killfeed,
        }
    }
}

// ---------------------------------------------------------------------------
// The tapes
// ---------------------------------------------------------------------------

/// Which side of the glass a tape lives on.
///
/// It decides three things at once and they all have to agree: which screen
/// edge the assembly is pinned to, which way round the ladder's tick and
/// numeral sit, and which side of the spine the readout hangs off. Getting one
/// of them backwards is a tape that reads outward.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    /// Speed, on the left, spine outboard of the numbers.
    Left,
    /// Hull, on the right, spine outboard of the numbers.
    Right,
}

/// Everything one ladder needs to be drawn.
struct Ladder {
    /// Top of the scale, in its own units. The bottom is always zero.
    top: i32,
    /// A numeral every this many units.
    major: i32,
    /// A bare tick every this many.
    minor: i32,
}

fn build_tapes(hud: &mut ChildSpawnerCommands, font: &HudFont, n: &mut Nodes) {
    n.spd = spawn_tape(
        hud,
        font,
        Side::Left,
        "SPD",
        &Ladder {
            top: SPD_TOP,
            major: SPD_MAJOR,
            minor: SPD_MINOR,
        },
        true,
    );

    // The hull tape *is* the health bar, so it is the first thing
    // `body.cockpit-view` stands down. The altitude block below is its own root
    // and stays up: `index.html:865`–`:870` hides six elements and there was
    // never an altimeter among them, and the instrument panel has no altimeter
    // to duplicate one with.
    n.hull = spawn_tape(
        hud,
        font,
        Side::Right,
        "HULL",
        &Ladder {
            top: MAX_HP,
            major: HULL_MAJOR,
            minor: HULL_MINOR,
        },
        false,
    );
    n.cockpit_hidden[0] = n.hull.root;

    // -- AGL / RNG, under the hull tape ---------------------------------------
    n.alt_row = hud
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: percent(50.0 + TAPE_INSET),
                top: percent(50),
                margin: UiRect::top(px(TAPE_H / 2.0 + 26.0)),
                flex_direction: FlexDirection::Column,
                row_gap: px(3),
                ..default()
            },
            // Down at startup, which is what `AltBlock::default()` says. Every
            // node whose resting state is not the tree's spawned state is a
            // first-frame write waiting to be skipped — see `sync_hud`.
            Visibility::Hidden,
        ))
        .with_children(|col| {
            col.spawn(Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Baseline,
                column_gap: px(6),
                ..default()
            })
            .with_children(|row| {
                n.alt_caption = row.spawn(caption(font, "RNG", 9.0)).id();
                n.alt_value = row
                    .spawn((
                        Text::new("0"),
                        hud_font(font, 15.0, 600),
                        TextColor(PHOSPHOR),
                        LetterSpacing::Px(1.0),
                        TextLayout::new(Justify::Left, LineBreak::NoWrap),
                    ))
                    .id();
            });
            n.pull_up = col
                .spawn((
                    Text::new(PULL_UP_TEXT.to_owned()),
                    hud_font(font, 12.0, 800),
                    TextColor(WARN),
                    LetterSpacing::Px(4.0),
                    TextLayout::new(Justify::Left, LineBreak::NoWrap),
                    Visibility::Hidden,
                ))
                .id();
        })
        .id();
}

/// One tape: a scrolling ladder past a fixed index, with a legend under it.
///
/// The ladder is built whole — every tick and every numeral from zero to the
/// top of the scale — and clipped by a window [`TAPE_H`] tall. That is the
/// trade this design makes: about forty nodes that exist forever and cost one
/// layout pass at startup, against a recycling pool that would have to rewrite
/// a numeral every time a graduation crossed the window edge. Forty static
/// nodes are free every frame; a text write is not.
fn spawn_tape(
    hud: &mut ChildSpawnerCommands,
    font: &HudFont,
    side: Side,
    legend_text: &str,
    ladder: &Ladder,
    with_bug: bool,
) -> TapeNodes {
    let mut out = TapeNodes::default();

    // Pinned to a fraction of the viewport rather than to a pixel count, so the
    // pair frames the boresight the same way at any window size.
    let mut anchor = Node {
        position_type: PositionType::Absolute,
        top: percent(50),
        margin: UiRect::top(px(-TAPE_H / 2.0)),
        flex_direction: FlexDirection::Column,
        row_gap: px(4),
        ..default()
    };
    match side {
        Side::Left => anchor.right = percent(50.0 + TAPE_INSET),
        Side::Right => anchor.left = percent(50.0 + TAPE_INSET),
    }

    out.root = hud
        .spawn(anchor)
        .with_children(|col| {
            col.spawn(Node {
                height: px(TAPE_H),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                ..default()
            })
            .with_children(|row| {
                // Spine, then ladder, reading in from the screen edge — the
                // `┌ 420` and `100 ┐` of the sketch — with the current-value
                // readout outboard of the spine, where a real airspeed box
                // sits. The order of the three reverses with the side and
                // nothing else does.
                if side == Side::Right {
                    spawn_ladder_window(row, font, side, ladder, &mut out, with_bug);
                    spawn_spine(row, side, &mut out);
                    out.value = row.spawn(tape_value(font, side)).id();
                } else {
                    out.value = row.spawn(tape_value(font, side)).id();
                    spawn_spine(row, side, &mut out);
                    spawn_ladder_window(row, font, side, ladder, &mut out, with_bug);
                }
            });

            // The legend, tucked against the spine on the ladder's side of it —
            // `│ SPD` and `HULL ┘`. Padded out past the readout rather than
            // justified, because the readout is the wider of the two halves and
            // justifying would put the word under it.
            let past_readout = px(TAPE_VALUE_W + TAPE_VALUE_GAP * 2.0 + 4.0);
            col.spawn(Node {
                width: px(TAPE_ROW_W),
                padding: match side {
                    Side::Left => UiRect::left(past_readout),
                    Side::Right => UiRect::right(past_readout),
                },
                justify_content: match side {
                    Side::Left => JustifyContent::Start,
                    Side::Right => JustifyContent::End,
                },
                ..default()
            })
            .with_children(|r| {
                r.spawn(caption(font, legend_text, 9.0));
            });
        })
        .id();

    out
}

/// The tape's vertical rule, with a cap turned in at each end.
///
/// Three one-pixel nodes. They are the `┌ │ └` of the sketch and the only
/// structural lines in the whole design.
fn spawn_spine(row: &mut ChildSpawnerCommands, side: Side, out: &mut TapeNodes) {
    row.spawn((
        Node {
            width: px(1),
            height: px(TAPE_H),
            ..default()
        },
        BackgroundColor(hairline()),
    ))
    .with_children(|spine| {
        for top in [0.0, TAPE_H - 1.0] {
            // The caps turn in over the ladder, which is on the inboard side of
            // the spine on the left tape and the outboard side on the right —
            // `┌`/`└` against `┐`/`┘`. Anchoring both to `left` drew the right
            // tape's caps out into empty sky.
            let mut cap = Node {
                position_type: PositionType::Absolute,
                top: px(top),
                width: px(TICK_MAJOR),
                height: px(1),
                ..default()
            };
            match side {
                Side::Left => cap.left = px(0),
                Side::Right => cap.right = px(0),
            }
            spine.spawn((cap, BackgroundColor(hairline())));
        }
        // The index. Sits *through* the spine at the reading line, and is the
        // one part of the frame that takes the alert colour.
        out.caret = spine
            .spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: px(-CARET_W / 2.0),
                    top: px((TAPE_H - CARET_H) / 2.0),
                    width: px(CARET_W),
                    height: px(CARET_H),
                    ..default()
                },
                BackgroundColor(PHOSPHOR),
            ))
            .id();
    });
}

/// The clipped window and the ladder inside it.
fn spawn_ladder_window(
    row: &mut ChildSpawnerCommands,
    font: &HudFont,
    side: Side,
    ladder: &Ladder,
    out: &mut TapeNodes,
    with_bug: bool,
) {
    row.spawn(Node {
        width: px(TAPE_W),
        height: px(TAPE_H),
        // Without this the ladder draws its whole two hundred units down the
        // canopy.
        overflow: Overflow::clip(),
        ..default()
    })
    .with_children(|window| {
        out.ladder = window
            .spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: px(0),
                    top: px(0),
                    width: percent(100),
                    height: px(ladder.top as f32 * TAPE_PPU),
                    ..default()
                },
                // The only thing ever written to a tape's ladder.
                UiTransform::IDENTITY,
            ))
            .with_children(|l| {
                spawn_graduations(l, font, side, ladder);
            })
            .id();

        if with_bug {
            // The commanded-throttle bug. Rides the window rather than the
            // ladder: its position is already the difference between two tape
            // readings, so it moves against the *glass*, not against the scale.
            out.bug = window
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: px(0),
                        top: px(-CARET_H / 2.0),
                        width: px(TICK_MINOR),
                        height: px(CARET_H),
                        ..default()
                    },
                    BackgroundColor(AMBER),
                    UiTransform::IDENTITY,
                ))
                .id();
        }
    });
}

/// Every tick and numeral on one ladder, laid out once.
fn spawn_graduations(l: &mut ChildSpawnerCommands, font: &HudFont, side: Side, ladder: &Ladder) {
    let mut v = 0;
    while v <= ladder.top {
        let major = v % ladder.major == 0;
        // Zero at the bottom, `top` at the top, which is the way a tape runs.
        let y = (ladder.top - v) as f32 * TAPE_PPU - TAPE_ROW_H / 2.0;
        let mut node = Node {
            position_type: PositionType::Absolute,
            left: px(0),
            right: px(0),
            top: px(y),
            height: px(TAPE_ROW_H),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: px(4),
            ..default()
        };
        // The tick always touches the spine, so on the right-hand tape the row
        // packs to its end and the numeral leads.
        node.justify_content = if side == Side::Left {
            JustifyContent::Start
        } else {
            JustifyContent::End
        };

        l.spawn(node).with_children(|r| {
            let tick = (
                Node {
                    width: px(if major { TICK_MAJOR } else { TICK_MINOR }),
                    height: px(1),
                    ..default()
                },
                BackgroundColor(if major {
                    PHOSPHOR.with_alpha(0.6)
                } else {
                    PHOSPHOR.with_alpha(0.3)
                }),
            );
            if side == Side::Left {
                r.spawn(tick);
                if major {
                    r.spawn(graduation(font, v));
                }
            } else {
                if major {
                    r.spawn(graduation(font, v));
                }
                r.spawn(tick);
            }
        });

        v += ladder.minor;
    }
}

/// One numeral on a ladder. Never rewritten — see [`spawn_tape`].
fn graduation(font: &HudFont, v: i32) -> impl Bundle {
    (
        Text::new(v.to_string()),
        hud_font(font, 10.0, 500),
        TextColor(PHOSPHOR.with_alpha(0.72)),
        TextLayout::new(Justify::Left, LineBreak::NoWrap),
    )
}

/// The current-value readout that sits against the index.
fn tape_value(font: &HudFont, side: Side) -> impl Bundle {
    (
        Node {
            width: px(TAPE_VALUE_W),
            margin: UiRect::axes(px(TAPE_VALUE_GAP), px(0)),
            ..default()
        },
        Text::new("0"),
        hud_font(font, 19.0, 700),
        TextColor(PHOSPHOR),
        LetterSpacing::Px(1.0),
        TextLayout::new(
            if side == Side::Left {
                Justify::Right
            } else {
                Justify::Left
            },
            LineBreak::NoWrap,
        ),
    )
}

// ---------------------------------------------------------------------------
// The meters
// ---------------------------------------------------------------------------

fn build_meters(hud: &mut ChildSpawnerCommands, font: &HudFont, n: &mut Nodes) {
    // -- the brake-charge strip -----------------------------------------------
    // Uniform ticks, so it cannot be read as one of the two ramped stacks. It
    // is only on the glass while the brake is charging, which is what the CSS
    // said with `opacity: 0` and what `Visibility` says here.
    //
    // Two nodes, not one, and the reason is a rule this module runs on: a
    // `Visibility` may have exactly one writer. The outer row is what
    // `body.cockpit-view` stands down and the inner strip is what the brake
    // raises, and folding them together meant leaving the cockpit re-showed an
    // idle strip — the seat's write landing after the brake's and nothing left
    // to correct it.
    n.cockpit_hidden[1] = hud
        .spawn(centred_row(CHARGE_ROW_BOTTOM))
        .with_children(|row| {
            n.charge_row = row
                .spawn((
                    Node {
                        flex_direction: FlexDirection::Row,
                        align_items: AlignItems::Center,
                        column_gap: px(SEG_GAP),
                        ..default()
                    },
                    // `Visibility` rather than `Display::None`: hiding by
                    // display drops the node out of layout and forces a
                    // relayout on every brake, which is the cost this module
                    // exists to avoid.
                    Visibility::Hidden,
                ))
                .with_children(|strip| {
                    for slot in &mut n.charge_segs {
                        *slot = strip
                            .spawn((
                                Node {
                                    width: px(SEG_W - 1.0),
                                    height: px(CHARGE_SEG_H),
                                    ..default()
                                },
                                BackgroundColor(unlit()),
                            ))
                            .id();
                    }
                })
                .id();
        })
        .id();

    // -- GUN, left of centre; BST, right of it --------------------------------
    // `▁▃▅▇ GUN   ▁▃▅ BST`. Both ramps climb the same way rather than mirroring
    // about the boresight: a mirrored pair reads as one wide symbol and the eye
    // stops telling the two apart, which is the opposite of what a glance at a
    // meter is for.
    let gun = hud
        .spawn(Node {
            position_type: PositionType::Absolute,
            right: percent(50),
            bottom: px(METER_ROW_BOTTOM),
            margin: UiRect::right(px(ROW_SPLIT)),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::End,
            column_gap: px(7),
            ..default()
        })
        .with_children(|row| {
            spawn_stack(row, &mut n.gun_segs, false);
            n.gun_label = row.spawn(caption(font, "GUN", 9.0)).id();
        })
        .id();
    n.cockpit_hidden[3] = gun;

    let boost = hud
        .spawn(Node {
            position_type: PositionType::Absolute,
            left: percent(50),
            bottom: px(METER_ROW_BOTTOM),
            margin: UiRect::left(px(ROW_SPLIT)),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::End,
            column_gap: px(7),
            ..default()
        })
        .with_children(|row| {
            spawn_stack(row, &mut n.boost_segs, false);
            row.spawn(caption(font, "BST", 9.0));
        })
        .id();
    n.cockpit_hidden[2] = boost;

    // -- the EMP charge strip, above the brake charge -------------------------
    // Uniform ticks like the brake's, because it is the same kind of readout: a
    // thing filling up rather than a quantity being spent. The legend sits to
    // its left and turns amber with the strip when the weapon is armed, which is
    // the only moment the pilot needs to notice it — for the fifty-nine seconds
    // before that it is a slow bar in the corner of the eye, which is exactly
    // what it should be.
    //
    // Always up, including in the cockpit: the instrument panel has no EMP
    // gauge, so this duplicates nothing the seated pilot can already read. It
    // goes out only under a pulse, which is what its own row's visibility says.
    n.emp_row = hud
        .spawn(centred_row(EMP_ROW_BOTTOM))
        .with_children(|row| {
            row.spawn(Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: px(7),
                ..default()
            })
            .with_children(|inner| {
                n.emp_label = inner.spawn(caption(font, "EMP", 9.0)).id();
                inner
                    .spawn(Node {
                        flex_direction: FlexDirection::Row,
                        align_items: AlignItems::Center,
                        column_gap: px(SEG_GAP),
                        ..default()
                    })
                    .with_children(|strip| {
                        for slot in &mut n.emp_segs {
                            *slot = strip
                                .spawn((
                                    Node {
                                        width: px(SEG_W - 1.0),
                                        height: px(CHARGE_SEG_H),
                                        ..default()
                                    },
                                    BackgroundColor(unlit()),
                                ))
                                .id();
                        }
                    });
            });
        })
        .id();
}

/// One `▁▃▅▇` stack. `reversed` runs the ramp right to left.
///
/// Index 0 is always the *shortest* segment, whichever way the ramp is drawn,
/// so "n lit" is a prefix of the array and [`sync_stack`] can compare one
/// integer per segment rather than reasoning about direction.
fn spawn_stack(row: &mut ChildSpawnerCommands, segs: &mut [Entity; METER_SEGS], reversed: bool) {
    row.spawn(Node {
        flex_direction: if reversed {
            FlexDirection::RowReverse
        } else {
            FlexDirection::Row
        },
        align_items: AlignItems::End,
        column_gap: px(SEG_GAP),
        ..default()
    })
    .with_children(|stack| {
        for (i, slot) in segs.iter_mut().enumerate() {
            let t = i as f32 / (METER_SEGS - 1) as f32;
            *slot = stack
                .spawn((
                    Node {
                        width: px(SEG_W),
                        height: px(SEG_H_MIN + (SEG_H_MAX - SEG_H_MIN) * t),
                        ..default()
                    },
                    BackgroundColor(unlit()),
                ))
                .id();
        }
    });
}

// ---------------------------------------------------------------------------
// Stores
// ---------------------------------------------------------------------------

fn build_pips(hud: &mut ChildSpawnerCommands, font: &HudFont, n: &mut Nodes) {
    let msl = hud
        .spawn(Node {
            position_type: PositionType::Absolute,
            right: percent(50),
            bottom: px(PIP_ROW_BOTTOM),
            margin: UiRect::right(px(ROW_SPLIT)),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: px(7),
            ..default()
        })
        .with_children(|row| {
            row.spawn(caption(font, "MSL", 9.0));
            row.spawn(pip_row()).with_children(|pips| {
                for slot in &mut n.missile_pips {
                    *slot = pips.spawn(pip(ORDNANCE)).id();
                }
            });
        })
        .id();
    n.cockpit_hidden[4] = msl;

    let flr = hud
        .spawn(Node {
            position_type: PositionType::Absolute,
            left: percent(50),
            bottom: px(PIP_ROW_BOTTOM),
            margin: UiRect::left(px(ROW_SPLIT)),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: px(7),
            ..default()
        })
        .with_children(|row| {
            row.spawn(caption(font, "FLR", 9.0));
            row.spawn(pip_row()).with_children(|pips| {
                for slot in &mut n.flare_pips {
                    *slot = pips.spawn(pip(AMBER)).id();
                }
            });
        })
        .id();
    n.cockpit_hidden[5] = flr;
}

// ---------------------------------------------------------------------------
// The boresight
// ---------------------------------------------------------------------------

/// The gun cross: a circle, four radial ticks, and the pipper at its centre.
///
/// **This is the reticle, and where it sits is not decoration.**
/// [`sync_world_markers`] slides the anchor onto the gun line at the range of
/// whatever the nose is nearest to, because the camera is not on that line and a
/// sight pinned to screen centre is wrong by a degree of parallax — which is
/// what made the lock never fill in. The anchor is laid out screen-centred so a
/// frame before the first projection draws it in the right place anyway.
fn build_boresight(hud: &mut ChildSpawnerCommands, n: &mut Nodes) {
    n.reticle_anchor = hud
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: percent(50),
                top: percent(50),
                width: px(SIGHT_D),
                height: px(SIGHT_D),
                margin: UiRect::new(px(-SIGHT_D / 2.0), px(0), px(-SIGHT_D / 2.0), px(0)),
                ..default()
            },
            UiTransform::IDENTITY,
        ))
        .with_children(|anchor| {
            n.reticle = anchor
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
                    BorderColor::all(PHOSPHOR),
                    // A `UiTransform` is applied after layout, so the lock
                    // state costs no relayout — where animating `width` would.
                    UiTransform::IDENTITY,
                ))
                .with_children(|r| {
                    // Four ticks radiating from the circle, clockwise from the
                    // top. A real gun cross has them; they are what tells you
                    // the sight is level when the horizon is not in view.
                    let arms: [(Val, Val, f32, f32); 4] = [
                        (percent(50), px(-SIGHT_TICK - 1.0), 1.0, SIGHT_TICK),
                        (percent(100), percent(50), SIGHT_TICK, 1.0),
                        (percent(50), percent(100), 1.0, SIGHT_TICK),
                        (px(-SIGHT_TICK - 1.0), percent(50), SIGHT_TICK, 1.0),
                    ];
                    for (slot, (left, top, w, h)) in n.reticle_marks.iter_mut().zip(arms) {
                        *slot = r
                            .spawn((
                                Node {
                                    position_type: PositionType::Absolute,
                                    left,
                                    top,
                                    width: px(w),
                                    height: px(h),
                                    ..default()
                                },
                                BackgroundColor(PHOSPHOR),
                            ))
                            .id();
                    }
                    // The pipper. The aim point itself, and the only filled
                    // shape on the glass.
                    n.reticle_marks[4] = r
                        .spawn((
                            Node {
                                position_type: PositionType::Absolute,
                                left: px(SIGHT_D / 2.0 - SIGHT_PIP / 2.0),
                                top: px(SIGHT_D / 2.0 - SIGHT_PIP / 2.0),
                                width: px(SIGHT_PIP),
                                height: px(SIGHT_PIP),
                                border_radius: BorderRadius::all(percent(50)),
                                ..default()
                            },
                            BackgroundColor(PHOSPHOR),
                        ))
                        .id();
                })
                .id();
        })
        .id();
}

// ---------------------------------------------------------------------------
// Killfeed, banners, scoreline
// ---------------------------------------------------------------------------

fn build_killfeed(hud: &mut ChildSpawnerCommands, font: &HudFont, n: &mut Nodes) {
    hud.spawn((
        Node {
            position_type: PositionType::Absolute,
            // `main.rs` notes the macOS traffic lights float over the top-left
            // ~80x30 px of the viewport, so the feed starts below them.
            top: px(44),
            left: px(20),
            max_width: vw(70),
            flex_direction: FlexDirection::Column,
            row_gap: px(5),
            ..default()
        },
        ZIndex(Z_OVERLAY),
    ))
    .with_children(|feed| {
        for slot in &mut n.killfeed {
            *slot = spawn_kill_row(feed, font);
        }
    });
}

fn build_banners(hud: &mut ChildSpawnerCommands, font: &HudFont, n: &mut Nodes) {
    // -- the hit vignette -----------------------------------------------------
    n.vignette = hud
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

    // -- DESTROYED ------------------------------------------------------------
    hud.spawn(Node {
        position_type: PositionType::Absolute,
        left: px(0),
        right: px(0),
        top: percent(42),
        justify_content: JustifyContent::Center,
        ..default()
    })
    .with_children(|row| {
        n.death_banner = row
            .spawn((
                Text::new("DESTROYED"),
                hud_font(font, 54.0, 800),
                TextColor(WARN),
                LetterSpacing::Px(14.0),
                Visibility::Hidden,
            ))
            .id();
    });

    // -- the result card ------------------------------------------------------
    // Above the death banner's line, because a match can perfectly well end
    // while you are dead and the two would otherwise overlap.
    n.result_banner = hud
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(0),
                right: px(0),
                top: percent(28),
                justify_content: JustifyContent::Center,
                ..default()
            },
            Visibility::Hidden,
        ))
        .with_children(|row| {
            n.result_text = row
                .spawn((
                    Text::new(Outcome::Draw.text()),
                    hud_font(font, 48.0, 800),
                    TextColor(PHOSPHOR),
                    LetterSpacing::Px(14.0),
                ))
                .id();
        })
        .id();

    // -- the missile-lock warning ---------------------------------------------
    hud.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: px(0),
            right: px(0),
            top: px(96),
            justify_content: JustifyContent::Center,
            ..default()
        },
        ZIndex(Z_WARNING),
    ))
    .with_children(|row| {
        n.lock_warning = row
            .spawn((
                Text::new(LOCK_WARNING_TEXT.to_owned()),
                hud_font(font, 15.0, 800),
                TextColor(WARN),
                LetterSpacing::Px(7.0),
                Visibility::Hidden,
            ))
            .id();
    });

    // -- the EMP legend -------------------------------------------------------
    //
    // §2 asks how a victim knows what happened, and this is the answer: one word
    // on an otherwise empty glass. Without it a pilot who has never been hit by
    // one reads four seconds of nothing as the game having broken, which is the
    // worst possible outcome for a weapon whose entire effect is an absence.
    //
    // One word and no more. A countdown would hand back a piece of the
    // information the pulse just took, and the recovery is already legible: the
    // cockpit reboots on a ramp and the glass comes back with it.
    //
    // Amber rather than red. Red on this HUD means a threat you must act on —
    // the hull warning, the lock warning, `PULL UP`. An EMP is a caution: there
    // is nothing to do about it but keep flying.
    hud.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: px(0),
            right: px(0),
            top: percent(42),
            justify_content: JustifyContent::Center,
            ..default()
        },
        ZIndex(Z_WARNING),
    ))
    .with_children(|row| {
        n.emp_banner = row
            .spawn((
                Text::new(EMP_BANNER_TEXT.to_owned()),
                hud_font(font, 15.0, 800),
                TextColor(AMBER),
                LetterSpacing::Px(7.0),
                Visibility::Hidden,
            ))
            .id();
    });
}

/// The scoreline: two counts and a clock, separated by hairlines.
///
/// No panel behind it. The old one was `--glass-bg` with a blurred backdrop and
/// a rounded border, which is the single most web-looking thing that was on the
/// screen.
fn build_scoreline(hud: &mut ChildSpawnerCommands, font: &HudFont, n: &mut Nodes) {
    hud.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: px(0),
            right: px(0),
            top: px(18),
            justify_content: JustifyContent::Center,
            ..default()
        },
        ZIndex(Z_OVERLAY),
    ))
    .with_children(|row| {
        n.match_panel = row
            .spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: px(16),
                    ..default()
                },
                Visibility::Hidden,
            ))
            .with_children(|panel| {
                n.team0 = panel.spawn(score_text(font, FRIENDLY, Justify::Right)).id();
                panel.spawn(divider());
                n.clock = panel
                    .spawn((
                        Node {
                            min_width: px(76),
                            ..default()
                        },
                        Text::new("0:00"),
                        hud_font(font, 26.0, 700),
                        TextColor(PHOSPHOR),
                        LetterSpacing::Px(2.0),
                        TextLayout::new(Justify::Center, LineBreak::NoWrap),
                    ))
                    .id();
                panel.spawn(divider());
                n.team1 = panel.spawn(score_text(font, HOSTILE, Justify::Left)).id();
            })
            .id();
    });
}

/// A vertical hairline between two cells of the scoreline.
fn divider() -> impl Bundle {
    (
        Node {
            width: px(1),
            height: px(16),
            ..default()
        },
        BackgroundColor(hairline()),
    )
}

// ---------------------------------------------------------------------------
// The world-space markers
// ---------------------------------------------------------------------------

/// Builds one target slot: a bracketed box with a label, and a lead ring.
///
/// Both start hidden and at the tree's origin. Neither is ever rebuilt; the
/// only thing that happens to them afterwards is a translation, a visibility
/// flag, and — when the target itself changes — a string and four colours.
fn spawn_marker(hud: &mut ChildSpawnerCommands, font: &HudFont) -> MarkerNodes {
    let mut corners = [Entity::PLACEHOLDER; 4];
    let mut label = Entity::PLACEHOLDER;

    // Four L-shaped corner brackets: four small nodes wearing two borders each.
    // The web original stacked eight linear gradients to draw the same thing,
    // which also made "which target is the assist on" an eight-layer gradient
    // string rather than four colour writes.
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
            // The half-box offset is folded into the translation
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
                        BorderColor::all(HOSTILE),
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
                    hud_font(font, 10.0, 600),
                    TextColor(HOSTILE),
                    LetterSpacing::Px(1.5),
                    TextLayout::new(Justify::Left, LineBreak::NoWrap),
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

/// The lead marker, in both of its states.
///
/// A 16px circle with a **dashed** outline that goes **solid and filled** when
/// the shot is lined up. `bevy_ui` has one border style and it is solid, so the
/// scattered state is drawn rather than styled: eight dots on the same circle,
/// which is what a 2px dash pattern resolves to at that diameter anyway. Both
/// rings are built here and the swap is two `Visibility` writes — no archetype
/// move, no relayout, and no per-frame cost, since it only happens when the shot
/// goes from lined up to not.
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
                    BorderColor::all(HOSTILE),
                    BackgroundColor(HOSTILE.with_alpha(0.35)),
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
                            BackgroundColor(HOSTILE),
                        ));
                    }
                })
                .id();
        })
        .id();

    (root, solid, dashed)
}

/// One killfeed row: killer, arrow, victim. No slab, no border, no spine.
fn spawn_kill_row(feed: &mut ChildSpawnerCommands, font: &HudFont) -> KillRowNodes {
    let mut killer = Entity::PLACEHOLDER;
    let mut victim = Entity::PLACEHOLDER;

    let row = feed
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: px(8),
                ..default()
            },
            Visibility::Hidden,
        ))
        .with_children(|r| {
            killer = r.spawn(kill_text(font, PHOSPHOR)).id();
            r.spawn((
                Text::new(KF_ARROW.to_owned()),
                hud_font(font, 9.0, 600),
                TextColor(legend()),
            ));
            victim = r.spawn(kill_text(font, legend())).id();
        })
        .id();

    KillRowNodes {
        row,
        killer,
        victim,
    }
}

/// A killfeed cell. `text-transform: uppercase` is applied when the string is
/// written, since `bevy_text` has no such property.
fn kill_text(font: &HudFont, colour: Color) -> impl Bundle {
    (
        Text::new(String::new()),
        hud_font(font, 10.0, 600),
        TextColor(colour),
        LetterSpacing::Px(1.5),
        TextLayout::new(Justify::Left, LineBreak::NoWrap),
    )
}

// --- tree helpers ----------------------------------------------------------

/// A full-width row pinned `bottom` pixels off the floor, centring its child.
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

/// A legend: upper case, tracked, dim. `SPD`, `HULL`, `GUN`, `MSL`, `AGL`.
///
/// Tracked at a fifth of the size, which is `ui.rs::caption`'s ratio — the two
/// surfaces set their captions the same way as well as in the same colour.
fn caption(font: &HudFont, text: &str, size: f32) -> impl Bundle {
    (
        Text::new(text.to_owned()),
        hud_font(font, size, 700),
        TextColor(legend()),
        LetterSpacing::Px(size * 0.2),
        TextLayout::new(Justify::Left, LineBreak::NoWrap),
    )
}

/// The row a set of pips sits in.
fn pip_row() -> Node {
    Node {
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::Center,
        column_gap: px(3),
        ..default()
    }
}

/// One missile or flare pip: a stroke, present or spent.
///
/// The border is spawned at width 1 and colour `NONE` even though a loaded pip
/// has no outline: with `BoxSizing::BorderBox` a 1px border does not change the
/// footprint, so "spent" is reached by writing two colours and never by changing
/// the layout.
fn pip(colour: Color) -> impl Bundle {
    (
        Node {
            width: px(PIP_WIDTH),
            height: px(PIP_HEIGHT),
            border: UiRect::all(px(1)),
            ..default()
        },
        BackgroundColor(colour),
        BorderColor::all(Color::NONE),
    )
}

/// A scoreline cell: fixed minimum width, its own colour and alignment.
fn score_text(font: &HudFont, colour: Color, justify: Justify) -> impl Bundle {
    (
        Node {
            min_width: px(34),
            ..default()
        },
        Text::new("0"),
        hud_font(font, 20.0, 700),
        TextColor(colour),
        TextLayout::new(justify, LineBreak::NoWrap),
    )
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
    vis: Query<'w, 's, &'static mut Visibility>,
    bg: Query<'w, 's, &'static mut BackgroundColor>,
    border: Query<'w, 's, &'static mut BorderColor>,
    /// The vignette's radial gradient, which is the only gradient left on the
    /// glass — every bar it used to share the query with is a segment stack
    /// now, and a segment is a flat colour.
    grad: Query<'w, 's, &'static mut BackgroundGradient>,
    text: Query<'w, 's, &'static mut Text>,
    text_colour: Query<'w, 's, &'static mut TextColor>,
    /// The tape scrolls, the throttle bug, and the boresight's lock scale.
    /// **No `Node` query at all**: nothing in this HUD resizes, which is what
    /// keeps `bevy_ui`'s layout out of the frame entirely.
    xform: Query<'w, 's, &'static mut UiTransform>,
}

/// Writes the frame's HUD, and nothing that did not change.
///
/// The shape to keep: build the model, compare it whole, and return before
/// acquiring a single `Mut` if it matches. Everything after the early-out is
/// guarded by its own field comparison, so the cost of a frame is proportional
/// to what moved rather than to how many nodes exist.
// Eleven, against clippy's ten. Every one is a distinct resource the model is
// built from, and the writes are already grouped into `HudWrite` for this exact
// reason — grouping the reads as well would put a second indirection between
// this system and the values `model` takes, for no gain.
#[allow(clippy::too_many_arguments)]
fn sync_hud(
    frame: Res<SimFrame>,
    time: Res<Time>,
    view: Res<crate::cockpit::ViewMode>,
    free: Res<crate::replay::FreeCam>,
    lobby: Option<Res<crate::ui::LobbyOpen>>,
    setup: Res<crate::sim_bridge::MatchSetup>,
    nodes: Option<Res<HudNodes>>,
    roster: Res<Roster>,
    feed: Res<KillFeed>,
    sight: Res<Boresight>,
    result: Res<MatchResult>,
    mut applied: ResMut<AppliedHud>,
    mut w: HudWrite,
) {
    let Some(nodes) = nodes else { return };

    // The lobby covers the screen, so the flight overlay stands down behind it
    // for the same reason it stands down in the cockpit: something else is
    // already the interface.
    let hidden = view.seated || lobby.is_some_and(|l| l.0);

    // A replay's free camera stands the whole thing down, which is a stronger
    // claim than `seated` makes. Seated, the glass keeps its reticle, its tapes
    // and its scoreline, because a real HUD is projected on the canopy and the
    // pilot is still flying — only the bars go, and `model` documents that list
    // in full. Detached from every aircraft there is no pilot and no gun: a
    // crosshair in the middle of a drone shot is aiming something nobody is
    // holding. `HudModel::default` is the no-local-ship model, which is the one
    // that hides the root, and it comes back the moment the viewer rides
    // something.
    let next = if free.active {
        HudModel::default()
    } else {
        model(
            &frame.0,
            Env {
                time: time.elapsed_secs(),
                seated: hidden,
                locked: sight.locked,
                range: sight.range,
                map: setup.map,
                hull: crate::weapons::forced_hull(),
            },
            &feed,
            result.outcome,
        )
    };
    let prev = applied.0;

    // The early-out. On a frame where nothing the player can see has changed —
    // the overwhelming majority of frames — this system ends here, having read
    // two resources and compared thirty integers. No `Mut` is taken, so no
    // component is flagged changed, so `bevy_ui` skips the node in layout and
    // the render world skips it in extraction.
    if prev == Some(next) {
        return;
    }
    applied.0 = Some(next);

    // **The frame the HUD comes up is a full write.**
    //
    // Every guard below asks "did this field move", and before the first ship
    // exists the applied model is `HudModel::default()` — so any field whose
    // first *real* value happens to equal its default is judged unchanged and
    // never written, leaving that node in whatever state `spawn_hud` left it.
    // That is not hypothetical: it shipped a speed tape parked at the top of its
    // own ladder, because a stationary ship reads zero and zero is the default;
    // and an `RNG 0` under the hull tape, because "no contact" is the default
    // too and the row had never been told to hide.
    //
    // Dropping `prev` on the transition costs one full pass on the frame a match
    // starts and nothing on any other frame, and it removes the whole class
    // rather than the two instances that were noticed.
    let prev = if next.present && prev.is_none_or(|p| !p.present) {
        None
    } else {
        prev
    };

    /// True if this is the first write, or if any named field moved.
    macro_rules! moved {
        ($($field:ident),+ $(,)?) => {
            prev.is_none_or(|p| $(p.$field != next.$field)||+)
        };
    }

    // -- master visibility --------------------------------------------------
    // `result` counts here as well as `present`: the card hangs off this root,
    // so a match that ends while the local ship is gone would otherwise hide
    // the very thing it just put up.
    if moved!(present, result) {
        set_visible(
            &mut w.vis,
            nodes.root,
            next.present || next.result.is_some(),
        );
    }

    // -- the result card ----------------------------------------------------
    // Before the `present` early-out, because the card has to survive the local
    // ship: a match can end while you are dead.
    if moved!(result) {
        set_visible(&mut w.vis, nodes.result_banner, next.result.is_some());
        if let Some(outcome) = next.result {
            set_text(&mut w.text, nodes.result_text, || {
                outcome.text().to_string()
            });
            set(
                &mut w.text_colour,
                nodes.result_text,
                TextColor(outcome.colour()),
            );
        }
    }

    if !next.present {
        // Nothing below is meaningful without a ship, and leaving the tree
        // untouched means a HUD that is off costs one comparison a frame.
        return;
    }

    // -- body.cockpit-view --------------------------------------------------
    // Six writes on the frame `V` is pressed and none after it. The values
    // behind the hidden elements are zeroed while seated (see `model`), so the
    // guards below are all comparing an unchanging zero and none of them fire.
    if moved!(bars_hidden) {
        for node in nodes.cockpit_hidden {
            set_visible(&mut w.vis, node, !next.bars_hidden);
        }
    }

    // -- the speed tape -----------------------------------------------------
    //
    // Two writes at most, and only while accelerating: the ladder's scroll and
    // the digits beside the index. Nothing on the ladder itself is ever touched.
    if moved!(spd_px) {
        set_scroll(&mut w.xform, nodes.spd.ladder, next.spd_px, SPD_TOP);
    }
    if moved!(spd) {
        set_text(&mut w.text, nodes.spd.value, || next.spd.to_string());
    }
    if moved!(thr_px) {
        set(
            &mut w.xform,
            nodes.spd.bug,
            UiTransform::from_translation(Val2::px(0.0, f32::from(next.thr_px))),
        );
    }

    // -- the hull tape ------------------------------------------------------
    if moved!(hull_px) {
        set_scroll(&mut w.xform, nodes.hull.ladder, next.hull_px, MAX_HP);
    }
    if moved!(hp) {
        set_text(&mut w.text, nodes.hull.value, || next.hp.to_string());
    }
    if moved!(hull_alert) {
        // The readout and the index take the alert colour together; the ladder
        // stays phosphor, because a scale that changed colour would read as a
        // different scale.
        let colour = next.hull_alert.colour();
        set(&mut w.text_colour, nodes.hull.value, TextColor(colour));
        set(&mut w.bg, nodes.hull.caret, BackgroundColor(colour));
    }

    // -- AGL / RNG ----------------------------------------------------------
    if moved!(alt) {
        set_visible(&mut w.vis, nodes.alt_row, next.alt.shown);
        if next.alt.shown {
            set_text(&mut w.text, nodes.alt_caption, || {
                if next.alt.agl { "AGL" } else { "RNG" }.to_owned()
            });
            set_text(&mut w.text, nodes.alt_value, || {
                (u32::from(next.alt.steps) * ALT_STEP as u32).to_string()
            });
            set(
                &mut w.text_colour,
                nodes.alt_value,
                TextColor(if next.alt.warn { WARN } else { PHOSPHOR }),
            );
            set_visible(&mut w.vis, nodes.pull_up, next.alt.warn);
            // A square wave, so two writes per 200 ms rather than sixty, and
            // none at all with the ground where it belongs.
            set(
                &mut w.text_colour,
                nodes.pull_up,
                TextColor(WARN.with_alpha(if next.alt.blink { 1.0 } else { 0.15 })),
            );
        }
    }

    // -- BST and GUN --------------------------------------------------------
    //
    // The pattern the DOM HUD got wrong, done right: each segment's own lit-ness
    // is compared against its own previous lit-ness, so a tenth of a tank writes
    // one segment. Rewriting all ten would still be cheap — the point is that
    // the shape does not degrade as the stack gets longer, which is exactly how
    // "8 writes per player" became 64.
    sync_stack(
        &mut w.bg,
        &nodes.boost_segs,
        prev.map(|p| p.boost_seg),
        next.boost_seg,
        FRIENDLY,
    );
    // The gun stack cannot use that diff, because its *colour* changes as well
    // as its count: a segment whose lit-ness did not move still has to be
    // repainted when the gun goes out, and one left red when it comes back
    // would stay red forever. Ten writes, only on a frame where the count, the
    // overheat or the pulse moved — and the pulse only advances while the gun
    // is actually out.
    if moved!(gun_seg, overheated, pulse) {
        if next.overheated {
            // Every segment, not just the lit ones. An out gun is an alarm, and
            // an empty stack is exactly what the glass looks like at rest — the
            // one state it must not be confused with.
            let alert = WARN.with_alpha(0.55 + 0.45 * pulse01(next.pulse));
            for &seg in &nodes.gun_segs {
                set(&mut w.bg, seg, BackgroundColor(alert));
            }
        } else {
            for (i, &seg) in nodes.gun_segs.iter().enumerate() {
                set(
                    &mut w.bg,
                    seg,
                    BackgroundColor(if i < next.gun_seg as usize {
                        PHOSPHOR
                    } else {
                        unlit()
                    }),
                );
            }
        }
    }
    if moved!(overheated) {
        // The legend says it in words, since an empty stack and a stack with
        // one segment left look much the same at a glance.
        set(
            &mut w.text_colour,
            nodes.gun_label,
            TextColor(if next.overheated { WARN } else { legend() }),
        );
    }

    // -- the brake-charge strip ---------------------------------------------
    if moved!(charge) {
        set_visible(
            &mut w.vis,
            nodes.charge_row,
            next.charge != ChargeState::Idle,
        );
    }
    if moved!(charge, charge_seg, pulse) {
        let colour = match next.charge {
            ChargeState::Overload => WARN.with_alpha(0.55 + 0.45 * pulse01(next.pulse)),
            ChargeState::Full => AMBER,
            _ => PHOSPHOR,
        };
        for (i, &seg) in nodes.charge_segs.iter().enumerate() {
            set(
                &mut w.bg,
                seg,
                BackgroundColor(if i < next.charge_seg as usize {
                    colour
                } else {
                    unlit()
                }),
            );
        }
    }

    // -- the EMP strip ------------------------------------------------------
    if moved!(emp_seg, emp_ready) {
        // Amber at full deflection, phosphor while it fills. The colour change
        // is the only announcement the weapon gets: there is no toast, no
        // chime, and nothing on the reticle, because an armed EMP is an option
        // rather than an event.
        let colour = if next.emp_ready { AMBER } else { PHOSPHOR };
        for (i, &seg) in nodes.emp_segs.iter().enumerate() {
            set(
                &mut w.bg,
                seg,
                BackgroundColor(if i < next.emp_seg as usize {
                    colour
                } else {
                    unlit()
                }),
            );
        }
    }
    if moved!(emp_ready) {
        set(
            &mut w.text_colour,
            nodes.emp_label,
            TextColor(if next.emp_ready { AMBER } else { legend() }),
        );
    }

    // -- the pulse ----------------------------------------------------------
    //
    // Three writes on the frame a pulse lands and three on the frame it clears.
    // Everything else the EMP switches off was already forced to its resting
    // value by `HudModel::unpowered`, so those guards see an unchanging zero
    // and none of them fire for the whole four seconds — the same discipline
    // the cockpit stand-down runs on.
    if moved!(blind) {
        set_visible(&mut w.vis, nodes.spd.root, !next.blind);
        set_visible(&mut w.vis, nodes.emp_row, !next.blind);
        set_visible(&mut w.vis, nodes.reticle_anchor, !next.blind);
        set_visible(&mut w.vis, nodes.emp_banner, next.blind);
    }
    if moved!(blind_blink) {
        set(
            &mut w.text_colour,
            nodes.emp_banner,
            TextColor(AMBER.with_alpha(if next.blind_blink { 1.0 } else { 0.15 })),
        );
    }

    // -- stores -------------------------------------------------------------
    sync_pips(
        &mut w.bg,
        &mut w.border,
        &nodes.missile_pips,
        prev.map(|p| p.missiles),
        next.missiles,
        ORDNANCE,
    );
    sync_pips(
        &mut w.bg,
        &mut w.border,
        &nodes.flare_pips,
        prev.map(|p| p.flares),
        next.flares,
        AMBER,
    );

    // -- the boresight ------------------------------------------------------
    if moved!(reticle_locked) {
        let colour = if next.reticle_locked { WARN } else { PHOSPHOR };
        set(&mut w.border, nodes.reticle, BorderColor::all(colour));
        for mark in nodes.reticle_marks {
            set(&mut w.bg, mark, BackgroundColor(colour));
        }
        set(
            &mut w.xform,
            nodes.reticle,
            if next.reticle_locked {
                UiTransform::from_scale(Vec2::splat(1.15))
            } else {
                UiTransform::IDENTITY
            },
        );
    }

    // -- the missile-lock warning -------------------------------------------
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
            TextColor(WARN.with_alpha(if next.lock_blink { 1.0 } else { 0.1 })),
        );
    }

    // -- the hit vignette ---------------------------------------------------
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

    // -- DESTROYED ----------------------------------------------------------
    if moved!(alive) {
        set_visible(&mut w.vis, nodes.death_banner, !next.alive);
    }

    // -- the scoreline ------------------------------------------------------
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

    // -- the killfeed -------------------------------------------------------
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
        // The row is about you, on one side or the other.
        set(
            &mut w.text_colour,
            slot.killer,
            TextColor(if row.killer == LOCAL_ID {
                AMBER
            } else {
                PHOSPHOR
            }),
        );
        set(
            &mut w.text_colour,
            slot.victim,
            TextColor(if row.victim == LOCAL_ID {
                WARN
            } else {
                legend()
            }),
        );
    }
}

/// Slides a ladder so its reading sits on the index.
///
/// The ladder runs top-down from `top` to zero and `px_up` is the reading in
/// pixels up from its bottom, so putting that reading on the middle of the
/// window is one subtraction. It lives here rather than in the model because it
/// is a statement about this tree's layout; the model's `spd_px` is a statement
/// about the aircraft, and stays one.
fn set_scroll(q: &mut Query<&mut UiTransform>, ladder: Entity, px_up: i16, top: i32) {
    let y = TAPE_H / 2.0 + f32::from(px_up) - top as f32 * TAPE_PPU;
    set(q, ladder, UiTransform::from_translation(Val2::px(0.0, y)));
}

/// The overheat / overload pulse as a `0..1` ramp.
fn pulse01(step: u8) -> f32 {
    f32::from(step) / f32::from(PULSE_STEPS)
}

/// Brings one segment stack in line with the count lit, writing only the
/// segments whose state actually flipped.
fn sync_stack(
    q_bg: &mut Query<&mut BackgroundColor>,
    segs: &[Entity],
    prev: Option<u8>,
    next: u8,
    lit: Color,
) {
    for (i, &seg) in segs.iter().enumerate() {
        let i = u8::try_from(i).unwrap_or(u8::MAX);
        let is_lit = i < next;
        // A segment whose lit-ness is unchanged is skipped outright.
        if prev.is_some_and(|p| (i < p) == is_lit) {
            continue;
        }
        set(
            q_bg,
            seg,
            BackgroundColor(if is_lit { lit } else { unlit() }),
        );
    }
}

/// Brings one pip row in line with the count remaining, writing only the pips
/// whose state actually flipped.
///
/// A spent pip keeps its footprint and loses its fill: the outline stays so the
/// row still says how many you started with, which is the whole reason a pip row
/// beats a number.
fn sync_pips(
    q_bg: &mut Query<&mut BackgroundColor>,
    q_border: &mut Query<&mut BorderColor>,
    pips: &[Entity],
    prev: Option<u8>,
    next: u8,
    full: Color,
) {
    for (i, &pip) in pips.iter().enumerate() {
        let i = u8::try_from(i).unwrap_or(u8::MAX);
        let is_empty = i >= next;
        if prev.is_some_and(|p| (i >= p) == is_empty) {
            continue;
        }
        set(
            q_bg,
            pip,
            BackgroundColor(if is_empty { Color::NONE } else { full }),
        );
        set(
            q_border,
            pip,
            BorderColor::all(if is_empty {
                full.with_alpha(0.35)
            } else {
                Color::NONE
            }),
        );
    }
}

// ---------------------------------------------------------------------------
// The world-space layer
// ---------------------------------------------------------------------------

/// How long the result card stays up before the menu comes back.
///
/// `main.js` leaves `#matchresult` up until the player dismisses it; a card
/// that waits for a click needs a pointer, and the pointer is grabbed in
/// flight. Timed instead, and long enough to read the score twice.
const MATCH_RESULT_SECS: f32 = 6.0;

/// The finished match, until the menu takes over.
///
/// The clock reaching zero used to do nothing at all: `tick.rs` emits
/// [`SimEvent::MatchEnded`] and no system in this client read it, so the HUD
/// sat at `0:00` with the bots still fighting.
#[derive(Resource, Default)]
struct MatchResult {
    /// What to print, or `None` when a match is in progress.
    outcome: Option<Outcome>,
    /// `Frame::time` when the match ended. `f64`, as `Frame::time` is.
    at: f64,
    /// Whether [`crate::ui::ReturnToLobby`] has been sent, so it is sent once
    /// rather than every frame of the wait.
    returned: bool,
}

/// The three things the card can say.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Outcome {
    #[default]
    Draw,
    Victory,
    Defeat,
}

impl Outcome {
    fn text(self) -> &'static str {
        match self {
            Outcome::Draw => "DRAW",
            Outcome::Victory => "VICTORY",
            Outcome::Defeat => "DEFEAT",
        }
    }

    fn colour(self) -> Color {
        match self {
            Outcome::Draw => pal::WHITE,
            Outcome::Victory => PHOSPHOR,
            Outcome::Defeat => WARN,
        }
    }
}

/// Watches for the end of the match, and hands back to the menu.
fn watch_match_end(
    frame: Res<SimFrame>,
    mut result: ResMut<MatchResult>,
    mut back: MessageWriter<crate::ui::ReturnToLobby>,
) {
    // A fresh match puts time back on the clock, which is the signal that this
    // card belongs to a match that is over rather than the one being played.
    if result.outcome.is_some() && frame.0.hud.match_timer > 1.0 {
        *result = MatchResult::default();
    }

    if result.outcome.is_none() {
        for event in &frame.0.events {
            let SimEvent::MatchEnded { winner } = *event else {
                continue;
            };
            // `winner` is the winning *team*; whether that is a victory depends
            // on which side the local pilot is on. A match with no local ship
            // — spectating a finished match, or a mode with no teams — reads as
            // a draw rather than inventing a defeat.
            // `ShipView::team` is `Team::index()` with `-1` for "no team"
            // (`tick.rs:1093`), so an unteamed pilot reads as a draw rather
            // than losing to a side they were never on.
            let mine = frame
                .0
                .ships
                .iter()
                .find(|s| s.flags.contains(ShipFlags::LOCAL))
                .map(|s| s.team)
                .filter(|t| *t >= 0);
            result.outcome = Some(match (winner, mine) {
                (None, _) | (_, None) => Outcome::Draw,
                (Some(w), Some(mine)) => {
                    if w.index() as i32 == mine {
                        Outcome::Victory
                    } else {
                        Outcome::Defeat
                    }
                }
            });
            result.at = frame.0.time;
            result.returned = false;
        }
        return;
    }

    if !result.returned && frame.0.time - result.at >= f64::from(MATCH_RESULT_SECS) {
        result.returned = true;
        back.write(crate::ui::ReturnToLobby);
    }
}

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
    mut sight: ResMut<Boresight>,
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
        hide_all(&mut applied, &mut q_vis, &nodes, &mut sight);
        return;
    };
    let Some(viewport) = camera.logical_viewport_size() else {
        hide_all(&mut applied, &mut q_vis, &nodes, &mut sight);
        return;
    };

    // The local ship, from the interpolated transform rather than the frame.
    let Some((_, me)) = ships.iter().find(|(root, _)| root.0 == LOCAL_ID) else {
        hide_all(&mut applied, &mut q_vis, &nodes, &mut sight);
        return;
    };

    // An EMP takes the target brackets, the callsigns and the lead rings with
    // the rest of the avionics — `BACKLOG.md` §2 names all three. This is the
    // same exit a missing camera takes, which is the right one: `hide_all` also
    // clears `Boresight`, so `sync_hud`'s reticle lock goes out with them
    // instead of staying red on a target the pilot can no longer see marked.
    //
    // Note what a blinded pilot still has here: the enemy ship itself, drawn in
    // the world by `scene.rs` and completely unaffected. They can see the
    // aircraft. What they have lost is everything this file was drawing *on top
    // of* it.
    if frame.0.hud.emp_blind > 0.0 {
        hide_all(&mut applied, &mut q_vis, &nodes, &mut sight);
        return;
    }

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
    sight.locked = locked_on.is_some();
    // The `RNG` half of the altitude block, published here because this is the
    // only system that knows which contact the boresight is nearest to. Only a
    // *boxed* contact counts: a ring three kilometres out is not a range you are
    // managing, and putting its distance under the hull tape would be noise.
    sight.range = best
        .filter(|(_, c)| c.boxed)
        .map_or(f32::INFINITY, |(_, c)| c.range);

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
            let colour = if now.assisted {
                HOSTILE
            } else {
                HOSTILE.with_alpha(0.7)
            };
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
fn hide_all(
    applied: &mut AppliedMarkers,
    q_vis: &mut Query<&mut Visibility>,
    nodes: &HudNodes,
    sight: &mut Boresight,
) {
    // Guarded rather than assigned: this runs on every frame in the lobby, and
    // an unconditional write through the `ResMut` would flag the resource
    // changed sixty times a second for a value that never moves.
    if sight.locked || sight.range.is_finite() {
        *sight = Boresight::default();
    }
    for (slot, was) in nodes.markers.iter().zip(&mut applied.0) {
        if was.shown {
            set_visible(q_vis, slot.boxes, false);
            set_visible(q_vis, slot.lead, false);
            *was = MarkerModel::default();
        }
    }
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

/// `display: none` in the markup, `Visibility` here — see the charge strip.
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
// - **No height query.** The `AGL` half of [`AltBlock`] calls
//   [`sim::ship::terrain_height`] straight, which is a lattice lookup and four
//   noise evaluations per frame on the render thread, for a number `sim::ship`
//   has already computed
//   this tick to decide whether the ship is still alive. One `f32` on `HudState`
//   would delete it, and the same field would let the cockpit grow a radar
//   altimeter without a second copy of the sum.
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

    /// [`model`] with the out-of-frame inputs at rest — no target lock, no
    /// killfeed, match still running, in space. The cases that care pass their
    /// own [`Env`].
    fn model(frame: &Frame, time: f32, seated: bool) -> HudModel {
        super::model(
            frame,
            Env {
                time,
                seated,
                ..Env::default()
            },
            &KillFeed::default(),
            None,
        )
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

    /// An EMP takes the whole glass, and the model says so in one comparison.
    #[test]
    fn a_pulse_darkens_every_instrument_at_once() {
        let lit = model(&frame(healthy()), 0.0, false);
        let dark = model(
            &frame(HudState {
                emp_blind: 4.0,
                ..healthy()
            }),
            0.0,
            false,
        );

        assert!(dark.blind);
        assert!(dark.bars_hidden, "the six bars go with everything else");
        // Every readout at its resting value: tapes parked, stacks empty, pips
        // spent, the range block down, the boresight unlocked.
        assert_eq!(dark.spd, 0);
        assert_eq!(dark.spd_px, 0);
        assert_eq!(dark.hp, 0);
        assert_eq!(dark.hull_px, 0);
        assert_eq!(dark.boost_seg, 0);
        assert_eq!(dark.gun_seg, 0);
        assert_eq!(dark.emp_seg, 0);
        assert_eq!(dark.missiles, 0);
        assert_eq!(dark.flares, 0);
        assert!(!dark.alt.shown);
        assert!(!dark.reticle_locked);
        assert!(!dark.lock_warning);
        // And none of that was already true, or the assertions above would pass
        // on a HUD that had never been lit.
        assert!(lit.spd_px != 0 || lit.hp != 0);
        assert_ne!(lit.gun_seg, 0);
        assert_ne!(lit.boost_seg, 0);
    }

    /// What the pulse must *not* take: the referee's half of the screen, and
    /// being hit.
    #[test]
    fn a_pulse_leaves_the_match_and_the_damage_flash_alone() {
        let f = Frame {
            ships: vec![ShipView {
                id: 1,
                flags: ShipFlags::LOCAL.with(ShipFlags::ALIVE),
                hit_flash: 1.0,
                ..Default::default()
            }],
            hud: HudState {
                emp_blind: 4.0,
                match_active: true,
                team_kills: [3, 5],
                match_timer: 61.0,
                ..healthy()
            },
            ..Default::default()
        };
        let dark = super::model(
            &f,
            Env::default(),
            &KillFeed::default(),
            Some(Outcome::Victory),
        );

        assert!(dark.match_on && dark.team0 == 3 && dark.team1 == 5);
        assert_eq!(dark.clock, 61);
        assert_eq!(dark.result, Some(Outcome::Victory));
        assert!(dark.vignette > 0, "you can still feel being shot");
        assert!(dark.alive, "and a blackout may never hide DESTROYED");
    }

    /// A blind frame has to be *constant*, or the diff churns for four seconds
    /// on state nobody can see. The blink is the only thing allowed to move.
    #[test]
    fn a_blind_frame_writes_nothing_but_the_legend() {
        let f = frame(HudState {
            emp_blind: 4.0,
            ..healthy()
        });
        let a = model(&f, 0.0, false);
        let b = model(&f, BLINK_HALF_PERIOD * 2.0, false);
        assert_eq!(a, b, "a whole blink period on, the model is identical");

        let half = model(&f, BLINK_HALF_PERIOD, false);
        assert_ne!(a.blind_blink, half.blind_blink);
        assert_eq!(
            HudModel {
                blind_blink: a.blind_blink,
                ..half
            },
            a,
            "and the blink is the only field the clock touches"
        );
    }

    /// The attacker's half: a strip that fills, and one colour change at the top
    /// of it.
    #[test]
    fn the_emp_strip_fills_and_says_when_it_is_armed() {
        let at = |charge: f32| {
            let m = model(
                &frame(HudState {
                    emp_charge01: charge,
                    ..healthy()
                }),
                0.0,
                false,
            );
            (m.emp_seg, m.emp_ready)
        };
        assert_eq!(at(0.0), (0, false), "a fresh spawn is not armed");
        assert_eq!(at(0.5), (EMP_SEGS as u8 / 2, false));
        // Rounded up, like every other stack here: a meter with something in it
        // must not read as empty.
        assert_eq!(at(0.001).0, 1);
        assert_eq!(at(1.0), (EMP_SEGS as u8, true));
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
        assert_ne!(before.hull_px, after.hull_px);
        assert_eq!(
            HudModel {
                hp: before.hp,
                hull_px: before.hull_px,
                ..after
            },
            before,
            "damage should not disturb any other field"
        );
    }

    /// The hull tape runs phosphor to amber to red, and the thresholds are the
    /// ones a pilot is told about rather than a gradient nobody can read a
    /// number off.
    #[test]
    fn the_hull_tape_escalates_as_the_hull_goes() {
        let at = |hp01: f32| {
            model(
                &frame(HudState {
                    hp: (hp01 * 100.0) as i32,
                    hp01,
                    ..healthy()
                }),
                0.0,
                false,
            )
            .hull_alert
        };
        assert_eq!(at(1.0), Alert::Ok);
        assert_eq!(at(HULL_CAUTION), Alert::Ok, "the threshold is exclusive");
        assert_eq!(at(HULL_CAUTION - 0.01), Alert::Caution);
        assert_eq!(at(HULL_WARNING - 0.01), Alert::Warning);
        assert_eq!(at(0.0), Alert::Warning);
    }

    /// A tape reading quantises to the pixel it will be drawn on, which is what
    /// stops a ship holding a speed from writing anything at all.
    #[test]
    fn tape_readings_quantise_to_a_pixel() {
        assert_eq!(tape_px(0.0), 0);
        assert_eq!(tape_px(100.0), (100.0 * TAPE_PPU) as i16);
        // Two speeds a hundredth of a unit apart land on the same pixel...
        assert_eq!(tape_px(40.00), tape_px(40.01));
        // ...and a whole unit apart do not, at two pixels per unit.
        assert_ne!(tape_px(40.0), tape_px(41.0));
        // Never negative, so the ladder cannot be scrolled off its own top.
        assert_eq!(tape_px(-5.0), 0);
    }

    /// The speed ladder covers everything the ship can actually do, derived
    /// from the rules rather than typed in — a needle off the end of its own
    /// tape is the one failure a tape must not have.
    #[test]
    fn the_speed_tape_covers_the_top_speed() {
        let ship = &Rules::DEFAULT.ship;
        let top = ship.max_throttle * ship.boost_factor + ship.brake_boost_bonus_max;
        assert!(f64::from(SPD_TOP) >= top, "{SPD_TOP} does not reach {top}");
        assert_eq!(SPD_TOP % SPD_MAJOR, 0, "the top of the scale is a numeral");
    }

    /// A segment stack rounds *up*, so "some left" never reads as empty.
    #[test]
    fn segment_stacks_never_round_the_last_one_away() {
        assert_eq!(segments(0.0, METER_SEGS), 0);
        assert_eq!(segments(1.0, METER_SEGS), METER_SEGS as u8);
        assert_eq!(segments(0.5, METER_SEGS), 5);
        // One round in ninety is a lit segment, not an empty stack.
        assert_eq!(segments(1.0 / 90.0, METER_SEGS), 1);
        // ...and quantised, so nine tenths of a tank and a hair more are one
        // reading and cost no write.
        assert_eq!(segments(0.901, METER_SEGS), segments(0.999, METER_SEGS));
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

    /// Seated in the cockpit the **bars** stand down and nothing else does.
    ///
    /// `index.html:865`–`:870` is the whole of `body.cockpit-view`: six
    /// elements, all of them duplicated by the 3D instrument panel. This test
    /// exists because the port originally hid the entire overlay, which took
    /// the reticle with it and left the cockpit with no crosshair.
    #[test]
    fn only_the_bars_stand_down_in_the_cockpit() {
        let mut f = frame(healthy());
        f.ships[0].flags = ShipFlags::LOCAL.with(ShipFlags::ALIVE);

        let out = model(&f, 0.0, false);
        let seated = model(&f, 0.0, true);

        assert!(out.present && seated.present, "the tree stays up in both");
        assert!(!out.bars_hidden, "third person shows the bars");
        assert!(seated.bars_hidden, "the cockpit hides them");

        // Every value behind a hidden bar is pinned to a constant, so the diff
        // cannot churn on state no one can see.
        assert_eq!(
            (
                seated.hp,
                seated.hull_px,
                seated.boost_seg,
                seated.gun_seg,
                seated.charge_seg,
                seated.missiles,
                seated.flares,
                seated.pulse
            ),
            (0, 0, 0, 0, 0, 0, 0, 0),
        );
        assert!(!seated.overheated);
        assert_eq!(seated.charge, ChargeState::Idle);
        assert_eq!(seated.hull_alert, Alert::Ok);

        // The speed tape is deliberately *not* on the list: the six elements
        // `body.cockpit-view` hides never included the speed readout, and an
        // aircraft shows airspeed on the glass and on the panel both.
        assert_eq!(seated.spd, out.spd);
        assert_eq!(seated.spd_px, out.spd_px);

        // And everything the JS keeps, this keeps. The match clock is the
        // readable proxy: it is driven from the same model and is not on the
        // hidden list.
        assert_eq!(seated.match_on, out.match_on);
        assert_eq!(seated.clock, out.clock);
        assert_eq!(seated.alive, out.alive);
    }

    /// The reticle in particular, since losing it is the reported bug.
    #[test]
    fn the_cockpit_keeps_the_reticle() {
        let mut f = frame(healthy());
        f.ships[0].flags = ShipFlags::LOCAL.with(ShipFlags::ALIVE);

        // `locked` is the third argument to the real `model`; the wrapper
        // passes `false`, so this asserts the reticle's *presence* rather than
        // its lock state — `present` is what gates the node's visibility.
        assert!(model(&f, 0.0, true).present);
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

    /// Why [`sync_hud`] drops `prev` on the first `present` frame.
    ///
    /// A stationary ship's model **equals `HudModel::default()`** in several
    /// fields, so a diff taken against the default writes none of them and the
    /// tree keeps whatever `spawn_hud` left there. This pins the collision that
    /// caused it, so the guard cannot be deleted as redundant.
    #[test]
    fn a_fresh_ship_reads_the_same_as_no_ship_in_the_fields_that_bit() {
        let live = model(&frame(healthy()), 0.0, false);
        let none = HudModel::default();

        assert_eq!(live.spd_px, none.spd_px, "a parked ship reads zero");
        assert_eq!(live.spd, none.spd);
        assert_eq!(live.alt, none.alt, "space with no contact is the default");
        assert_eq!(live.charge_seg, none.charge_seg);

        // `present` is the one field that always moves, which is exactly what
        // makes it the trigger the guard can hang off.
        assert_ne!(live.present, none.present);
    }

    /// The block under the hull tape is an altimeter on terrain and a
    /// rangefinder in space, and it is the map that decides — not a guess about
    /// which mode is running.
    #[test]
    fn the_altitude_block_follows_the_map() {
        let mut f = frame(healthy());
        // Well clear of the ground, and with a contact on the boresight.
        //
        // Not over the origin: since the Sierras rebuild the middle of the map
        // is a lake, and a test that wants to prove the readout *subtracts* the
        // ground needs somewhere the ground is not zero.
        f.ships[0].pos = [820.0, 900.0, -430.0];

        let space = super::model(
            &f,
            Env {
                map: MapKind::Space,
                range: 420.0,
                ..Env::default()
            },
            &KillFeed::default(),
            None,
        )
        .alt;
        assert!(space.shown && !space.agl, "space reads range");
        assert_eq!(space.steps, quantise(420.0));
        assert!(!space.warn, "there is no ground to be near");

        let terrain = super::model(
            &f,
            Env {
                map: MapKind::Terrain,
                range: 420.0,
                ..Env::default()
            },
            &KillFeed::default(),
            None,
        )
        .alt;
        assert!(terrain.shown && terrain.agl, "terrain reads height");
        assert!(!terrain.warn, "900 units up is not a ground warning");
        // Height *above ground*, not altitude — and the ground under the origin
        // is a long way up, which is the whole reason this is a radar altimeter
        // and not `pos.y`. Expected from `sim::ship` rather than assumed flat.
        let ground = sim::ship::terrain_height(820.0, -430.0, &Rules::DEFAULT);
        assert!(ground > 0.0, "the test point is not over land");
        assert_eq!(terrain.steps, quantise(900.0 - ground as f32));
    }

    /// With nothing on the boresight, space says nothing rather than zero.
    #[test]
    fn the_range_readout_is_absent_without_a_contact() {
        let alt = model(&frame(healthy()), 0.0, false).alt;
        assert!(!alt.shown);
        assert_eq!(alt, AltBlock::default(), "and is the constant model");
    }

    /// `PULL UP` is terrain-only, and its threshold is anchored to the floor
    /// that actually kills you rather than to a number picked here.
    #[test]
    fn the_ground_warning_is_terrain_only() {
        let floor = (Rules::DEFAULT.world.terrain_kill_clearance * GROUND_WARN_CLEARANCES) as f32;
        // Measured off the height field, not off zero: the ground under the
        // origin is several hundred units up, so an absolute altitude says
        // nothing about how close to it you are.
        let ground = sim::ship::terrain_height(0.0, 0.0, &Rules::DEFAULT) as f32;
        let agl = |clearance: f32, map| {
            let mut f = frame(healthy());
            f.ships[0].pos = [0.0, ground + clearance, 0.0];
            super::model(
                &f,
                Env {
                    map,
                    ..Env::default()
                },
                &KillFeed::default(),
                None,
            )
            .alt
        };

        let low = agl(floor * 0.5, MapKind::Terrain);
        assert!(low.warn, "half the warning height must warn");
        assert!(low.blink, "and the warning blinks");

        assert!(
            !agl(floor * 2.0, MapKind::Terrain).warn,
            "twice it must not"
        );
        // The same *height* in space is not a warning, because there is no
        // ground under it to hit.
        let in_space = agl(floor * 0.5, MapKind::Space);
        assert!(!in_space.warn && !in_space.blink);
    }

    /// The blink is forced still while the warning is off, or the model would
    /// differ every frame and the whole early-out would go with it.
    #[test]
    fn the_ground_warning_blink_is_still_when_clear() {
        let mut f = frame(healthy());
        // Well above the height field under the origin, so the warning is off.
        f.ships[0].pos = [
            0.0,
            sim::ship::terrain_height(0.0, 0.0, &Rules::DEFAULT) as f32 + 900.0,
            0.0,
        ];
        for time in [0.0, 0.1, 0.35, 2.0] {
            let alt = super::model(
                &f,
                Env {
                    time,
                    map: MapKind::Terrain,
                    ..Env::default()
                },
                &KillFeed::default(),
                None,
            )
            .alt;
            assert!(!alt.blink, "blink advanced at {time} with no warning");
        }
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
        let aimed = Env {
            locked: true,
            ..Env::default()
        };
        assert!(
            !super::model(&held, Env::default(), &KillFeed::default(), None).reticle_locked,
            "the assist holding a target is not a lock"
        );
        assert!(
            super::model(&held, aimed, &KillFeed::default(), None).reticle_locked,
            "being lined up on one is"
        );

        // And a corpse never locks, however well aimed.
        let mut dead = frame(healthy());
        dead.ships[0].flags = ShipFlags::LOCAL;
        assert!(!super::model(&dead, aimed, &KillFeed::default(), None).reticle_locked);
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

        let quiet = super::model(&f, Env::default(), &feed, None);
        let later = Env {
            time: 5.0,
            ..Env::default()
        };
        assert_eq!(quiet, super::model(&f, later, &feed, None));

        feed.push(2, 3, 0.0);
        let loud = super::model(&f, Env::default(), &feed, None);
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
