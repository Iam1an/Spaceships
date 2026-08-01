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
//! # What it is not
//!
//! `bevy_ui` has no CSS transitions, no `backdrop-filter`, and no
//! `mix-blend-mode`; where the CSS relies on those the port lands on the end
//! state rather than the animation, and the deviations are noted at each site.

use bevy::prelude::*;
use bevy::text::{FontSource, FontWeight, Justify, LetterSpacing};

use sim::rules::Rules;
use sim::world::{Frame, GunMode, HudState, ShipFlags};
use spaceships_sim as sim;

use crate::sim_bridge::{SimFrame, SimSet};

/// Wires the HUD in: one tree at startup, one diffing system per frame.
pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AppliedHud>()
            .add_systems(Startup, spawn_hud)
            // `SimSet` lives in `FixedUpdate` and this is `Update`, so the
            // ordering is nominal — the point is documentary, matching
            // `scene.rs`: the HUD reads whatever `SimFrame` the most recent
            // fixed tick left behind, and never runs before one exists.
            .add_systems(Update, sync_hud.after(SimSet));
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
}

/// The model [`sync_hud`] last wrote to the tree. `None` until the first frame,
/// which is what makes that frame write everything.
#[derive(Resource, Default)]
struct AppliedHud(Option<HudModel>);

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

/// Reduces one simulation frame to the pixels it implies.
///
/// Pure, and deliberately free of Bevy — which is what lets the tests below
/// assert the no-change property directly.
fn model(frame: &Frame, time: f32, seated: bool) -> HudModel {
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

        reticle_locked: hud.assist_target >= 0,
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
        // hit flash and the vignette's opacity are the same quantity, so the
        // HUD reads it off the local `ShipView` rather than needing its own.
        vignette: (me.hit_flash.clamp(0.0, 1.0) * VIGNETTE_STEPS).round() as u8,

        // `match_timer` alone is not the test: it holds the match's full
        // duration before the clock starts and in modes that have no clock,
        // which drew the multiplayer scoreline over the campaign.
        match_on: hud.match_active,
        team0: hud.team_kills[0],
        team1: hud.team_kills[1],
        clock: hud.match_timer.max(0.0).ceil() as u32,
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

    reticle: Entity,
    reticle_ticks: [Entity; 2],

    lock_warning: Entity,
    vignette: Entity,
    death_banner: Entity,

    match_panel: Entity,
    team0: Entity,
    team1: Entity,
    clock: Entity,
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
    let mut reticle = Entity::PLACEHOLDER;
    let mut reticle_ticks = [Entity::PLACEHOLDER; 2];
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

            // -- #reticle ---------------------------------------------------
            // Screen-centred, unlike the JS, which projects the aim ray's
            // impact point and writes `style.left`/`style.top` every frame
            // (`main.js:1839`). Reproducing that needs a world-space aim point,
            // which `HudState` does not carry — see the gap note at the end of
            // this file.
            reticle = hud
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: percent(50),
                        top: percent(50),
                        width: px(16),
                        height: px(16),
                        // `margin: -8px 0 0 -8px`.
                        margin: UiRect::new(px(-8), px(0), px(-8), px(0)),
                        border: UiRect::all(px(1)),
                        border_radius: BorderRadius::all(percent(50)),
                        ..default()
                    },
                    BorderColor::all(RETICLE_CYAN),
                    BoxShadow::new(rgba(102, 221, 255, 0.4), px(0), px(0), px(0), px(8)),
                    // `.locked` scales to 1.2. A `UiTransform` is applied after
                    // layout, so the lock state costs no relayout — where
                    // animating `width`/`height` would.
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
                    // `#reticle::after` — a 6x1 tick to the left of it. The
                    // crosshair really is asymmetric in the CSS; this is not a
                    // porting slip.
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

            // TODO(hud): the floating, world-space part of the HUD —
            // `.target-box`, `.target-label`, `.lead-marker` and `#killfeed` —
            // is not here.
            //
            // Not because projection is hard: `Camera::world_to_viewport` plus
            // an absolutely positioned node per target is a dozen lines, and
            // those nodes moving every frame would be legitimate, because
            // their value genuinely changes every frame. It is missing because
            // `Frame` carries no *identity*: `ShipView` has an `id`, a `team`
            // and flags, but no callsign, and `Frame` has no roster, so
            // `.target-label` and every killfeed row would read `"1"`. The
            // killfeed also needs the killer's and victim's names, and
            // `SimEvent::ShipDestroyed` carries `EntityId`s only.
            //
            // What unblocks all four at once is a name source on the frame —
            // `MatchState::scores` already holds `Score` rows and is simply not
            // copied into `Frame`. See the gap note at the end of this file.
            // `.lead-marker` additionally needs the aim-assist intercept point,
            // which `sim::world::AimAssistState` computes and `HudState`
            // reduces to a bare `assist_target: i32`.
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
        reticle,
        reticle_ticks,
        lock_warning,
        vignette,
        death_banner,
        match_panel,
        team0,
        team1,
        clock,
    });
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
/// towards the corners and leaves the centre clear. The stop *positions* and
/// the shape are unchanged, and the opacity ramp is still the hit flash.
fn vignette_gradient(alpha: f32) -> BackgroundGradient {
    BackgroundGradient::from(RadialGradient {
        // CSS's `ellipse at center` with no explicit extent is farthest-corner.
        shape: RadialGradientShape::FarthestCorner,
        position: UiPosition::CENTER,
        stops: vec![
            ColorStop::new(rgba(140, 0, 0, 0.0), percent(40)),
            ColorStop::new(rgba(120, 0, 0, 0.35 * alpha), percent(80)),
            ColorStop::new(rgba(90, 0, 0, 0.7 * alpha), percent(110)),
        ],
        ..default()
    })
}

// ---------------------------------------------------------------------------
// The diff
// ---------------------------------------------------------------------------

/// Writes the frame's HUD, and nothing that did not change.
///
/// The shape to keep: build the model, compare it whole, and return before
/// acquiring a single `Mut` if it matches. Everything after the early-out is
/// guarded by its own field comparison, so the cost of a frame is proportional
/// to what moved rather than to how many nodes exist.
#[expect(
    clippy::too_many_arguments,
    reason = "one query per component type written; splitting the system would \
              only split the single comparison that makes it cheap"
)]
fn sync_hud(
    frame: Res<SimFrame>,
    time: Res<Time>,
    view: Res<crate::cockpit::ViewMode>,
    lobby: Option<Res<crate::ui::LobbyOpen>>,
    nodes: Option<Res<HudNodes>>,
    mut applied: ResMut<AppliedHud>,
    mut q_node: Query<&mut Node>,
    mut q_vis: Query<&mut Visibility>,
    mut q_bg: Query<&mut BackgroundColor>,
    mut q_border: Query<&mut BorderColor>,
    mut q_grad: Query<&mut BackgroundGradient>,
    mut q_shadow: Query<&mut BoxShadow>,
    mut q_text: Query<&mut Text>,
    mut q_text_colour: Query<&mut TextColor>,
    mut q_xform: Query<&mut UiTransform>,
) {
    let Some(nodes) = nodes else { return };

    // The lobby covers the screen, so the flight overlay stands down behind it
    // for the same reason it stands down in the cockpit: something else is
    // already the interface.
    let hidden = view.seated || lobby.is_some_and(|l| l.0);
    let next = model(&frame.0, time.elapsed_secs(), hidden);
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
        set_visible(&mut q_vis, nodes.root, next.present);
    }
    if !next.present {
        // Nothing below is meaningful without a ship, and leaving the tree
        // untouched means a HUD that is off costs one comparison a frame.
        return;
    }

    // -- #healthbar ---------------------------------------------------------
    if moved!(hp_mil) {
        set_width(&mut q_node, nodes.health_fill, next.hp_mil);
    }
    if moved!(hp_hue) {
        set(&mut q_grad, nodes.health_fill, health_gradient(next.hp_hue));
    }
    if moved!(hp) {
        set_text(&mut q_text, nodes.health_text, || {
            format!("{} / {MAX_HP}", next.hp)
        });
    }

    // -- #boostbar ----------------------------------------------------------
    if moved!(boost_mil) {
        set_width(&mut q_node, nodes.boost_fill, next.boost_mil);
    }

    // -- #heatbar -----------------------------------------------------------
    if moved!(heat_mil) {
        set_width(&mut q_node, nodes.heat_fill, next.heat_mil);
    }
    if moved!(overheated) {
        set(
            &mut q_border,
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
            &mut q_shadow,
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
        set_width(&mut q_node, nodes.charge_fill, next.charge_mil);
    }
    if moved!(charge) {
        let overload = next.charge == ChargeState::Overload;
        set_visible(
            &mut q_vis,
            nodes.charge_frame,
            next.charge != ChargeState::Idle,
        );
        set(
            &mut q_border,
            nodes.charge_frame,
            BorderColor::all(match next.charge {
                ChargeState::Overload => ALERT_BORDER,
                ChargeState::Full => CHARGE_FULL_BORDER,
                _ => METER_BORDER,
            }),
        );
        set(&mut q_grad, nodes.charge_fill, charge_gradient(overload));
    }
    if moved!(charge, pulse) {
        set(
            &mut q_shadow,
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
        &mut q_bg,
        &mut q_border,
        &mut q_shadow,
        &nodes.missile_pips,
        prev.map(|p| p.missiles),
        next.missiles,
        MSL_PIP,
        MSL_PIP_EMPTY,
        MSL_PIP_EMPTY_BORDER,
    );
    sync_pips(
        &mut q_bg,
        &mut q_border,
        &mut q_shadow,
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
        set(&mut q_border, nodes.reticle, BorderColor::all(colour));
        for tick in nodes.reticle_ticks {
            set(&mut q_bg, tick, BackgroundColor(colour));
        }
        set(
            &mut q_shadow,
            nodes.reticle,
            if next.reticle_locked {
                BoxShadow::new(RED_BRIGHT, px(0), px(0), px(0), px(12))
            } else {
                BoxShadow::new(rgba(102, 221, 255, 0.4), px(0), px(0), px(0), px(8))
            },
        );
        set(
            &mut q_xform,
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
        set_visible(&mut q_vis, nodes.lock_warning, next.lock_warning);
    }
    if moved!(lock_blink) {
        // `@keyframes msl-lock-blink { 0%,49% { opacity: 1 } 50%,100% { 0.1 } }`
        // — a square wave, so two writes per 250 ms rather than sixty, and none
        // at all when nothing has a lock on you.
        set(
            &mut q_text_colour,
            nodes.lock_warning,
            TextColor(RED_BRIGHT.with_alpha(if next.lock_blink { 1.0 } else { 0.1 })),
        );
    }

    // -- #hit-vignette ------------------------------------------------------
    if moved!(vignette) {
        set_visible(&mut q_vis, nodes.vignette, next.vignette > 0);
        if next.vignette > 0 {
            set(
                &mut q_grad,
                nodes.vignette,
                vignette_gradient(f32::from(next.vignette) / VIGNETTE_STEPS),
            );
        }
    }

    // -- #deathbanner -------------------------------------------------------
    if moved!(alive) {
        set_visible(&mut q_vis, nodes.death_banner, !next.alive);
    }

    // -- #matchhud ----------------------------------------------------------
    if moved!(match_on) {
        set_visible(&mut q_vis, nodes.match_panel, next.match_on);
    }
    if next.match_on {
        if moved!(team0) {
            set_text(&mut q_text, nodes.team0, || next.team0.to_string());
        }
        if moved!(team1) {
            set_text(&mut q_text, nodes.team1, || next.team1.to_string());
        }
        if moved!(clock) {
            set_text(&mut q_text, nodes.clock, || fmt_clock(next.clock));
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
// - **No roster.** `Frame` has no player names. `MatchState::scores` already
//   holds them; copying that into `Frame` (or adding a name to `ShipView`) is
//   what unblocks `.target-label`, `#killfeed` and `#scoreboard` at once.
// - **`assist_target` is only an id.** `AimAssistState` computes an intercept
//   point that `.lead-marker` needs, and `HudState` keeps only the target id.
//   The same reduction means the reticle cannot follow the aim ray's impact
//   point the way `main.js:1839` does, and is drawn at screen centre.
//
// Separately, and not a `HudState` problem: `sim_bridge::tick` does not yet
// populate `overcharge01`, `missile_lock_warning` or `assist_target` — the
// projectile, missile and aim-assist systems it calls out as missing are what
// would fill them. The fields exist and are read here, so `.overload`, the lock
// warning and the locked reticle light up the moment that lands, with no change
// to this file.

#[cfg(test)]
mod tests {
    use super::*;
    use sim::world::ShipView;

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

    /// The vignette reads the local ship's hit flash, which is the same
    /// quantity `main.js:1942` copies into `camTel.hitFlash`.
    #[test]
    fn the_vignette_follows_the_hit_flash() {
        let mut f = frame(healthy());
        assert_eq!(model(&f, 0.0, false).vignette, 0);
        f.ships[0].hit_flash = 1.0;
        assert_eq!(model(&f, 0.0, false).vignette, VIGNETTE_STEPS as u8);
        f.ships[0].hit_flash = 0.5;
        assert_eq!(model(&f, 0.0, false).vignette, VIGNETTE_STEPS as u8 / 2);
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
}
