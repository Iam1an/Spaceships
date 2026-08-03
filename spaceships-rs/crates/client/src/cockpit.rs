//! The first-person cockpit: interior, live instruments, and the seated camera.
//!
//! A port of `public/src/cockpit.js` (interior geometry and the two ship
//! profiles), `public/src/dash.js` (radar, annunciators, gauges) and
//! `public/src/fpcamera.js` (the seated camera with free-look), plus the view
//! switching `main.js` spreads across `setViewMode`, `inCockpit`,
//! `applyExteriorMode` and `syncShipVisibility`.
//!
//! Press `V` to sit down. Hold the right mouse button for free-look, `Alt` to
//! look back over your shoulder, `G` to fire a test EMP at yourself.
//!
//! # The instruments are geometry, not a canvas
//!
//! This is the one deliberate departure from the JS, and it is why this module
//! is not a transliteration.
//!
//! `dash.js` draws each instrument into a `CanvasTexture` and re-uploads it
//! when a memoisation key changes. Two of the three screens memoise honestly
//! and cost nothing while the numbers hold still. The radar does not:
//!
//! ```text
//! // dash.js:145
//! if (keyOnly) return `${sweep.toFixed(3)}:${(s.contacts ?? []).length}`;
//! ```
//!
//! `sweep` advances every frame, so the key changes every frame, so a 256x128
//! RGBA canvas is re-uploaded to the GPU every frame the pilot is in the
//! cockpit — 128 kB a frame, ~18 MB/s at 144 Hz, to animate one rotating line.
//! It is not a bug you can memoise your way out of: the sweep genuinely does
//! change every frame.
//!
//! So nothing here is a texture. Every instrument is built once out of meshes:
//!
//! - the radar is a static ring/reticle mesh, a sweep arm that is one entity
//!   with a `Transform` rotation, and a **fixed pool** of blip quads that are
//!   moved and hidden rather than allocated;
//! - the gauges are quads whose `Transform.scale.x` is the bar fill;
//! - every caption (`SPD`, `HULL`, `MSL`, `TGT`…) is a 3x5 bitmap font baked
//!   into one static mesh per word — see [`caption_mesh`].
//!
//! Steady-state GPU upload from this module is **zero bytes**: no image is ever
//! written, and [`drive_instruments`] compares a quantised [`Readout`] against
//! the last one it applied and returns without touching an entity when nothing
//! the pixels could show has changed — the same trick `hud.rs` plays on
//! `bevy_ui`, for the same reason.
//!
//! # One switch for the EMP
//!
//! `BACKLOG.md` section 2 specs an EMP that takes a pilot's *information* away
//! and leaves them their hands: cockpit lighting, instrument panel, radar and
//! annunciators dead for a few seconds. That is [`CockpitPower`], and it is
//! deliberately not something to retrofit.
//!
//! Every self-illuminated material in here is registered in [`Rig::lit`] with
//! the emissive colour it has when powered, and both interior lamps carry
//! [`CockpitLamp`]. [`apply_power`] multiplies the lot by one `level` float.
//! Nothing else in the module knows the EMP exists; adding the weapon means
//! calling [`CockpitPower::emp`] where its event arrives, next to the
//! `AudioCommands::stop_warnings` that is already there for the same reason.
//!
//! # Where the camera comes from
//!
//! There is exactly one camera that draws the *match* — `camera.rs`'s, carrying
//! the whole Ultra stack (HDR, bloom, ACES, vignette), the environment map
//! `skybox.rs` hangs on it, and the render target `hud.rs` resolves as
//! `bevy_ui`'s default. Another camera for the flight view would have to
//! duplicate all of that and would make the "highest-order camera" pick
//! ambiguous, so [`seat_camera`] moves the one that exists.
//!
//! It is no longer the only `Camera3d` in the world, though, and that mattered:
//! `ui.rs` runs one off-screen for the menu's rotating ship, and both queries
//! here used to say `With<Camera3d>` and ask for a single match. Two cameras
//! make that an error, not a choice, so `V` hid the HUD and the canopy and then
//! left the view in the chase camera — the whole cockpit, invisible from
//! inside. Both now filter on [`FlightCamera`].
//!
//! Doing that safely is the subtle part. `camera.rs::follow` writes the chase
//! pose in `PostUpdate` `.before(TransformSystems::Propagate)`, and it is
//! private, so this module cannot order itself against it by name.
//! [`seat_camera`] runs `.after(TransformSystems::Propagate)` instead, which
//! orders it after `follow` *transitively* and cannot be broken by an edit to
//! `camera.rs` — and, because propagation has already run by then, it writes
//! `GlobalTransform` as well as `Transform`. It stays
//! `.before(VisibilitySystems::UpdateFrusta)` so the culling later in the same
//! schedule sees this frame's camera rather than last frame's.

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::VisibilitySystems;
use bevy::gltf::{GltfMaterialName, GltfMeshName};
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::Face;

use spaceships_sim as sim;

use crate::audio::AudioCommands;
use crate::camera::FlightCamera;
use crate::replay::ViewTarget;
use crate::scene::ShipRoot;
use crate::sim_bridge::SimFrame;
use sim::world::EntityId;

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

/// Where the pilot sits in a given hull, and how big the tub around them is.
///
/// Authored in **unfitted ship** units, exactly as `cockpit.js` authors them:
/// the model's own geometry, turned into ship axes but not otherwise moved. The
/// glTF is rotated `-PI/2` about Y by `scene.rs` (as `ship.js:45` does), which
/// maps model `(x, y, z)` to ship `(-z, y, x)`: ship forward is `+z`, and the
/// pilot's right is therefore `-x`.
///
/// [`CockpitProfile::fitted`] then applies `scene::ship_fit` — the same scale
/// and nose-ward shift the hull itself gets — so the anchors below can be read
/// straight off a measurement of the glb. For `spaceship.glb` and
/// `spaceshipADMIN.glb` that fit is the identity and the two spaces coincide,
/// which is why the numbers in the first two profiles are unchanged.
///
/// The JS scales the whole ship group by `SHIP_SCALE = 1.5` and these anchors
/// ride along with it; `scene.rs` leaves the ship root at unit scale, so the
/// numbers transfer unchanged. What matters is that the interior and the hull
/// share one space, and they do in both clients.
///
/// The interior is built larger than the real canopy glass. That is safe
/// because the exterior hull and the interior are never seen from the same
/// side — see [`sync_hull`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CockpitProfile {
    /// Eye point, ship-local.
    pub eye: Vec3,
    /// Vertical field of view, degrees. Wider than the chase camera's on
    /// purpose: a cockpit that fills the screen needs the peripheral vision.
    pub fov_deg: f32,
    /// Half-width of the tub.
    hw: f32,
    /// Floor height.
    floor_y: f32,
    /// Canopy rail: the top of the solid tub sides. Everything above it is open
    /// bubble carried on two thin hoops — a boxed-in roof and full-height walls
    /// are what made the JS's first pass feel like a grey crate.
    rail_y: f32,
    /// Rear bulkhead.
    back_z: f32,
    /// Instrument panel plane.
    dash_z: f32,
    /// Top of the panel; the canopy rail lines up with it.
    dash_top: f32,
    /// Strip lighting and bezel colour, `#rrggbb`.
    accent: u32,
    /// Interior lamp colour, `#rrggbb`.
    lamp: u32,
}

/// `spaceship.glb` (`Cockpit` node: ship-local x -0.5..0.5, y -0.1..1.2,
/// z 0.1..2.6).
///
/// Seated above the fuselage spine but well back in the blister, so the nose
/// runs out ahead of the panel and the wing roots sit in peripheral view. The
/// rail is dropped further below the eye than the panel top, to open the sides.
const DEFAULT_PROFILE: CockpitProfile = CockpitProfile {
    eye: Vec3::new(0.0, 1.26, 0.40),
    fov_deg: 84.0,
    hw: 0.60,
    floor_y: 0.64,
    rail_y: 0.92,
    back_z: -0.70,
    dash_z: 1.50,
    dash_top: 1.04,
    accent: 0x5fd8ff,
    lamp: 0x9fd0ff,
};

/// `spaceshipADMIN.glb` (`Cylinder.002`: ship-local y 0.6..0.9, z 3.2..4.6).
///
/// Two hull facts constrain this one hard: the spine cylinder (z -1.9..6.9, top
/// y 0.9) passes straight through the cockpit, so dropping the floor to see
/// more ship punches the spine through the footwell; and the only structure
/// forward of z 4.6 is that same thin spine, so there is very little ship ahead
/// of the seat to look at in the first place.
const ADMIN_PROFILE: CockpitProfile = CockpitProfile {
    eye: Vec3::new(0.0, 1.16, 4.15),
    fov_deg: 86.0,
    hw: 0.74,
    floor_y: 0.54,
    rail_y: 0.86,
    back_z: 3.05,
    dash_z: 5.15,
    dash_top: 0.94,
    accent: 0xffc451,
    lamp: 0xffd39a,
};

/// `jet.glb` (`Accent_Cockpit`: unfitted ship x ±0.268, y -0.192..0.154,
/// z 0.780..1.974; the bubble's centreline crown is y 0.154 at z 1.43).
///
/// This airframe is a real aircraft and its canopy is a *cut-out patch of the
/// upper skin*, not a blister sitting proud of a fuselage, so there is far less
/// room than the first two profiles have: the bubble is 0.54 across and 0.35
/// deep where the other is 1.0 across. Everything here is therefore measured
/// off the canopy rather than scaled from the default — the eye sits 0.03 under
/// the crown, the panel plane stops 0.07 short of the windscreen bow, and the
/// bulkhead lands on the canopy's rear edge.
///
/// The tub is 0.284 half-width against the glass's 0.268 — proud by the same
/// ratio the default profile is, and for the same reason.
const JET_PROFILE: CockpitProfile = CockpitProfile {
    // Set aft of the bubble's crown rather than under it. `build_interior`
    // plants the stick a fixed 0.30 ahead of the eye, so on a cockpit this
    // short the panel closes to within twice that and the grip rises straight
    // through the radar scope. Moving the seat back is what re-opens the angle
    // between them; the crown is 0.03 above the eye either way.
    eye: Vec3::new(0.0, 0.121, 1.300),
    fov_deg: 84.0,
    // `hw` is the arch radius as well as the tub's half-width, and the crown
    // therefore sits `rail_y + hw` above the floor. Held out past the glass by
    // more than the other two profiles hold theirs, because on a canopy this
    // shallow the difference is the only headroom the pilot gets.
    hw: 0.320,
    floor_y: -0.262,
    rail_y: -0.089,
    back_z: 0.767,
    // Low enough that the sightline over it clears the nose deck, which falls
    // away to y -0.10 by the radome: this hull's whole forward view *is* its
    // own nose, and a panel authored to the default's proportions covered it.
    dash_top: -0.030,
    dash_z: 1.909,
    accent: 0x5fd8ff,
    lamp: 0x9fd0ff,
};

impl CockpitProfile {
    /// Lifts anchors authored against the model's own geometry into the ship
    /// space the interior is actually built in.
    ///
    /// `scene::ship_fit` reduces the whole hull fit to `q' = scale * (q +
    /// offset)` in ship space, so a point transforms whole, a plane coordinate
    /// transforms on its own axis, and a half-width is simply scaled. Nothing
    /// else in the profile is a length: the field of view and the two colours
    /// pass through.
    ///
    /// This is the piece that was missing. `jet.glb` is fitted at 1.62 and
    /// shifted 3.2 units nose-ward, and a cockpit that ignored that put the
    /// pilot's head out among the tailfins with the panel buried in the
    /// fuselage — the hull moves, the seat does not.
    ///
    /// Test-only now that [`interior_transform`] carries the fit. It is kept
    /// rather than deleted because it is a *second, independent* statement of
    /// the same arithmetic, written before the transform was: the test that
    /// checks the two agree is only worth anything while they are written out
    /// separately.
    #[cfg(test)]
    const fn fitted(self, scale: f32, offset: Vec3) -> CockpitProfile {
        CockpitProfile {
            eye: Vec3::new(
                scale * (self.eye.x + offset.x),
                scale * (self.eye.y + offset.y),
                scale * (self.eye.z + offset.z),
            ),
            hw: self.hw * scale,
            floor_y: scale * (self.floor_y + offset.y),
            rail_y: scale * (self.rail_y + offset.y),
            back_z: scale * (self.back_z + offset.z),
            dash_z: scale * (self.dash_z + offset.z),
            dash_top: scale * (self.dash_top + offset.y),
            ..self
        }
    }
}

/// Which hull the pilot is strapped into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hull {
    /// `spaceship.glb`.
    Default,
    /// `spaceshipADMIN.glb`.
    Admin,
    /// `jet.glb`.
    Jet,
}

/// `getCockpitProfile(isAdmin)`, plus the hull `scene.rs` is actually flying.
///
/// The admin flag is `SPACESHIPS_ADMIN_COCKPIT`, which exists so that profile
/// can be looked at at all; when the client learns to pick a hull it becomes
/// the `isLocalAdmin` the JS computes from the pilot name. The jet comes from
/// `SPACESHIPS_SHIP_MODEL`, read through `scene::ship_model` so there is one
/// answer to "which model is flying" rather than two.
fn hull() -> Hull {
    if admin_hull() {
        Hull::Admin
    } else if crate::scene::ship_model().contains("jet") {
        // The same substring test `scene::model_fit` splits on, so the profile
        // and the fit can never disagree about which hull this is.
        Hull::Jet
    } else {
        Hull::Default
    }
}

/// Whether to sit in the admin hull.
///
/// Kept as its own function rather than folded into [`hull`] with a `cfg` so
/// that `Hull::Admin` is constructed in code the compiler sees on **both**
/// targets — a `cfg`'d early return leaves the variant provably unreachable on
/// wasm, and dead code there is a warning.
fn admin_hull() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::env::var_os("SPACESHIPS_ADMIN_COCKPIT").is_some()
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}

/// The profile for a hull, in the space the interior is **built** in.
///
/// Deliberately *unfitted*. The fit used to be applied here, and that was
/// wrong: `build_interior` measures a great deal of the cockpit in literal
/// constants rather than off the profile — the stick sits `eye.z + 0.30`
/// forward, its shaft is 0.24 long, the pedals are 0.115 by 0.135 — and those
/// literals do not go through `fitted`. Scaling the profile alone therefore
/// pulled the tub away from the furniture inside it: at the jet's 2.43 the
/// stick ended up a third of the distance from the seat it should have been,
/// which is to say inside the pilot and out of frame, and the consoles no
/// longer reached the side walls, which is the gap either side of the panel.
///
/// So the fit goes on the interior's **root transform** instead, and every
/// number below it — profile and literal alike — lives in one unscaled space
/// and scales together. That is also exactly what the JS does: the cockpit
/// group is a child of the ship group, and `main.js:219` scales the group.
///
/// The one thing that still needs the fit explicitly is the seated camera,
/// because it is not parented to the interior; see [`Rig::eye`].
fn profile(hull: Hull) -> CockpitProfile {
    match hull {
        Hull::Default => DEFAULT_PROFILE,
        Hull::Admin => ADMIN_PROFILE,
        Hull::Jet => JET_PROFILE,
    }
}

/// How much to shrink the hand-authored furniture — stick, throttle — for the
/// hull being flown.
///
/// The tub is described by a profile and so follows the aircraft; the furniture
/// inside it is not. `build_interior` writes the stick as a 0.24 shaft under a
/// 0.15 grip, and those numbers were authored against `spaceship.glb`, whose
/// fit is the identity. A pilot's hand is the same size in every cockpit, so
/// what has to hold constant is the furniture's size *after* the fit — which
/// means dividing by the fit the interior root is about to multiply by.
///
/// [`crate::scene::SHIP_SCALE`] over the hull's own fit: 1 for the two original
/// hulls, and about 0.62 for `jet.glb`, which is authored small and scaled up
/// 1.62 to match. Without it the jet gets a stick two and a half times life
/// size — a black column through the middle of the windscreen.
fn furniture_scale() -> f32 {
    crate::scene::SHIP_SCALE / crate::scene::ship_fit().0
}

/// The interior root's transform: `scene::ship_fit` as a `Transform`.
///
/// `ship_fit` describes `q' = scale * (q + offset)`, and a `Transform` applies
/// `scale * q + translation`, so the translation is `scale * offset`.
fn interior_transform() -> Transform {
    let (scale, offset) = crate::scene::ship_fit();
    Transform::from_translation(offset * scale).with_scale(Vec3::splat(scale))
}

// ---------------------------------------------------------------------------
// Tuning
// ---------------------------------------------------------------------------

const DEG: f32 = std::f32::consts::PI / 180.0;

/// Arena scale: the motherships sit at z ±600, so the JS's first 500-unit scope
/// showed an empty screen for most of a match (`main.js:437`).
const RADAR_RANGE: f32 = 1200.0;

/// How many contacts the scope can show at once. A fixed pool, because the
/// point of drawing the radar with geometry is that a frame never allocates.
/// Ten players plus bots is the whole match.
const MAX_BLIPS: usize = 12;

/// Radians per second the sweep arm turns (`dash.js:230`).
const SWEEP_RATE: f32 = 2.2;

/// Free-look sensitivity, radians per pixel of mouse travel.
const LOOK_SENSITIVITY: f32 = 0.0026;

const MAX_YAW: f32 = 110.0 * DEG;
const MAX_PITCH: f32 = 70.0 * DEG;
const LOOK_BACK_YAW: f32 = 180.0 * DEG;

/// How far the head drifts into a turn at full steer deflection.
const AUTO_LEAN_YAW: f32 = 12.0 * DEG;
const AUTO_LEAN_PITCH: f32 = 7.0 * DEG;

/// Near plane while seated. The chase camera's 0.5 would slice the pilot's own
/// instrument panel in half; it is restored on the way out.
const COCKPIT_NEAR: f32 = 0.02;

/// Seconds the panel takes to come back up after an EMP expires. Not instant: a
/// reboot is the part the victim gets to watch.
const REBOOT_SECS: f32 = 0.6;

/// Names that mean "canopy glass" rather than "hull" — `CANOPY_RE`
/// (`main.js:445`), as a word list because this crate has no regex.
const CANOPY_WORDS: [&str; 5] = ["cockpit", "canopy", "glass", "windshield", "window"];

/// Full-scale deflection for the `SPD` gauge: throttle, boost and a fully
/// charged brake-release. Derived from `rules.rs` rather than guessed.
const TOP_SPEED: f32 = {
    let s = sim::rules::Rules::DEFAULT.ship;
    (s.max_throttle * s.boost_factor + s.brake_boost_bonus_max) as f32
};

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

pub struct CockpitPlugin;

impl Plugin for CockpitPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ViewMode>()
            .init_resource::<CockpitPower>()
            .init_resource::<Head>()
            .init_resource::<Applied>()
            .init_resource::<HullMode>()
            // Chained: the cockpit has to exist before anything drives it, and
            // the view has to be settled before the head, the instruments and
            // the hull read it. Seven small systems in a fixed order beat seven
            // ordering constraints.
            .add_systems(
                Update,
                (
                    build_cockpit,
                    reseat_interior,
                    toggle_view,
                    tick_power,
                    drive_head,
                    drive_instruments,
                    apply_power,
                    sync_hull,
                )
                    .chain(),
            )
            // See the module docs: `.after(Propagate)` is what orders this
            // after `camera.rs`'s private `follow` without naming it, and is
            // why `GlobalTransform` is written here by hand.
            .add_systems(
                PostUpdate,
                seat_camera
                    .after(TransformSystems::Propagate)
                    .before(VisibilitySystems::UpdateFrusta),
            );
    }
}

// ---------------------------------------------------------------------------
// View mode
// ---------------------------------------------------------------------------

/// Which camera the player is looking through.
///
/// `main.js` persists this in `localStorage['spaceships:viewMode']`, which the
/// display's `COCKPIT VIEW` row stands in for: `ui.rs`'s `mirror_cockpit_flag`
/// keeps [`ViewMode::first_person`] and that row equal in **both** directions,
/// so the menu switch actually seats the pilot and a `V` pressed in flight is
/// what the row reads when they open the display again. This resource is the
/// authority; the row is a view of it.
///
/// `SPACESHIPS_COCKPIT` seeds it, which exists for the same reason
/// `SPACESHIPS_SCREENSHOT` does: so a visual check can capture this view
/// without anyone being there to press `V`. It is a starting value and not an
/// override — once the app is running, `V` and the menu row both move it.
#[derive(Resource)]
pub struct ViewMode {
    /// What `V` last selected.
    pub first_person: bool,
    /// `inCockpit()` (`main.js:427`): first person **and** alive. A dead pilot
    /// always watches in third person — the cockpit is hidden with the wreck —
    /// and this is the flag every other system in the module reads.
    pub seated: bool,
}

impl Default for ViewMode {
    fn default() -> Self {
        ViewMode {
            #[cfg(not(target_arch = "wasm32"))]
            first_person: std::env::var_os("SPACESHIPS_COCKPIT").is_some(),
            #[cfg(target_arch = "wasm32")]
            first_person: false,
            seated: false,
        }
    }
}

/// Head state for the seated camera: `fpcamera.js`'s instance fields.
///
/// The ship's heading is never driven by head movement. Steering leans the head
/// into the turn automatically; the right mouse button unlocks a clamped
/// free-look that damps back to boresight on release.
#[derive(Resource, Default)]
struct Head {
    yaw: f32,
    pitch: f32,
    lean_yaw: f32,
    lean_pitch: f32,
    shake_t: f32,
    shake_amp: f32,
}

/// What the pilot's hands are asking the head to do this frame.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Look {
    /// `Alt` — over the shoulder.
    Back,
    /// Right mouse held, carrying this frame's travel in pixels.
    Free(Vec2),
    /// Neither, so the head damps home.
    Boresight,
}

impl Head {
    /// `snap()`: centre the head, kill the shake.
    fn snap(&mut self) {
        *self = Head::default();
    }

    /// One frame of head movement — the whole of `FirstPersonCamera.update`
    /// except reading the hardware.
    ///
    /// Split out from [`drive_head`] and left free of Bevy on purpose. The
    /// failure modes in here are all arithmetic — a lean that never returns to
    /// centre, a free-look clamp applied to the wrong axis, damping that is not
    /// frame-rate independent — and every one of them is a still frame away
    /// from looking correct. The tests at the foot of the module drive this
    /// directly instead.
    fn update(&mut self, look: Look, steer: (f32, f32), boosting: bool, hit_flash: f32, dt: f32) {
        match look {
            Look::Back => {
                self.yaw = damp(self.yaw, LOOK_BACK_YAW, 9.0, dt);
                self.pitch = damp(self.pitch, 0.0, 9.0, dt);
            }
            Look::Free(delta) => {
                self.yaw -= delta.x * LOOK_SENSITIVITY;
                self.pitch -= delta.y * LOOK_SENSITIVITY;
                self.yaw = self.yaw.clamp(-MAX_YAW, MAX_YAW);
                self.pitch = self.pitch.clamp(-MAX_PITCH, MAX_PITCH);
            }
            // Damped return to boresight.
            Look::Boresight => {
                self.yaw = damp(self.yaw, 0.0, 7.0, dt);
                self.pitch = damp(self.pitch, 0.0, 7.0, dt);
            }
        }

        // Automatic lean into the turn, from the ship's effective steering.
        let (sx, sy) = steer;
        self.lean_yaw = damp(self.lean_yaw, -sx * AUTO_LEAN_YAW, 5.0, dt);
        self.lean_pitch = damp(self.lean_pitch, -sy * AUTO_LEAN_PITCH, 5.0, dt);

        // Airframe shake: a constant rumble under boost, plus a decaying kick
        // on damage. `ShipView::hit_flash` is the same 1 -> 0 envelope the
        // damage vignette rides.
        self.shake_t += dt;
        self.shake_amp = if boosting { 0.0030 } else { 0.0 } + hit_flash * 0.014;
    }
}

/// `setViewMode` plus the `myAlive` half of `inCockpit`.
fn toggle_view(
    keys: Res<ButtonInput<KeyCode>>,
    frame: Res<SimFrame>,
    target: Res<ViewTarget>,
    free: Res<crate::replay::FreeCam>,
    mut view: ResMut<ViewMode>,
    mut head: ResMut<Head>,
    // `FlightCamera` and not `Camera3d`: the menu's ship preview is a second
    // one, and `single_mut` over two cameras returns an error rather than a
    // camera. See the marker's own note — this query silently matching nothing
    // is what broke the cockpit.
    mut cam: Query<&mut Projection, With<FlightCamera>>,
    mut third_person: Local<Option<PerspectiveProjection>>,
) {
    if keys.just_pressed(KeyCode::KeyV) {
        view.first_person = !view.first_person;
    }

    let alive = seated_ship(&frame.0, target.0)
        .is_some_and(|s| s.flags.contains(sim::world::ShipFlags::ALIVE));
    // A canopy drawn around a camera that has left the aircraft is a box in the
    // middle of the map, so free flight is third person by construction — the
    // same rule as being dead, for the same reason.
    let seated = view.first_person && alive && !free.active;
    if seated == view.seated {
        return;
    }
    view.seated = seated;
    head.snap();

    // FOV and near plane belong to the camera, not to the cockpit, so they are
    // swapped once here rather than re-asserted every frame the way
    // `fpcamera.js` has to — `warp.js` restores a captured `baseFov` behind its
    // back, and this client has no such thing.
    let Ok(mut projection) = cam.single_mut() else {
        return;
    };
    let Projection::Perspective(persp) = &mut *projection else {
        return;
    };
    let saved = third_person.get_or_insert_with(|| persp.clone());
    if seated {
        persp.fov = profile(hull()).fov_deg * DEG;
        persp.near = COCKPIT_NEAR;
    } else {
        *persp = saved.clone();
    }
}

// ---------------------------------------------------------------------------
// Power — the EMP switch
// ---------------------------------------------------------------------------

/// Everything the cockpit lights up, as one number.
///
/// `1.0` is a healthy ship and `0.0` is a dead panel: no instrument glow, no
/// strip lighting, no radar, no annunciators, no interior lamps. See the module
/// docs — this exists so that `BACKLOG.md`'s EMP is a call, not a refactor.
#[derive(Resource, Debug)]
pub struct CockpitPower {
    /// Applied brightness, `0..1`.
    pub level: f32,
    /// Seconds of hard blackout left.
    blackout: f32,
}

impl Default for CockpitPower {
    fn default() -> Self {
        CockpitPower {
            level: 1.0,
            blackout: 0.0,
        }
    }
}

impl CockpitPower {
    /// Kills every instrument for `secs`, then reboots over [`REBOOT_SECS`].
    ///
    /// The only entry point an EMP needs. The longest pending blackout wins, so
    /// two overlapping hits cannot cut each other short.
    pub fn emp(&mut self, secs: f32) {
        self.blackout = self.blackout.max(secs);
    }

    /// Whether the panel is dark enough that there is nothing to read. The
    /// radar and the annunciators skip their work entirely when it is.
    fn dark(&self) -> bool {
        self.level <= 0.02
    }
}

/// `G`, but only when `SPACESHIPS_EMP` is set — a dev hook for an effect with
/// no weapon in front of it yet, kept off the shipped binding table.
fn emp_test_key() -> Option<KeyCode> {
    #[cfg(not(target_arch = "wasm32"))]
    if std::env::var_os("SPACESHIPS_EMP").is_some() {
        return Some(KeyCode::KeyG);
    }
    None
}

fn tick_power(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut power: ResMut<CockpitPower>,
    mut audio: Option<ResMut<AudioCommands>>,
) {
    // TODO(emp): the weapon does not exist yet, so this stands in for the event
    // that will eventually carry it. When it lands, this block is the only
    // thing that changes.
    //
    // Behind `SPACESHIPS_EMP` rather than on a bare `G`: the JS binds no such
    // key, and a port whose whole promise is that the controls are the same
    // cannot quietly grow one for an unshipped weapon.
    if emp_test_key().is_some_and(|k| keys.just_pressed(k)) {
        power.emp(4.0);
        // "Should it kill the audio warnings? A dead cockpit that also goes
        // silent is a genuinely unsettling few seconds." — BACKLOG.md §2.
        if let Some(audio) = audio.as_mut() {
            audio.stop_warnings();
        }
    }

    let dt = time.delta_secs();
    if power.blackout > 0.0 {
        power.blackout = (power.blackout - dt).max(0.0);
        power.level = 0.0;
    } else if power.level < 1.0 {
        power.level = (power.level + dt / REBOOT_SECS).min(1.0);
    }
}

/// Marks the two interior point lights so the EMP can dim them with the rest.
#[derive(Component)]
struct CockpitLamp {
    /// Intensity at full power, in lumens.
    lit: f32,
}

/// Writes [`CockpitPower::level`] onto every registered emissive material and
/// both lamps — only when it has actually moved, quantised to the 32 steps the
/// reboot ramp can show.
fn apply_power(
    power: Res<CockpitPower>,
    rig: Option<Res<Rig>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut lamps: Query<(&CockpitLamp, &mut PointLight)>,
    mut applied: Local<Option<u8>>,
) {
    let Some(rig) = rig else {
        return;
    };
    let step = (power.level.clamp(0.0, 1.0) * 32.0).round() as u8;
    if *applied == Some(step) {
        return;
    }
    *applied = Some(step);

    let level = f32::from(step) / 32.0;
    for (handle, lit) in &rig.lit {
        if let Some(mut mat) = materials.get_mut(handle) {
            mat.emissive = *lit * level;
        }
    }
    for (lamp, mut light) in &mut lamps {
        light.intensity = lamp.lit * level;
    }
}

// ---------------------------------------------------------------------------
// The built cockpit
// ---------------------------------------------------------------------------

/// One gauge: a fill quad anchored at its left edge.
///
/// A `Transform`, not a texture, so a bar sweeping empty to full costs one
/// matrix write and no upload at all.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Bar {
    fill: Entity,
    /// Left edge, in the panel face's local X.
    x0: f32,
    /// Width at full deflection.
    w: f32,
}

impl Bar {
    /// A slot in the gauge list that has not been built yet.
    const NONE: Bar = Bar {
        fill: Entity::PLACEHOLDER,
        x0: 0.0,
        w: 0.0,
    };

    fn set(self, tf: &mut Transform, frac: f32) {
        // Never exactly zero: a zero scale on one axis makes the normal matrix
        // singular, and the inverse-transpose then hands the shader NaNs.
        let f = frac.clamp(0.0005, 1.0);
        tf.scale.x = self.w * f;
        tf.translation.x = self.x0 + self.w * f * 0.5;
    }
}

/// The gauges, in the order they sit in [`Rig::bars`].
#[derive(Clone, Copy)]
enum Gauge {
    Speed = 0,
    Throttle = 1,
    Hull = 2,
    Boost = 3,
    Heat = 4,
    Charge = 5,
}

impl Gauge {
    const COUNT: usize = 6;
}

/// Everything [`drive_instruments`] has to find, resolved once at build time.
///
/// Inserted only once the cockpit exists, so every reader takes
/// `Option<Res<Rig>>` and no field has to be an `Option`.
#[derive(Resource)]
struct Rig {
    /// [`CockpitProfile::eye`] carried through `scene::ship_fit`, in **ship**
    /// space.
    ///
    /// The interior root holds the fit for everything parented to it, but the
    /// camera is not parented to the interior — `seat_camera` writes it from
    /// the ship's world matrix — so the one anchor that leaves this hierarchy
    /// has to be fitted by hand. Stored rather than recomputed so the fit is
    /// read once, at spawn, from the same call the root transform used.
    ///
    /// This is the whole of the profile the `Rig` still needs: everything else
    /// it described is now geometry hanging off [`Rig::root`], and the seated
    /// FOV is read straight from `profile(hull())` at `toggle_view`.
    eye: Vec3,
    /// The interior root, a child of the local ship. Hidden in third person.
    root: Entity,
    /// Centre stick and throttle lever, which follow the controls.
    stick: Entity,
    throttle: Entity,
    /// Radar sweep pivot.
    sweep: Entity,
    /// Fixed pool of contact blips.
    blips: Vec<Entity>,
    /// Half-width of the scope face, in ship units.
    scope_r: f32,
    /// Gauges, indexed by [`Gauge`].
    bars: Vec<Bar>,
    /// Missile and flare pips.
    missile_pips: Vec<Entity>,
    flare_pips: Vec<Entity>,
    /// `GUN` / `BEAM`; one visible at a time.
    gun_label: Entity,
    beam_label: Entity,
    /// Glareshield annunciators.
    tgt_lamp: Entity,
    msl_lamp: Entity,
    /// Materials swapped onto the above at runtime.
    swatch: Swatch,
    /// Every emissive material in the cockpit, with the colour it has at full
    /// power. The EMP's entire implementation.
    lit: Vec<(Handle<StandardMaterial>, LinearRgba)>,
}

/// Materials that get swapped at runtime, kept together so [`Rig`] does not
/// grow a dozen loose handles.
#[derive(Clone)]
struct Swatch {
    cyan: Handle<StandardMaterial>,
    orange: Handle<StandardMaterial>,
    green: Handle<StandardMaterial>,
    yellow: Handle<StandardMaterial>,
    red: Handle<StandardMaterial>,
    hot: Handle<StandardMaterial>,
    purple: Handle<StandardMaterial>,
    blue: Handle<StandardMaterial>,
    dead: Handle<StandardMaterial>,
    blip_hostile: Handle<StandardMaterial>,
    blip_friendly: Handle<StandardMaterial>,
    lamp_off: Handle<StandardMaterial>,
    tgt_on: Handle<StandardMaterial>,
    msl_on: Handle<StandardMaterial>,
}

// ---------------------------------------------------------------------------
// Palette and builder
// ---------------------------------------------------------------------------

/// The shared handles the interior is assembled from.
///
/// Every box in the cockpit is the same unit cube and every flat face the same
/// unit quad, scaled: about 150 meshes drawn from a handful of handles and
/// twenty materials, which is what lets Bevy batch the interior instead of
/// issuing a draw call per switch cap.
struct Palette {
    cube: Handle<Mesh>,
    quad: Handle<Mesh>,
    hoops: [Handle<Mesh>; 2],
    boot: Handle<Mesh>,
    shaft: Handle<Mesh>,
    panel: Handle<StandardMaterial>,
    frame: Handle<StandardMaterial>,
    trim: Handle<StandardMaterial>,
    rubber: Handle<StandardMaterial>,
    seat: Handle<StandardMaterial>,
    strap: Handle<StandardMaterial>,
    stick: Handle<StandardMaterial>,
    screen: Handle<StandardMaterial>,
    well: Handle<StandardMaterial>,
    accent: Handle<StandardMaterial>,
    lamp: Handle<StandardMaterial>,
    grid: Handle<StandardMaterial>,
    white: Handle<StandardMaterial>,
    caption: Handle<StandardMaterial>,
    switches: [Handle<StandardMaterial>; 3],
    swatch: Swatch,
    lit: Vec<(Handle<StandardMaterial>, LinearRgba)>,
}

impl Palette {
    fn new(
        profile: &CockpitProfile,
        meshes: &mut Assets<Mesh>,
        mats: &mut Assets<StandardMaterial>,
    ) -> Palette {
        // Real cockpits are near-black. Keeping albedo this low is what lets
        // the emissive strips and the instrument glow read as the actual light
        // sources in here.
        let mut matte = |rgb: u32, rough: f32, metal: f32| {
            mats.add(StandardMaterial {
                base_color: Color::from(srgb(rgb)),
                perceptual_roughness: rough,
                metallic: metal,
                ..default()
            })
        };
        let panel = matte(0x0e1216, 0.95, 0.05);
        let frame = matte(0x1a2027, 0.60, 0.50);
        let trim = matte(0x252c34, 0.50, 0.60);
        let rubber = matte(0x080a0c, 1.00, 0.00);
        let seat = matte(0x121619, 0.98, 0.00);
        let strap = matte(0x3a3527, 0.95, 0.00);
        let stick = matte(0x0b0e11, 0.92, 0.06);
        // `createScreen` clears its canvas to #05080b; `bar()` draws its
        // unfilled well in #0d151c.
        let screen = matte(0x05080b, 0.35, 0.00);
        let well = matte(0x0d151c, 0.40, 0.00);

        let mut lit = Vec::new();
        // Ultra renders to an HDR target with a bloom prefilter threshold of
        // 0.9 (`camera.rs`), so an emissive has to clear 1.0 to glow at all.
        // These are the JS's `MeshBasicMaterial({ toneMapped: false })` faces,
        // pushed past the knee so they bloom slightly.
        let mut glow = |rgb: u32, gain: f32| {
            let colour = LinearRgba::from(srgb(rgb)) * gain;
            let handle = mats.add(StandardMaterial {
                base_color: Color::BLACK,
                emissive: colour,
                ..default()
            });
            lit.push((handle.clone(), colour));
            handle
        };

        let swatch = Swatch {
            cyan: glow(0x3ddcff, 3.0),
            orange: glow(0xff9d3d, 3.0),
            green: glow(0x4ade80, 3.0),
            yellow: glow(0xfacc15, 3.0),
            red: glow(0xff4d4d, 3.0),
            hot: glow(0xff8a3d, 3.0),
            purple: glow(0xc084fc, 3.0),
            blue: glow(0x3d9dff, 3.0),
            dead: glow(0x16222c, 0.8),
            blip_hostile: glow(0xff4d4d, 5.0),
            blip_friendly: glow(0x46ff9b, 5.0),
            lamp_off: glow(0x0b0f13, 0.8),
            tgt_on: glow(0x38ff9b, 6.0),
            msl_on: glow(0xff3b30, 6.0),
        };
        let accent = glow(profile.accent, 2.4);
        let lamp = glow(profile.lamp, 1.8);
        let grid = glow(0x1d3a4a, 1.6);
        let white = glow(0xeaf6ff, 3.0);
        let caption = glow(0x7fa6c0, 2.2);
        let switches = [
            glow(0xff5a3c, 2.6),
            glow(0x46ff9b, 2.6),
            glow(0xffd24a, 2.6),
        ];

        Palette {
            cube: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
            quad: meshes.add(Rectangle::new(1.0, 1.0)),
            hoops: [
                meshes.add(hoop_mesh(profile.hw, 0.017)),
                meshes.add(hoop_mesh(profile.hw * 0.99, 0.015)),
            ],
            boot: meshes.add(frustum_mesh(0.072, 0.095, 0.07)),
            shaft: meshes.add(frustum_mesh(0.022, 0.030, 0.24)),
            panel,
            frame,
            trim,
            rubber,
            seat,
            strap,
            stick,
            screen,
            well,
            accent,
            lamp,
            grid,
            white,
            caption,
            switches,
            swatch,
            lit,
        }
    }
}

/// `#rrggbb` as an `Srgba`.
fn srgb(rgb: u32) -> Srgba {
    Srgba::rgb_u8(
        (rgb >> 16) as u8,
        ((rgb >> 8) & 0xff) as u8,
        (rgb & 0xff) as u8,
    )
}

/// A scoped spawner, so the port below reads line for line against the JS's
/// `box(w, h, d, mat, x, y, z)` helper.
struct Bld<'a, 'w, 's> {
    commands: &'a mut Commands<'w, 's>,
    pal: &'a Palette,
    /// Only the captions need this — one baked mesh per word. It lives here so
    /// that a `label(..)` call is as short as the `gauge(..)` next to it, which
    /// is what keeps the panel layout below readable against `dash.js`.
    meshes: &'a mut Assets<Mesh>,
}

impl Bld<'_, '_, '_> {
    fn boxm(
        &mut self,
        parent: Entity,
        size: Vec3,
        mat: &Handle<StandardMaterial>,
        at: Vec3,
    ) -> Entity {
        self.mesh(
            parent,
            &self.pal.cube.clone(),
            mat,
            Transform::from_translation(at).with_scale(size),
        )
    }

    /// The same, with three.js's `mesh.rotation.x` — applied about the mesh's
    /// own centre, after the scale.
    fn box_rx(
        &mut self,
        parent: Entity,
        size: Vec3,
        mat: &Handle<StandardMaterial>,
        at: Vec3,
        angle: f32,
    ) -> Entity {
        self.mesh(
            parent,
            &self.pal.cube.clone(),
            mat,
            Transform {
                translation: at,
                rotation: Quat::from_rotation_x(angle),
                scale: size,
            },
        )
    }

    fn box_rz(
        &mut self,
        parent: Entity,
        size: Vec3,
        mat: &Handle<StandardMaterial>,
        at: Vec3,
        angle: f32,
    ) -> Entity {
        self.mesh(
            parent,
            &self.pal.cube.clone(),
            mat,
            Transform {
                translation: at,
                rotation: Quat::from_rotation_z(angle),
                scale: size,
            },
        )
    }

    /// A flat face, in a frame whose local `+x` is the pilot's right and `+y`
    /// is up.
    fn quad(
        &mut self,
        parent: Entity,
        size: Vec2,
        mat: &Handle<StandardMaterial>,
        at: Vec3,
    ) -> Entity {
        self.mesh(
            parent,
            &self.pal.quad.clone(),
            mat,
            Transform::from_translation(at).with_scale(size.extend(1.0)),
        )
    }

    fn mesh(
        &mut self,
        parent: Entity,
        mesh: &Handle<Mesh>,
        mat: &Handle<StandardMaterial>,
        tf: Transform,
    ) -> Entity {
        self.commands
            .spawn((
                Mesh3d(mesh.clone()),
                MeshMaterial3d(mat.clone()),
                tf,
                ChildOf(parent),
            ))
            .id()
    }

    /// A bare transform node, for the stick, the throttle and the panel faces.
    fn node(&mut self, parent: Entity, tf: Transform) -> Entity {
        self.commands
            .spawn((tf, Visibility::Inherited, ChildOf(parent)))
            .id()
    }
}

/// Aims a panel at the pilot's eye.
///
/// `Transform::looking_at` points local `-Z` at its target and every flat mesh
/// here faces local `+Z`, so the panel is aimed *away* from the eye and ends up
/// facing it. In the resulting frame local `+x` is the pilot's right and `+y`
/// is up, which is the same mirroring `dash.js` calls out — canvas-right is
/// ship `-X`.
fn facing_eye(at: Vec3, eye: Vec3) -> Transform {
    Transform::from_translation(at).looking_at(at + (at - eye), Vec3::Y)
}

// ---------------------------------------------------------------------------
// Build
// ---------------------------------------------------------------------------

/// Spawns the interior under the local ship, once the ship exists.
///
/// Eager, like the JS: `createCockpit` runs at match start whether or not the
/// player ever presses `V`. In third person the root is simply `Hidden`, which
/// costs one visibility check.
fn build_cockpit(
    mut commands: Commands,
    ships: Query<(Entity, &ShipRoot)>,
    rig: Option<Res<Rig>>,
    target: Res<ViewTarget>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if rig.is_some() {
        return;
    }
    let Some((ship, _)) = ships.iter().find(|(_, r)| r.0 == target.0) else {
        return;
    };

    let profile = profile(hull());
    let palette = Palette::new(&profile, &mut meshes, &mut materials);

    let root = commands
        .spawn((
            Name::new("CockpitInterior"),
            // Carries `scene::ship_fit` for the whole interior — see
            // `profile`. Everything below this node is authored unscaled.
            interior_transform(),
            // `syncShipVisibility`: shown only while seated.
            Visibility::Hidden,
            ChildOf(ship),
        ))
        .id();

    let (stick, throttle, dash) = {
        let mut b = Bld {
            commands: &mut commands,
            pal: &palette,
            meshes: &mut meshes,
        };
        let (stick, throttle) = build_interior(&mut b, root, &profile);
        let dash = build_dash(&mut b, root, &profile, &palette);
        (stick, throttle, dash)
    };

    commands.insert_resource(Rig {
        eye: interior_transform().transform_point(profile.eye),
        root,
        stick,
        throttle,
        sweep: dash.sweep,
        blips: dash.blips,
        scope_r: dash.scope_r,
        bars: dash.bars,
        missile_pips: dash.missile_pips,
        flare_pips: dash.flare_pips,
        gun_label: dash.gun_label,
        beam_label: dash.beam_label,
        tgt_lamp: dash.tgt_lamp,
        msl_lamp: dash.msl_lamp,
        swatch: palette.swatch.clone(),
        lit: palette.lit,
    });
}

/// Moves the interior onto whichever ship is being ridden.
///
/// The tub, the canopy and the panel are one subtree parented to a ship, so the
/// ship's world matrix carries them and `seat_camera` only has to place an eye.
/// That is the right structure and it has exactly one consequence: riding a
/// different aircraft means re-parenting, not rebuilding. One `ChildOf` write,
/// and only on the frame the target actually changes — `Res::is_changed` is
/// true for the frame a resource is written, and `ViewTarget` is written by a
/// key press.
///
/// A live match never runs the body: `ViewTarget` is written once, at
/// `init_resource`, and never again.
fn reseat_interior(
    mut commands: Commands,
    target: Res<ViewTarget>,
    rig: Option<Res<Rig>>,
    ships: Query<(Entity, &ShipRoot)>,
) {
    if !target.is_changed() {
        return;
    }
    let Some(rig) = rig else {
        return;
    };
    let Some((ship, _)) = ships.iter().find(|(_, r)| r.0 == target.0) else {
        return;
    };
    commands.entity(rig.root).insert(ChildOf(ship));
}

/// `createCockpit`: the tub, the canopy hoops, the seat, and the controls.
///
/// Returns the stick and throttle nodes, which are the two things that move.
fn build_interior(b: &mut Bld, root: Entity, p: &CockpitProfile) -> (Entity, Entity) {
    let (hw, floor_y, rail_y, back_z, dash_z) = (p.hw, p.floor_y, p.rail_y, p.back_z, p.dash_z);
    let eye = p.eye;
    let furn = furniture_scale();
    let dash_top = p.dash_top;
    let dash_bot = dash_top - 0.36;
    let rail_z1 = dash_z - 0.10;
    let rail_len = rail_z1 - back_z;
    let rail_mid = (back_z + rail_z1) / 2.0;
    let (panel, frame, trim) = (b.pal.panel.clone(), b.pal.frame.clone(), b.pal.trim.clone());
    let (rubber, seat_m, strap_m, stick_m) = (
        b.pal.rubber.clone(),
        b.pal.seat.clone(),
        b.pal.strap.clone(),
        b.pal.stick.clone(),
    );
    let (accent, lamp) = (b.pal.accent.clone(), b.pal.lamp.clone());

    // -- floor: solid, with a raised footwell deck --------------------------
    // A glazed floor read as a missing floor rather than as a window, so the
    // tub is closed. Downward context comes from the hull itself, which stays
    // drawn in first person.
    b.boxm(
        root,
        Vec3::new(hw * 2.0, 0.04, dash_z - back_z),
        &panel,
        Vec3::new(0.0, floor_y, (back_z + dash_z) / 2.0),
    );
    for x in [-hw * 0.60, hw * 0.60] {
        b.boxm(
            root,
            Vec3::new(0.30, 0.022, (dash_z - eye.z) * 0.8),
            &trim,
            Vec3::new(x, floor_y + 0.03, eye.z + (dash_z - eye.z) * 0.5),
        );
    }
    b.boxm(
        root,
        Vec3::new(0.09, 0.035, dash_z - back_z),
        &frame,
        Vec3::new(0.0, floor_y + 0.028, (back_z + dash_z) / 2.0),
    );
    // Footwell lighting, washing up off the deck.
    for s in [-1.0, 1.0] {
        b.boxm(
            root,
            Vec3::new(0.016, 0.008, (dash_z - eye.z) * 0.6),
            &lamp,
            Vec3::new(
                s * (hw - 0.30),
                floor_y + 0.045,
                eye.z + (dash_z - eye.z) * 0.55,
            ),
        );
    }

    // -- tub sides: only up to the canopy rail ------------------------------
    for s in [-1.0, 1.0] {
        b.boxm(
            root,
            Vec3::new(0.04, rail_y - floor_y, rail_len),
            &panel,
            Vec3::new(s * hw, (floor_y + rail_y) / 2.0, rail_mid),
        );
        // Rail cap, plus the light strip washing down into the tub.
        b.boxm(
            root,
            Vec3::new(0.075, 0.05, rail_len),
            &trim,
            Vec3::new(s * (hw - 0.02), rail_y + 0.02, rail_mid),
        );
        b.boxm(
            root,
            Vec3::new(0.022, 0.012, rail_len * 0.86),
            &accent,
            Vec3::new(s * (hw - 0.055), rail_y - 0.012, rail_mid),
        );
    }
    b.boxm(
        root,
        Vec3::new(hw * 2.0, rail_y - floor_y, 0.04),
        &panel,
        Vec3::new(0.0, (floor_y + rail_y) / 2.0, back_z),
    );

    // -- side consoles, kept below the rail ---------------------------------
    let con_len = (dash_z - 0.18) - back_z;
    let con_z = (back_z + dash_z - 0.18) / 2.0;
    for s in [-1.0, 1.0] {
        b.box_rz(
            root,
            Vec3::new(0.17, 0.20, con_len),
            &frame,
            Vec3::new(s * (hw - 0.10), floor_y + 0.20, con_z),
            s * 0.10,
        );
        // Switch banks: rows of tiny lit caps, the main "cockpit full of
        // lights" read.
        for i in 0..7 {
            let z = con_z - con_len * 0.34 + i as f32 * (con_len * 0.11);
            b.boxm(
                root,
                Vec3::new(0.055, 0.022, 0.038),
                &trim,
                Vec3::new(s * (hw - 0.075), floor_y + 0.30, z),
            );
            let cap = b.pal.switches[i % 3].clone();
            b.boxm(
                root,
                Vec3::new(0.030, 0.010, 0.030),
                &cap,
                Vec3::new(s * (hw - 0.135), floor_y + 0.312, z),
            );
        }
    }

    // -- instrument panel + glareshield -------------------------------------
    b.box_rx(
        root,
        Vec3::new(hw * 1.8, dash_top - dash_bot, 0.06),
        &panel,
        Vec3::new(0.0, (dash_top + dash_bot) / 2.0, dash_z - 0.06),
        0.30,
    );
    b.box_rx(
        root,
        Vec3::new(hw * 1.85, 0.035, 0.20),
        &trim,
        Vec3::new(0.0, dash_top + 0.045, dash_z - 0.17),
        -0.22,
    );
    // Downward wash from under the glareshield onto the panel.
    b.boxm(
        root,
        Vec3::new(hw * 1.5, 0.010, 0.020),
        &lamp,
        Vec3::new(0.0, dash_top + 0.030, dash_z - 0.33),
    );

    // -- canopy: two thin hoops, nothing else above the rail ----------------
    let rear_hoop_z = eye.z - 0.34;
    for (i, z) in [rail_z1, rear_hoop_z].into_iter().enumerate() {
        let hoop = b.pal.hoops[i].clone();
        b.mesh(root, &hoop, &frame, Transform::from_xyz(0.0, rail_y, z));
    }
    // Slim spine linking the two, well above the sightline.
    b.boxm(
        root,
        Vec3::new(0.026, 0.026, rail_z1 - rear_hoop_z),
        &frame,
        Vec3::new(0.0, rail_y + hw - 0.02, (rail_z1 + rear_hoop_z) / 2.0),
    );

    // -- ejection seat ------------------------------------------------------
    let seat_z = eye.z - 0.26;
    b.boxm(
        root,
        Vec3::new(0.50, 0.09, 0.44),
        &seat_m,
        Vec3::new(0.0, floor_y + 0.17, seat_z + 0.06),
    );
    b.box_rx(
        root,
        Vec3::new(0.48, 0.74, 0.09),
        &seat_m,
        Vec3::new(0.0, eye.y - 0.14, seat_z - 0.18),
        -0.10,
    );
    b.boxm(
        root,
        Vec3::new(0.32, 0.15, 0.10),
        &rubber,
        Vec3::new(0.0, eye.y + 0.28, seat_z - 0.20),
    );
    for s in [-1.0, 1.0] {
        b.boxm(
            root,
            Vec3::new(0.06, 0.44, 0.34),
            &seat_m,
            Vec3::new(s * 0.25, eye.y - 0.24, seat_z - 0.02),
        );
        b.box_rx(
            root,
            Vec3::new(0.07, 0.46, 0.03),
            &strap_m,
            Vec3::new(s * 0.17, eye.y - 0.42, seat_z + 0.22),
            0.55,
        );
    }

    // -- centre stick, between the pilot's knees ----------------------------
    // `0.30` ahead of the eye is the JS's reach to the grip (`cockpit.js:184`)
    // and it scales with the pilot, not with the tub — so it goes through
    // `furn` like the stick's own dimensions do.
    let stick = b.node(
        root,
        Transform::from_xyz(0.0, floor_y + 0.02 * furn, eye.z + 0.30 * furn)
            .with_scale(Vec3::splat(furn)),
    );
    let boot = b.pal.boot.clone();
    let shaft = b.pal.shaft.clone();
    b.mesh(stick, &boot, &rubber, Transform::from_xyz(0.0, 0.03, 0.0));
    b.mesh(stick, &shaft, &stick_m, Transform::from_xyz(0.0, 0.13, 0.0));
    b.box_rx(
        stick,
        Vec3::new(0.056, 0.15, 0.078),
        &stick_m,
        Vec3::new(0.0, 0.29, 0.0),
        -0.16,
    );
    b.box_rx(
        stick,
        Vec3::new(0.060, 0.028, 0.082),
        &trim,
        Vec3::new(0.0, 0.365, 0.013),
        -0.16,
    );
    b.boxm(
        stick,
        Vec3::new(0.026, 0.038, 0.018),
        &accent,
        Vec3::new(0.0, 0.28, 0.048),
    );
    let hat = b.pal.switches[0].clone();
    b.boxm(
        stick,
        Vec3::new(0.030, 0.014, 0.030),
        &hat,
        Vec3::new(0.0, 0.358, -0.022),
    );

    // -- throttle lever, left console ---------------------------------------
    // Inboard of the left console wall by a hand's width, so this one offset is
    // measured from `hw` and stays there; everything else about the lever is
    // the pilot's scale.
    let throttle = b.node(
        root,
        Transform::from_xyz(hw - 0.14 * furn, floor_y + 0.34 * furn, eye.z + 0.02 * furn)
            .with_scale(Vec3::splat(furn)),
    );
    b.boxm(
        throttle,
        Vec3::new(0.045, 0.045, 0.26),
        &frame,
        Vec3::new(0.0, 0.0, 0.12),
    );
    b.boxm(
        throttle,
        Vec3::new(0.10, 0.085, 0.10),
        &rubber,
        Vec3::new(0.0, 0.0, 0.26),
    );
    b.boxm(
        throttle,
        Vec3::new(0.055, 0.010, 0.022),
        &accent,
        Vec3::new(0.0, 0.048, 0.26),
    );

    // -- rudder pedals, seen through the chin glazing -----------------------
    // A footwell is the pilot's, not the airframe's, so these go through `furn`
    // whole: sizes, spacing and reach alike.
    for s in [-1.0, 1.0] {
        b.box_rx(
            root,
            Vec3::new(0.115, 0.135, 0.022) * furn,
            &stick_m,
            Vec3::new(s * 0.20 * furn, floor_y + 0.065 * furn, eye.z + 0.72 * furn),
            0.62,
        );
        b.boxm(
            root,
            Vec3::new(0.05, 0.016, 0.24) * furn,
            &frame,
            Vec3::new(s * 0.20 * furn, floor_y + 0.018 * furn, eye.z + 0.60 * furn),
        );
    }

    // -- fill light ---------------------------------------------------------
    // High and behind the head. Sitting it just above the stick meant a decay-2
    // point light was effectively inside the grip, washing the whole thing out
    // to pale grey.
    //
    // The JS scopes these to a render layer so they cannot spill onto the hull,
    // because "now that the hull stays drawn in first person, unscoped point
    // lights this close blew its inner surfaces out to white". Bevy's
    // `RenderLayers` decide which *views* a light is extracted for, not which
    // meshes it reaches, so that trick does not port; the range is what keeps
    // them indoors instead.
    //
    // The output is calibrated rather than guessed, because Bevy lights are
    // photometric and the camera is exposed for daylight: a point light's
    // illuminance is `lumens / (4 pi d^2)`, so 16 klm at the 0.66 m from here
    // down to the floor is ~2900 lux against `scene.rs`'s 9000 lux key. A third
    // of the sun is a fill, which is what these are. Numbers that *look* like
    // interior lamps — a few hundred lumens — land three orders of magnitude
    // under the exposure and do nothing at all; that was measured, not assumed.
    for (colour, lumens, range, at) in [
        (
            p.lamp,
            16_000.0,
            2.6,
            Vec3::new(0.0, rail_y + 0.38, eye.z - 0.06),
        ),
        (
            p.accent,
            1_500.0,
            0.75,
            Vec3::new(0.0, dash_top + 0.02, dash_z - 0.15),
        ),
    ] {
        b.commands.spawn((
            PointLight {
                color: Color::from(srgb(colour)),
                intensity: lumens,
                range,
                shadow_maps_enabled: false,
                ..default()
            },
            CockpitLamp { lit: lumens },
            Transform::from_translation(at),
            ChildOf(root),
        ));
    }

    (stick, throttle)
}

// ---------------------------------------------------------------------------
// The instrument panel
// ---------------------------------------------------------------------------

/// A panel face, mapping `dash.js`'s canvas pixels onto the quad's local frame.
///
/// The ports below are literal because of this: a `bar(10, 44, 236, 18)` in the
/// JS is a `face.rect(10.0, 44.0, 236.0, 18.0)` here, with the same numbers.
#[derive(Clone, Copy)]
struct Panel {
    /// Canvas size the JS authored against.
    cw: f32,
    ch: f32,
    /// World units per canvas pixel.
    px: f32,
}

impl Panel {
    fn new(world_w: f32, cw: f32, ch: f32) -> Panel {
        Panel {
            cw,
            ch,
            px: world_w / cw,
        }
    }

    /// A canvas point, as a position in the face's local frame. Canvas y grows
    /// downward; local y grows up.
    fn at(self, cx: f32, cy: f32) -> Vec2 {
        Vec2::new(
            (cx - self.cw / 2.0) * self.px,
            (self.ch / 2.0 - cy) * self.px,
        )
    }

    /// A canvas size, in world units.
    fn dim(self, w: f32, h: f32) -> Vec2 {
        Vec2::new(w * self.px, h * self.px)
    }
}

/// Depth offsets within a face, in world units. Small, because the near plane
/// is 2 cm and reverse-Z has depth precision to spare this close.
const Z_BEZEL: f32 = -0.002;
const Z_WELL: f32 = 0.0006;
const Z_FILL: f32 = 0.0012;
const Z_TEXT: f32 = 0.0018;

struct Dash {
    sweep: Entity,
    blips: Vec<Entity>,
    scope_r: f32,
    bars: Vec<Bar>,
    missile_pips: Vec<Entity>,
    flare_pips: Vec<Entity>,
    gun_label: Entity,
    beam_label: Entity,
    tgt_lamp: Entity,
    msl_lamp: Entity,
}

/// `createDash`: two displays and a radar scope, set into the panel.
///
/// Two large displays rather than three, for the JS's reason: a centre stick
/// sits between the pilot's knees and would bisect a centre screen, so the
/// panel centre is left to physical detail.
fn build_dash(b: &mut Bld, root: Entity, p: &CockpitProfile, pal: &Palette) -> Dash {
    let hw = p.hw;
    let panel_h = 0.36;
    let dash_top = p.dash_top;

    // Usable panel width is only what sits BETWEEN the side consoles (0.17
    // wide, centred at hw - 0.10), otherwise the outer displays end up buried
    // inside the console boxes.
    let inner_half = hw - 0.20;
    let total_w = inner_half * 2.0;
    let gap = total_w * 0.022;
    let radar_s = (total_w * 0.26).min(panel_h * 0.80);
    let scr_h = ((total_w - radar_s - gap * 2.0) / 2.0 / 2.0).min(panel_h * 0.80);
    let scr_w = scr_h * 2.0;
    let scr_x = radar_s / 2.0 + gap + scr_w / 2.0;
    let screen_z = p.dash_z - 0.20;
    let screen_y = dash_top - scr_h.max(radar_s) / 2.0 - 0.03;

    // Emissive bezels, so the displays read as lit panels set into the dash.
    // One node per display, aimed at the eye; everything else is a child in its
    // local frame, which is where the canvas coordinates land.
    let display = |b: &mut Bld, x: f32, w: f32, h: f32| {
        let face = b.node(root, facing_eye(Vec3::new(x, screen_y, screen_z), p.eye));
        b.quad(
            face,
            Vec2::new(w + 0.016, h + 0.016),
            &pal.accent,
            Vec3::new(0.0, 0.0, Z_BEZEL),
        );
        b.quad(face, Vec2::new(w, h), &pal.screen, Vec3::ZERO);
        face
    };

    let flight = display(b, scr_x, scr_w, scr_h);
    let weapons = display(b, -scr_x, scr_w, scr_h);
    let scope = display(b, 0.0, radar_s, radar_s);

    let f = Panel::new(scr_w, 256.0, 128.0);

    // -- flight display (pilot's left; +X renders on screen-left) -----------
    // `dash.js` prints the speed as a 40 px numeral. Digits that change every
    // frame are exactly the thing this module refuses to redraw, so the
    // readout is a bar against `TOP_SPEED` in the same slot, captioned SPD.
    let mut bars = vec![Bar::NONE; Gauge::COUNT];
    bars[Gauge::Speed as usize] = gauge(b, flight, f, 56.0, 8.0, 190.0, 26.0, &pal.white);
    label(b, flight, f, "SPD", 10.0, 12.0, 15.0, &pal.caption);
    bars[Gauge::Throttle as usize] = gauge(b, flight, f, 10.0, 44.0, 236.0, 18.0, &pal.swatch.cyan);
    label(b, flight, f, "THR", 14.0, 47.0, 12.0, &pal.screen);
    bars[Gauge::Hull as usize] = gauge(b, flight, f, 10.0, 70.0, 236.0, 20.0, &pal.swatch.green);
    label(b, flight, f, "HULL", 14.0, 74.0, 12.0, &pal.screen);
    bars[Gauge::Boost as usize] = gauge(b, flight, f, 10.0, 98.0, 236.0, 18.0, &pal.swatch.blue);
    label(b, flight, f, "BOOST", 14.0, 101.0, 12.0, &pal.screen);

    // -- weapons display (pilot's right) ------------------------------------
    let gun_label = label(b, weapons, f, "GUN", 10.0, 10.0, 15.0, &pal.accent);
    let beam_label = label(b, weapons, f, "BEAM", 10.0, 10.0, 15.0, &pal.accent);
    b.commands.entity(beam_label).insert(Visibility::Hidden);
    bars[Gauge::Heat as usize] = gauge(b, weapons, f, 74.0, 8.0, 172.0, 18.0, &pal.swatch.red);
    label(b, weapons, f, "MSL", 10.0, 43.0, 13.0, &pal.caption);
    let missile_pips = (0..4)
        .map(|i| {
            b.quad(
                weapons,
                f.dim(24.0, 20.0),
                &pal.swatch.dead,
                f.at(62.0 + i as f32 * 32.0 + 12.0, 50.0).extend(Z_FILL),
            )
        })
        .collect();
    label(b, weapons, f, "FLR", 10.0, 77.0, 13.0, &pal.caption);
    let flare_pips = (0..3)
        .map(|i| {
            b.quad(
                weapons,
                f.dim(24.0, 20.0),
                &pal.swatch.dead,
                f.at(62.0 + i as f32 * 32.0 + 12.0, 84.0).extend(Z_FILL),
            )
        })
        .collect();
    bars[Gauge::Charge as usize] =
        gauge(b, weapons, f, 10.0, 100.0, 236.0, 16.0, &pal.swatch.purple);
    label(b, weapons, f, "CHARGE", 14.0, 103.0, 11.0, &pal.screen);

    // -- radar scope, panel centre ------------------------------------------
    // Heading-up: contacts are rotated into the ship's frame before they get
    // here, and `facing_eye` already put local +x on the pilot's right.
    let scope_r = radar_s / 2.0 * 0.92;
    let grid = b.meshes.add(scope_mesh());
    b.mesh(
        scope,
        &grid,
        &pal.grid,
        Transform::from_xyz(0.0, 0.0, Z_WELL).with_scale(Vec3::splat(scope_r)),
    );
    // The sweep arm: one entity, rotated. This is the whole reason the radar is
    // geometry — in `dash.js` this line is what re-uploads the texture.
    let sweep = b.node(scope, Transform::from_xyz(0.0, 0.0, Z_FILL));
    b.quad(
        sweep,
        Vec2::new(scope_r * 0.04, scope_r),
        &pal.accent,
        Vec3::new(0.0, scope_r / 2.0, 0.0),
    );
    let blip_size = radar_s * 10.0 / 160.0;
    let blips: Vec<Entity> = (0..MAX_BLIPS)
        .map(|_| {
            let e = b.quad(
                scope,
                Vec2::splat(blip_size),
                &pal.swatch.blip_hostile,
                Vec3::new(0.0, 0.0, Z_TEXT),
            );
            b.commands.entity(e).insert(Visibility::Hidden);
            e
        })
        .collect();
    // Own ship.
    b.quad(
        scope,
        Vec2::splat(radar_s * 6.0 / 160.0),
        &pal.white,
        Vec3::new(0.0, 0.0, Z_TEXT),
    );

    // -- annunciators on the glareshield ------------------------------------
    //   TGT — the reticle is aligned on an enemy
    //   MSL — an enemy missile is locking YOU
    // Split outboard rather than sitting as one centred plate: on a long-nosed
    // hull the centre of the glareshield is exactly the sightline to your own
    // nose, and the plate covered it.
    let lamp_face = Panel::new(hw * 0.34, 128.0, 40.0);
    let annunciator = |b: &mut Bld, x: f32, text: &str| {
        let at = Vec3::new(x, dash_top + 0.048, p.dash_z - 0.255);
        let lamp = b.mesh(
            root,
            &pal.cube.clone(),
            &pal.swatch.lamp_off,
            facing_eye(at, p.eye).with_scale(Vec3::new(hw * 0.28, 0.050, 0.02)),
        );
        let cap = b.node(
            root,
            facing_eye(Vec3::new(x, dash_top + 0.098, p.dash_z - 0.285), p.eye),
        );
        label(b, cap, lamp_face, text, 8.0, 6.0, 26.0, &pal.caption);
        lamp
    };
    let tgt_lamp = annunciator(b, hw * 0.66, "TGT");
    let msl_lamp = annunciator(b, -hw * 0.66, "MSL");

    Dash {
        sweep,
        blips,
        scope_r,
        bars,
        missile_pips,
        flare_pips,
        gun_label,
        beam_label,
        tgt_lamp,
        msl_lamp,
    }
}

/// `bar(ctx, x, y, w, h, frac, colour)`: an unfilled well with a fill quad
/// anchored at its left edge.
fn gauge(
    b: &mut Bld,
    face: Entity,
    f: Panel,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    colour: &Handle<StandardMaterial>,
) -> Bar {
    let centre = f.at(x + w / 2.0, y + h / 2.0);
    let size = f.dim(w, h);
    let well = b.pal.well.clone();
    b.quad(face, size, &well, centre.extend(Z_WELL));
    let x0 = f.at(x, 0.0).x;
    let bar = Bar {
        x0,
        w: size.x,
        ..Bar::NONE
    };
    // Spawned empty rather than full, so a gauge reads correctly even in the
    // frame before anything has driven it.
    let mut tf = Transform::from_xyz(0.0, centre.y, Z_FILL).with_scale(size.extend(1.0));
    bar.set(&mut tf, 0.0);
    Bar {
        fill: b.mesh(face, &b.pal.quad.clone(), colour, tf),
        ..bar
    }
}

/// A word, as one static mesh of 3x5 cells. See [`caption_mesh`].
fn label(
    b: &mut Bld,
    face: Entity,
    f: Panel,
    text: &str,
    x: f32,
    y: f32,
    cap_px: f32,
    colour: &Handle<StandardMaterial>,
) -> Entity {
    let cell = cap_px / 5.0;
    let w = caption_cells(text) * cell;
    let centre = f.at(x + w / 2.0, y + cap_px / 2.0);
    let mesh = b.meshes.add(caption_mesh(text));
    b.mesh(
        face,
        &mesh,
        colour,
        Transform::from_translation(centre.extend(Z_TEXT)).with_scale(Vec3::splat(cell * f.px)),
    )
}

// ---------------------------------------------------------------------------
// Per-frame: the head
// ---------------------------------------------------------------------------

/// `FirstPersonCamera.update` — everything except `_apply`, which is
/// [`seat_camera`]'s half.
fn drive_head(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    view: Res<ViewMode>,
    frame: Res<SimFrame>,
    target: Res<ViewTarget>,
    mut head: ResMut<Head>,
) {
    if !view.seated {
        return;
    }

    let look = if keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight) {
        Look::Back
    } else if buttons.pressed(MouseButton::Right) {
        Look::Free(motion.delta)
    } else {
        Look::Boresight
    };

    let me = seated_ship(&frame.0, target.0);
    head.update(
        look,
        steer(&frame.0),
        me.is_some_and(|s| s.flags.contains(sim::world::ShipFlags::BOOSTING)),
        me.map_or(0.0, |s| s.hit_flash),
        time.delta_secs(),
    );
}

/// The steering the head and the stick lean with.
///
/// `HudState::steer` is the field for this and is where it should come from —
/// but `sim_bridge::tick` does not fill it yet (it builds `HudState` with
/// `..Default::default()`), so this falls back to the raw arrow axes, which is
/// the same intent one step earlier. Delete the fallback when the bridge fills
/// the field.
/// Ramped steering, for head lean and the stick.
///
/// `sim::tick` fills `hud.steer` from the flight step (`tick.rs:1144`), so this
/// is a straight read. It carried a fallback to the raw arrow axes while
/// `sim_bridge` still ran a hand-rolled partial tick that left `HudState` at
/// its default; that is no longer true, and the fallback would have masked the
/// field going stale again.
fn steer(frame: &sim::world::Frame) -> (f32, f32) {
    let [sx, sy] = frame.hud.steer;
    (sx, sy)
}

/// The ship whose cockpit this is.
///
/// [`LOCAL_ID`] in a live match — that is `ViewTarget`'s default — and whoever
/// a replay is riding otherwise. Reading a resource rather than the constant is
/// the whole of what "ride a plane" needs from this module; see
/// `replay::ViewTarget`.
fn seated_ship(frame: &sim::world::Frame, id: EntityId) -> Option<&sim::world::ShipView> {
    frame.ships.iter().find(|s| s.id == id)
}

/// three.js's `MathUtils.damp`: frame-rate-independent exponential approach.
fn damp(from: f32, to: f32, lambda: f32, dt: f32) -> f32 {
    from + (to - from) * (1.0 - (-lambda * dt).exp())
}

// ---------------------------------------------------------------------------
// Per-frame: the camera
// ---------------------------------------------------------------------------

/// The camera's own pose, both halves. `GlobalTransform` is in here because
/// transform propagation has already run by the time [`seat_camera`] does.
type CameraPose<'w, 's> = Query<
    'w,
    's,
    (&'static mut Transform, &'static mut GlobalTransform),
    (With<FlightCamera>, Without<ShipRoot>),
>;

/// `FirstPersonCamera._apply`: put the camera at the eye anchor, looking where
/// the head looks.
///
/// See the module docs for why this writes `GlobalTransform` too, and why it is
/// ordered the way it is.
fn seat_camera(
    view: Res<ViewMode>,
    head: Res<Head>,
    rig: Option<Res<Rig>>,
    target: Res<ViewTarget>,
    ships: Query<(&ShipRoot, &GlobalTransform)>,
    mut cam: CameraPose,
) {
    if !view.seated {
        return;
    }
    let Some(rig) = rig else {
        return;
    };
    let Some((_, ship)) = ships.iter().find(|(r, _)| r.0 == target.0) else {
        return;
    };
    let Ok((mut tf, mut global)) = cam.single_mut() else {
        return;
    };

    // Two incommensurable frequencies per axis, so the rumble never reads as a
    // loop.
    let t = head.shake_t;
    let shake_yaw = head.shake_amp * ((t * 47.1).sin() + 0.6 * (t * 113.7).sin());
    let shake_pitch = head.shake_amp * ((t * 61.3).sin() + 0.6 * (t * 149.2).sin());

    // The eye anchor is ship-local; the ship's world matrix carries it out.
    // Reading `GlobalTransform` here rather than `ShipView::pos` is what keeps
    // the view free of the 60 Hz staircase `scene.rs` exists to remove.
    tf.translation = ship.transform_point(rig.eye);

    // A Bevy camera looks down its local -Z and this game's ship forward is
    // +Z, hence the PI yaw correction — the same one `fpcamera.js` bakes in.
    // Setting the rotation directly is also what keeps roll correct: the
    // horizon rolls with the airframe, as it must from inside it.
    tf.rotation = ship.rotation()
        * Quat::from_rotation_y(std::f32::consts::PI + head.yaw + head.lean_yaw + shake_yaw)
        * Quat::from_rotation_x(head.pitch + head.lean_pitch + shake_pitch);

    *global = GlobalTransform::from(*tf);
}

// ---------------------------------------------------------------------------
// Per-frame: the instruments
// ---------------------------------------------------------------------------

/// Everything the panel shows, quantised to what the pixels can resolve.
///
/// The comparison that makes a steady cockpit free: a ship flying level with
/// full health changes none of these, so [`drive_instruments`] returns having
/// touched no entity at all.
#[derive(Resource, Default, Clone, Copy, PartialEq, Eq)]
struct Applied {
    ready: bool,
    speed: u16,
    throttle: u16,
    hull: u16,
    boost: u16,
    heat: u16,
    charge: u16,
    missiles: u8,
    flares: u8,
    beam: bool,
    boosting: bool,
    low_hull: u8,
    tgt: bool,
    msl: bool,
    dark: bool,
    seated: bool,
}

/// A fraction as tenths of a percent — the same `.toFixed(1)` the JS HUD
/// already rounds bar widths to, and finer than a 256 px screen can show.
fn mil(v: f32) -> u16 {
    (v.clamp(0.0, 1.0) * 1000.0).round() as u16
}

#[expect(
    clippy::too_many_arguments,
    reason = "eleven, against clippy's ten: five resources the panel is built \
              from and four queries for the component types it writes. Grouping \
              the writes the way `hud.rs`'s `HudWrite` does would buy one \
              parameter and cost an indirection in every one of the forty \
              places below that reaches for a query."
)]
fn drive_instruments(
    time: Res<Time>,
    view: Res<ViewMode>,
    power: Res<CockpitPower>,
    frame: Res<SimFrame>,
    rig: Option<Res<Rig>>,
    target: Res<ViewTarget>,
    mut applied: ResMut<Applied>,
    // `Without<ShipRoot>` is load-bearing: the cockpit hangs off the ship, so
    // both queries touch `Transform` and Bevy cannot prove the archetypes are
    // disjoint on its own.
    ships: Query<(&ShipRoot, &Transform)>,
    mut transforms: Query<&mut Transform, Without<ShipRoot>>,
    mut materials: Query<&mut MeshMaterial3d<StandardMaterial>>,
    mut visibility: Query<&mut Visibility>,
) {
    let Some(rig) = rig else {
        return;
    };
    let dt = time.delta_secs();

    // The interior itself: `syncShipVisibility`.
    if applied.seated != view.seated {
        applied.seated = view.seated;
        set_visible(&mut visibility, rig.root, view.seated);
        // Sitting down invalidates the readout: the panel has been off, and a
        // gauge whose value happens to match the one it had when the pilot
        // stood up would otherwise never be written. `ready` is cleared here
        // and set below, so the first seated frame always applies everything.
        applied.ready = false;
    }
    if !view.seated {
        return;
    }

    let hud = frame.0.hud;
    // **The gauges are the recorded pilot's, not the ridden ship's.**
    // `HudState` is computed for `World::local_id` alone, so riding an enemy in
    // a replay puts you in their seat with your own telemetry on the panel.
    // Fixing it properly means `sim` deriving a `HudState` per ship, which is a
    // simulation change and not a rendering one; until then the pose, the
    // canopy, the radar centre and the hull hiding all follow the target and
    // the needles do not.
    let me = seated_ship(&frame.0, target.0);
    let boosting = me.is_some_and(|s| s.flags.contains(sim::world::ShipFlags::BOOSTING));

    // -- the controls, which move continuously ------------------------------
    // Stick pulls back to pitch up; steering right tilts the grip toward -X
    // (the pilot's right).
    let (sx, sy) = steer(&frame.0);
    if let Ok(mut tf) = transforms.get_mut(rig.stick) {
        let (x, _, z) = tf.rotation.to_euler(EulerRot::XYZ);
        tf.rotation = Quat::from_rotation_x(damp(x, sy * 0.30, 12.0, dt))
            * Quat::from_rotation_z(damp(z, sx * 0.30, 12.0, dt));
    }
    if let Ok(mut tf) = transforms.get_mut(rig.throttle) {
        let (x, _, _) = tf.rotation.to_euler(EulerRot::XYZ);
        let want = 0.55 + (-0.55 - 0.55) * hud.throttle01.clamp(0.0, 1.0);
        tf.rotation = Quat::from_rotation_x(damp(x, want, 8.0, dt));
    }

    // -- the radar, which is the only other continuous thing ----------------
    let dark = power.dark();
    if !dark {
        if let Ok(mut tf) = transforms.get_mut(rig.sweep) {
            let (_, _, z) = tf.rotation.to_euler(EulerRot::XYZ);
            tf.rotation =
                Quat::from_rotation_z((z - SWEEP_RATE * dt).rem_euclid(std::f32::consts::TAU));
        }
    }
    draw_contacts(
        &rig,
        &frame.0,
        target.0,
        &ships,
        dark,
        &mut transforms,
        &mut materials,
        &mut visibility,
    );

    // -- the discrete readout -----------------------------------------------
    let blink = time.elapsed_secs();
    // Missile warning blinks fast and urgent; target lock pulses slower and
    // steadier (`dash.js:234`).
    let msl = hud.missile_lock_warning && blink.rem_euclid(0.34) < 0.17;
    let tgt = hud.assist_target >= 0 && blink.rem_euclid(0.60) < 0.42;
    let hp = hud.hp01;
    let next = Applied {
        ready: true,
        speed: mil(hud.speed / TOP_SPEED),
        throttle: mil(hud.throttle01),
        hull: mil(hp),
        boost: mil(hud.boost01),
        heat: mil(hud.ammo01),
        charge: mil(hud.charge01),
        missiles: hud.missiles,
        flares: hud.flares,
        beam: hud.gun_mode == sim::world::GunMode::Beam,
        boosting,
        low_hull: if hp > 0.5 {
            0
        } else if hp > 0.25 {
            1
        } else {
            2
        },
        tgt: tgt && !dark,
        msl: msl && !dark,
        dark,
        seated: true,
    };
    if next == *applied {
        return;
    }
    let prev = *applied;
    *applied = next;

    // Only what differs, each behind its own comparison.
    let sw = &rig.swatch;
    for (gauge, was, now) in [
        (Gauge::Speed, prev.speed, next.speed),
        (Gauge::Throttle, prev.throttle, next.throttle),
        (Gauge::Hull, prev.hull, next.hull),
        (Gauge::Boost, prev.boost, next.boost),
        (Gauge::Heat, prev.heat, next.heat),
        (Gauge::Charge, prev.charge, next.charge),
    ] {
        if was == now && prev.ready {
            continue;
        }
        let bar = rig.bars[gauge as usize];
        if let Ok(mut tf) = transforms.get_mut(bar.fill) {
            bar.set(&mut tf, f32::from(now) / 1000.0);
        }
    }

    if prev.boosting != next.boosting || !prev.ready {
        let colour = if next.boosting { &sw.orange } else { &sw.cyan };
        set_material(
            &mut materials,
            rig.bars[Gauge::Throttle as usize].fill,
            colour,
        );
    }
    if prev.low_hull != next.low_hull || !prev.ready {
        let colour = match next.low_hull {
            0 => &sw.green,
            1 => &sw.yellow,
            _ => &sw.red,
        };
        set_material(&mut materials, rig.bars[Gauge::Hull as usize].fill, colour);
    }
    if (prev.heat > 200) != (next.heat > 200) || !prev.ready {
        let colour = if next.heat > 200 { &sw.hot } else { &sw.red };
        set_material(&mut materials, rig.bars[Gauge::Heat as usize].fill, colour);
    }
    if (prev.charge >= 1000) != (next.charge >= 1000) || !prev.ready {
        let colour = if next.charge >= 1000 {
            &sw.red
        } else {
            &sw.purple
        };
        set_material(
            &mut materials,
            rig.bars[Gauge::Charge as usize].fill,
            colour,
        );
    }
    if prev.missiles != next.missiles || !prev.ready {
        for (i, pip) in rig.missile_pips.iter().enumerate() {
            let lit = i < usize::from(next.missiles);
            set_material(
                &mut materials,
                *pip,
                if lit { &sw.orange } else { &sw.dead },
            );
        }
    }
    if prev.flares != next.flares || !prev.ready {
        for (i, pip) in rig.flare_pips.iter().enumerate() {
            let lit = i < usize::from(next.flares);
            set_material(
                &mut materials,
                *pip,
                if lit { &sw.yellow } else { &sw.dead },
            );
        }
    }
    if prev.beam != next.beam || !prev.ready {
        set_visible(&mut visibility, rig.gun_label, !next.beam);
        set_visible(&mut visibility, rig.beam_label, next.beam);
    }
    if prev.tgt != next.tgt || !prev.ready {
        let colour = if next.tgt { &sw.tgt_on } else { &sw.lamp_off };
        set_material(&mut materials, rig.tgt_lamp, colour);
    }
    if prev.msl != next.msl || !prev.ready {
        let colour = if next.msl { &sw.msl_on } else { &sw.lamp_off };
        set_material(&mut materials, rig.msl_lamp, colour);
    }
}

/// Moves the blip pool onto this frame's contacts.
///
/// Positions come from the *interpolated* `Transform`s rather than from
/// `ShipView::pos`, for the reason `sim_bridge` gives: `SimFrame` is the last
/// tick, and anything continuous read straight out of it staircases at the tick
/// rate. A blip is small, but it is 60 Hz motion on a 144 Hz display.
fn draw_contacts(
    rig: &Rig,
    frame: &sim::world::Frame,
    seat: EntityId,
    ships: &Query<(&ShipRoot, &Transform)>,
    dark: bool,
    transforms: &mut Query<&mut Transform, Without<ShipRoot>>,
    materials: &mut Query<&mut MeshMaterial3d<StandardMaterial>>,
    visibility: &mut Query<&mut Visibility>,
) {
    let mut shown = 0;
    if !dark {
        let me = ships.iter().find(|(r, _)| r.0 == seat).map(|(_, t)| *t);
        if let Some(me) = me {
            let my_team = seated_ship(frame, seat).map_or(-1, |s| s.team);
            for (root, tf) in ships {
                if shown >= rig.blips.len() || root.0 == seat {
                    continue;
                }
                let Some(view) = frame.ships.iter().find(|s| s.id == root.0) else {
                    continue;
                };
                if !view.flags.contains(sim::world::ShipFlags::ALIVE)
                    || view.flags.contains(sim::world::ShipFlags::BOSS_HITBOX)
                {
                    continue;
                }
                let Some(on_scope) =
                    contact_on_scope(me.translation, me.rotation, tf.translation, RADAR_RANGE)
                else {
                    continue;
                };

                let blip = rig.blips[shown];
                shown += 1;
                if let Ok(mut t) = transforms.get_mut(blip) {
                    t.translation.x = on_scope.x * rig.scope_r;
                    t.translation.y = on_scope.y * rig.scope_r;
                }
                let hostile = my_team < 0 || view.team != my_team;
                set_material(
                    materials,
                    blip,
                    if hostile {
                        &rig.swatch.blip_hostile
                    } else {
                        &rig.swatch.blip_friendly
                    },
                );
                set_visible(visibility, blip, true);
            }
        }
    }
    for blip in &rig.blips[shown..] {
        set_visible(visibility, *blip, false);
    }
}

/// A contact's place on a heading-up scope, in `-1..1` scope units where `+x`
/// is the pilot's right and `+y` is ahead. `None` if it is out of range.
///
/// Offsets stay in world units — the JS is careful not to use `worldToLocal`
/// here, which would divide by `SHIP_SCALE`.
fn contact_on_scope(me: Vec3, facing: Quat, other: Vec3, range: f32) -> Option<Vec2> {
    let rel = facing.inverse() * (other - me);
    if rel.length_squared() > range * range {
        return None;
    }
    Some(Vec2::new(-rel.x / range, rel.z / range))
}

fn set_material(
    q: &mut Query<&mut MeshMaterial3d<StandardMaterial>>,
    entity: Entity,
    handle: &Handle<StandardMaterial>,
) {
    if let Ok(mut mat) = q.get_mut(entity) {
        if mat.0 != *handle {
            mat.0 = handle.clone();
        }
    }
}

fn set_visible(q: &mut Query<&mut Visibility>, entity: Entity, visible: bool) {
    let want = if visible {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    if let Ok(mut vis) = q.get_mut(entity) {
        if *vis != want {
            *vis = want;
        }
    }
}

// ---------------------------------------------------------------------------
// The hull, from inside
// ---------------------------------------------------------------------------

/// What [`sync_hull`] last applied, so entering and leaving the cockpit is the
/// only time any of it is written.
#[derive(Resource, Default)]
struct HullMode {
    applied: Option<bool>,
    /// The materials [`HullMode::applied`] was applied *to*.
    ///
    /// The mode alone is not enough to early out on, and that was a real bug: a
    /// glTF instantiates a frame or two after the ship entity appears, so a
    /// pilot already seated when the match starts — which is exactly what
    /// `SPACESHIPS_COCKPIT` does — latched `Some(true)` against an empty walk
    /// and never forced the hull single-sided when the model finally arrived.
    /// The result is a solid black wall across the canopy that no amount of
    /// pressing `V` clears, because the latch says the work is already done.
    ///
    /// Comparing the material list instead makes the state a function of what
    /// is actually on screen, so a model that turns up late, or a paint job
    /// that swaps every handle for a clone, re-applies on its own.
    covers: Vec<Handle<StandardMaterial>>,
    /// Each hull material with the sidedness the glTF gave it, so third person
    /// gets exactly what it had.
    restore: Vec<(Handle<StandardMaterial>, bool, Option<Face>)>,
}

/// `applyExteriorMode` + the exterior half of `syncShipVisibility`.
///
/// **The hull stays drawn in first person.** That is worth stating plainly
/// because it is the opposite of what a cockpit view usually does: you can see
/// your own nose, wings and engines from the seat rather than floating in a
/// detached box. Two things make it work, and both are ported here:
///
/// - the material is forced single-sided, which back-face culls everything
///   enclosing the eye. `spaceship.glb` carries exactly one material and it is
///   authored `doubleSided`, so without this the hull is a solid black wall
///   from the inside — the same reason `main.js:449` forces `FrontSide`;
/// - only the *canopy* is hidden, since the cockpit interior brings its own.
///   `spaceship.glb` has a node called `Cockpit`, which is what `CANOPY_RE`
///   matches.
///
/// The materials are shared glTF assets, so this reaches every ship using them.
/// That is the correct rendering for an opaque hull either way, and there is
/// one ship in this slice; a per-ship version needs a material clone, which
/// costs the batching `scene.rs` is careful to keep.
fn sync_hull(
    view: Res<ViewMode>,
    rig: Option<Res<Rig>>,
    target: Res<ViewTarget>,
    mut mode: ResMut<HullMode>,
    ships: Query<(&ShipRoot, &Children)>,
    children: Query<&Children>,
    named: Query<(
        Option<&Name>,
        Option<&GltfMeshName>,
        Option<&GltfMaterialName>,
    )>,
    meshes: Query<&MeshMaterial3d<StandardMaterial>>,
    mut visibility: Query<&mut Visibility>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Some(rig) = rig else {
        return;
    };
    let Some((_, roots)) = ships.iter().find(|(r, _)| r.0 == target.0) else {
        return;
    };
    let fp = view.seated;

    // The glTF loads a frame or two after the ship entity appears, so the walk
    // has to keep happening; the writes inside it do not, which is what the
    // comparisons in `set_visible` and `mode.applied` are for.
    let mut hull_materials = Vec::new();
    for branch in roots.iter() {
        // Skip our own subtree: the interior is a sibling of the model and must
        // survive everything below.
        if branch == rig.root {
            continue;
        }
        for entity in std::iter::once(branch).chain(children.iter_descendants(branch)) {
            if let Ok(names) = named.get(entity) {
                if is_canopy(names) {
                    set_visible(&mut visibility, entity, !fp);
                    continue;
                }
                set_visible(&mut visibility, entity, true);
            }
            if let Ok(MeshMaterial3d(handle)) = meshes.get(entity) {
                // Distinct materials, not distinct meshes. `paint_and_upgrade`
                // clones once per authored material and shares the clone across
                // every mesh using it, so `spaceship.glb`'s six nodes hand back
                // the same handle six times — and saving the same material six
                // times captures the *forced* sidedness as if it were the
                // glTF's own, which is how the restore below used to leave the
                // hull single-sided for good.
                if !hull_materials.contains(handle) {
                    hull_materials.push(handle.clone());
                }
            }
        }
    }

    if mode.applied == Some(fp) && (!fp || mode.covers == hull_materials) {
        return;
    }

    // Always unwind first. Re-applying on top of a partly-applied set would
    // record `Face::Back` as the value to restore to.
    for (handle, double_sided, cull) in std::mem::take(&mut mode.restore) {
        if let Some(mut mat) = materials.get_mut(&handle) {
            mat.double_sided = double_sided;
            mat.cull_mode = cull;
        }
    }
    mode.applied = Some(fp);
    mode.covers.clear();

    if fp {
        for handle in hull_materials {
            // A handle whose asset has not landed yet is simply left out of
            // `covers`, so the comparison above brings this system back for it
            // next frame.
            let Some(mut mat) = materials.get_mut(&handle) else {
                continue;
            };
            mode.restore
                .push((handle.clone(), mat.double_sided, mat.cull_mode));
            mat.double_sided = false;
            mat.cull_mode = Some(Face::Back);
            mode.covers.push(handle);
        }
    }
}

/// `isCanopyMesh` (`main.js:446`), against the node name, the glTF mesh name
/// and the material name — the three places three.js would have looked.
fn is_canopy(
    names: (
        Option<&Name>,
        Option<&GltfMeshName>,
        Option<&GltfMaterialName>,
    ),
) -> bool {
    let (node, mesh, material) = names;
    let candidates = [
        node.map(Name::as_str),
        mesh.map(|m| m.0.as_str()),
        material.map(|m| m.0.as_str()),
    ];
    candidates.into_iter().flatten().any(|name| {
        let lower = name.to_ascii_lowercase();
        CANOPY_WORDS.iter().any(|w| lower.contains(w))
    })
}

// ---------------------------------------------------------------------------
// Meshes
// ---------------------------------------------------------------------------

/// A minimal triangle-soup builder, for the three meshes here that no Bevy
/// primitive covers.
#[derive(Default)]
struct MeshBuf {
    pos: Vec<[f32; 3]>,
    norm: Vec<[f32; 3]>,
    uv: Vec<[f32; 2]>,
    idx: Vec<u32>,
}

impl MeshBuf {
    /// A quad in the XY plane, facing +Z, given by its corner and its extent.
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let base = self.pos.len() as u32;
        for (px, py, u, v) in [
            (x, y, 0.0, 1.0),
            (x + w, y, 1.0, 1.0),
            (x + w, y + h, 1.0, 0.0),
            (x, y + h, 0.0, 0.0),
        ] {
            self.pos.push([px, py, 0.0]);
            self.norm.push([0.0, 0.0, 1.0]);
            self.uv.push([u, v]);
        }
        self.idx
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    fn build(self) -> Mesh {
        // The CPU copy is only needed by something that raycasts against the
        // mesh, and nothing does: the cockpit is scenery.
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD,
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.pos)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.norm)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.uv)
        .with_inserted_indices(Indices::U32(self.idx))
    }
}

/// The radar's rings and crosshair, in scope units where the outer ring is at
/// radius 1.
///
/// One mesh, built once, drawn once. `dash.js` strokes these into the canvas
/// on every redraw.
fn scope_mesh() -> Mesh {
    const SEGMENTS: usize = 48;
    const LINE: f32 = 0.041;
    let mut m = MeshBuf::default();

    for r in [0.33, 0.66, 1.0f32] {
        for i in 0..SEGMENTS {
            let a0 = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            let a1 = (i + 1) as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            let (i0, o0) = (r - LINE / 2.0, r + LINE / 2.0);
            let base = m.pos.len() as u32;
            for (rad, ang) in [(i0, a0), (o0, a0), (o0, a1), (i0, a1)] {
                m.pos.push([rad * ang.cos(), rad * ang.sin(), 0.0]);
                m.norm.push([0.0, 0.0, 1.0]);
                m.uv.push([0.0, 0.0]);
            }
            m.idx
                .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
    m.rect(-LINE / 2.0, -1.0, LINE, 2.0);
    m.rect(-1.0, -LINE / 2.0, 2.0, LINE);
    m.build()
}

/// Half a torus, in the XY plane, sweeping from +X through +Y to -X.
///
/// `TorusGeometry(radius, tube, 6, 20, Math.PI)`. Bevy's `Torus` primitive has
/// no arc parameter, and a full ring would pass straight through the floor.
fn hoop_mesh(radius: f32, tube: f32) -> Mesh {
    const MAJOR: usize = 20;
    const MINOR: usize = 6;
    let mut m = MeshBuf::default();

    for i in 0..=MAJOR {
        let u = i as f32 / MAJOR as f32 * std::f32::consts::PI;
        for j in 0..=MINOR {
            let v = j as f32 / MINOR as f32 * std::f32::consts::TAU;
            let (cu, su) = (u.cos(), u.sin());
            let (cv, sv) = (v.cos(), v.sin());
            m.pos.push([
                (radius + tube * cv) * cu,
                (radius + tube * cv) * su,
                tube * sv,
            ]);
            m.norm.push([cv * cu, cv * su, sv]);
            m.uv.push([i as f32 / MAJOR as f32, j as f32 / MINOR as f32]);
        }
    }
    let stride = (MINOR + 1) as u32;
    for i in 0..MAJOR as u32 {
        for j in 0..MINOR as u32 {
            let a = i * stride + j;
            m.idx
                .extend_from_slice(&[a, a + stride, a + stride + 1, a, a + stride + 1, a + 1]);
        }
    }
    m.build()
}

/// A tapered cylinder — `CylinderGeometry(top, bottom, height)`.
fn frustum_mesh(radius_top: f32, radius_bottom: f32, height: f32) -> Mesh {
    ConicalFrustum {
        radius_top,
        radius_bottom,
        height,
    }
    .mesh()
    .resolution(10)
    .build()
}

// ---------------------------------------------------------------------------
// The caption font
// ---------------------------------------------------------------------------

/// Cell width of a word, in 3x5 cells: three per glyph, one of spacing.
fn caption_cells(text: &str) -> f32 {
    let n = text.chars().count();
    if n == 0 {
        0.0
    } else {
        (n * 4 - 1) as f32
    }
}

/// A word as one mesh of unit cells, centred on the origin.
///
/// Bevy's text is a 2D screen-space affair — `Text2d` needs a 2D camera and
/// `bevy_ui` is on the HUD's layer — so there is no way to put a glyph on a
/// panel in the world without either a texture or geometry. A 3x5 bitmap font
/// baked into a static mesh is the version that costs nothing per frame, and
/// legibility at this size is not the constraint people expect: the JS panel is
/// drawn at a third of screen resolution behind the PSX pixel filter, so its
/// captions are already this chunky.
fn caption_mesh(text: &str) -> Mesh {
    let mut m = MeshBuf::default();
    let w = caption_cells(text);
    // Centre the word: cells run left to right from -w/2, rows top to bottom.
    for (i, ch) in text.chars().enumerate() {
        let x0 = -w / 2.0 + (i * 4) as f32;
        let bits = glyph(ch);
        for row in 0..5 {
            for col in 0..3 {
                if bits & (1 << (14 - (row * 3 + col))) != 0 {
                    m.rect(x0 + col as f32, 2.5 - (row + 1) as f32, 1.0, 1.0);
                }
            }
        }
    }
    m.build()
}

/// A 3x5 glyph, packed row-major from the top left, MSB first. Unknown
/// characters are blank, which is how a space is spelled.
fn glyph(ch: char) -> u16 {
    match ch.to_ascii_uppercase() {
        'A' => 0b010_101_111_101_101,
        'B' => 0b110_101_110_101_110,
        'C' => 0b011_100_100_100_011,
        'D' => 0b110_101_101_101_110,
        'E' => 0b111_100_110_100_111,
        'F' => 0b111_100_110_100_100,
        'G' => 0b011_100_101_101_011,
        'H' => 0b101_101_111_101_101,
        'I' => 0b111_010_010_010_111,
        'J' => 0b001_001_001_101_010,
        'K' => 0b101_101_110_101_101,
        'L' => 0b100_100_100_100_111,
        'M' => 0b101_111_111_101_101,
        'N' => 0b101_111_111_111_101,
        'O' => 0b010_101_101_101_010,
        'P' => 0b110_101_110_100_100,
        'Q' => 0b010_101_101_111_011,
        'R' => 0b110_101_110_101_101,
        'S' => 0b011_100_010_001_110,
        'T' => 0b111_010_010_010_010,
        'U' => 0b101_101_101_101_010,
        'V' => 0b101_101_101_010_010,
        'W' => 0b101_101_111_111_101,
        'X' => 0b101_101_010_101_101,
        'Y' => 0b101_101_010_010_010,
        'Z' => 0b111_001_010_100_111,
        '0' => 0b111_101_101_101_111,
        '1' => 0b010_110_010_010_111,
        '2' => 0b110_001_010_100_111,
        '3' => 0b110_001_010_001_110,
        '4' => 0b101_101_111_001_001,
        '5' => 0b111_100_110_001_110,
        '6' => 0b011_100_110_101_010,
        '7' => 0b111_001_010_010_010,
        '8' => 0b010_101_010_101_010,
        '9' => 0b010_101_011_001_110,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Everything here is a pure function, and deliberately: the parts of this
/// module with real failure modes — the panel layout collapsing to zero, the
/// scope mirroring left for right, the head damping never converging — are all
/// separable from the `App`, the window and the render device.
#[cfg(test)]
mod tests {
    use super::*;

    /// `scene::ship_fit()` for `jet.glb`, which the tests cannot call: it reads
    /// `SPACESHIPS_SHIP_MODEL`, and a test that set the environment would
    /// change what every other test in the binary sees.
    ///
    /// `model_fit`'s 1.62 times `scene::SHIP_SCALE`, with its (1.98, 0.20, 0.0)
    /// turned into ship axes by the -90 degree yaw.
    const JET_FIT: (f32, Vec3) = (1.62 * crate::scene::SHIP_SCALE, Vec3::new(0.0, 0.20, 1.98));

    /// Every profile as it is actually built — which is **unfitted**, because
    /// the fit now lives on the interior's root transform. See `profile`.
    fn built_profiles() -> [CockpitProfile; 3] {
        [DEFAULT_PROFILE, ADMIN_PROFILE, JET_PROFILE]
    }

    #[test]
    fn every_profile_describes_a_tub_the_pilot_fits_in() {
        for p in built_profiles() {
            assert!(p.rail_y > p.floor_y, "the rail is above the floor");
            assert!(p.eye.y > p.rail_y, "the eye clears the canopy rail");
            assert!(p.dash_z > p.eye.z, "the panel is ahead of the pilot");
            assert!(p.back_z < p.eye.z, "the bulkhead is behind the pilot");
            assert!(p.dash_top < p.eye.y, "the panel does not block the view");
            // The usable panel is what sits between the side consoles.
            assert!(p.hw - 0.20 > 0.0, "the panel has room for a display");
        }
    }

    /// The interior is built in **one** space: the profile is unfitted, and the
    /// fit is a transform on the root.
    ///
    /// This is the property that broke and it is worth stating outright, because
    /// nothing about the types enforces it. `build_interior` measures much of
    /// the cockpit in literal constants — the stick at `eye.z + 0.30`, a 0.24
    /// shaft, 0.115 by 0.135 pedals — and a literal cannot be fitted. Scale the
    /// profile without scaling those and the furniture separates from the tub:
    /// at the jet's fit the stick sat at 40% of its intended distance from the
    /// seat, which put it inside the pilot, and the consoles stopped short of
    /// the walls, which is a gap either side of the panel.
    #[test]
    fn the_profile_is_unfitted_so_it_matches_the_literals_around_it() {
        for hull in [Hull::Default, Hull::Admin, Hull::Jet] {
            let p = profile(hull);
            assert!(
                [DEFAULT_PROFILE, ADMIN_PROFILE, JET_PROFILE].contains(&p),
                "{hull:?} came back fitted; the fit belongs on the root",
            );
        }

        // And the root carries it. An identity fit is an identity transform, so
        // the hull that needs no fit pays nothing for the scheme.
        let jet =
            Transform::from_translation(JET_FIT.1 * JET_FIT.0).with_scale(Vec3::splat(JET_FIT.0));
        assert!(jet.scale.x > 2.0, "the jet's interior really is scaled up");
        // Approximate, and it has to be: the transform computes
        // `scale * q + scale * offset` where `fitted` computes
        // `scale * (q + offset)`, which differ in the last bit. A ten-thousandth
        // of a unit is far below anything the eye can be wrong by and far above
        // that.
        let (a, b) = (
            jet.transform_point(JET_PROFILE.eye),
            JET_PROFILE.fitted(JET_FIT.0, JET_FIT.1).eye,
        );
        assert!(
            a.abs_diff_eq(b, 1e-4),
            "the root transform and `fitted` disagree about the eye: {a} vs {b}",
        );
    }

    /// A hull whose fit is the identity — which is both of the originals — must
    /// come through untouched, or this scheme would have moved the one cockpit
    /// that was already right.
    #[test]
    fn an_identity_fit_leaves_a_profile_alone() {
        for p in [DEFAULT_PROFILE, ADMIN_PROFILE, JET_PROFILE] {
            assert_eq!(p.fitted(1.0, Vec3::ZERO), p);
        }
    }

    /// The bug the jet profile exists to fix: the seat has to follow the hull.
    ///
    /// `jet.glb`'s canopy, measured with the node transforms applied and then
    /// carried through `scene::ship_fit`, occupies ship x ±0.435, y 0.013..0.573,
    /// z 4.471..6.406. Unfitted, the default profile puts the pilot at
    /// (0, 1.26, 0.40) — four units aft of that glass and three quarters of a
    /// unit above it, which is out among the tailfins.
    #[test]
    fn the_jet_pilot_sits_inside_the_jets_canopy() {
        // `jet.glb`'s canopy carried through the same fit as the hull, so both
        // sides of every comparison below are in one space. The literals are
        // the canopy at `model_fit`'s 1.62; `SHIP_SCALE` applies to the glass
        // and the pilot alike, so it multiplies both.
        const CANOPY_Z: (f32, f32) = (
            4.471 * crate::scene::SHIP_SCALE,
            6.406 * crate::scene::SHIP_SCALE,
        );
        const CANOPY_CROWN: f32 = 0.573 * crate::scene::SHIP_SCALE;

        let p = JET_PROFILE.fitted(JET_FIT.0, JET_FIT.1);
        assert!(
            p.eye.z > CANOPY_Z.0 && p.eye.z < CANOPY_Z.1,
            "eye outside the canopy lengthwise: {}",
            p.eye.z
        );
        assert!(
            p.eye.y < CANOPY_CROWN,
            "the pilot's head is through the glass: {}",
            p.eye.y
        );
        // The panel is ahead of the pilot but still under the windscreen bow,
        // and the bulkhead is behind them but not out past the canopy's rear
        // edge — either would leave scenery hanging outside the aircraft.
        assert!(p.dash_z > p.eye.z && p.dash_z < CANOPY_Z.1);
        assert!(p.back_z < p.eye.z && p.back_z >= CANOPY_Z.0 - 0.1);

        // And the failure this replaces, stated as a fact so it cannot come
        // back: the unfitted anchors are nowhere near the aircraft.
        assert!(DEFAULT_PROFILE.eye.z < CANOPY_Z.0 - 3.0);
    }

    /// The layout is a chain of `min`s, and a profile that made any of them
    /// zero would leave an invisible panel rather than an error.
    #[test]
    fn the_dash_layout_leaves_room_for_three_displays() {
        for p in built_profiles() {
            let inner_half = p.hw - 0.20;
            let total_w = inner_half * 2.0;
            let gap = total_w * 0.022;
            let radar_s = (total_w * 0.26).min(0.36 * 0.80);
            let scr_h = ((total_w - radar_s - gap * 2.0) / 4.0).min(0.36 * 0.80);
            let scr_w = scr_h * 2.0;
            assert!(radar_s > 0.0 && scr_w > 0.0);
            // Nothing overlaps: two screens, a scope, and two gaps.
            assert!(
                scr_w * 2.0 + radar_s + gap * 2.0 <= total_w + 1e-6,
                "displays overflow the panel"
            );
        }
    }

    #[test]
    fn a_contact_dead_ahead_sits_at_the_top_of_the_scope() {
        let at = contact_on_scope(
            Vec3::ZERO,
            Quat::IDENTITY,
            Vec3::new(0.0, 0.0, 600.0),
            1200.0,
        )
        .expect("in range");
        assert_eq!(at.x, 0.0);
        assert!((at.y - 0.5).abs() < 1e-6, "half range, straight up: {at:?}");
    }

    /// The mirroring `dash.js` calls out, and the easiest thing in the module
    /// to get backwards: the pilot's right is ship `-x`, and it draws on the
    /// right of the scope.
    #[test]
    fn a_contact_to_starboard_sits_on_the_right_of_the_scope() {
        let starboard = Vec3::new(-100.0, 0.0, 0.0);
        let at = contact_on_scope(Vec3::ZERO, Quat::IDENTITY, starboard, 1200.0).expect("in range");
        assert!(at.x > 0.0, "starboard contact drew to port: {at:?}");
    }

    #[test]
    fn the_scope_is_heading_up() {
        // Ship turned 90 degrees to port: a contact due ship-ahead is still up.
        let facing = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let ahead = facing * Vec3::new(0.0, 0.0, 300.0);
        let at = contact_on_scope(Vec3::ZERO, facing, ahead, 1200.0).expect("in range");
        assert!(at.x.abs() < 1e-5 && at.y > 0.0, "not heading-up: {at:?}");
    }

    #[test]
    fn out_of_range_contacts_are_dropped() {
        assert!(contact_on_scope(
            Vec3::ZERO,
            Quat::IDENTITY,
            Vec3::new(0.0, 0.0, 1300.0),
            1200.0
        )
        .is_none());
    }

    /// Runs the head for `secs` at 60 Hz under one steady set of inputs.
    fn fly(head: &mut Head, look: Look, steer: (f32, f32), secs: f32) {
        let dt = 1.0 / 60.0;
        for _ in 0..(secs / dt) as u32 {
            head.update(look, steer, false, 0.0, dt);
        }
    }

    /// The head leans *into* the turn and comes back to centre when the stick
    /// does. Both halves matter: a lean that tracked but never returned would
    /// leave the pilot looking permanently off-axis after one turn.
    #[test]
    fn the_head_leans_into_a_turn_and_returns_to_centre() {
        let mut head = Head::default();

        // Full deflection right. The pilot's right is ship -x, so a positive
        // steer leans the head to negative yaw — into the turn, not away.
        fly(&mut head, Look::Boresight, (1.0, 0.0), 2.0);
        assert!(
            (head.lean_yaw + AUTO_LEAN_YAW).abs() < 1e-3,
            "did not reach full lean: {}",
            head.lean_yaw
        );
        assert!(head.lean_yaw < 0.0, "leaned away from the turn");

        // Full deflection left mirrors it exactly.
        let mut other = Head::default();
        fly(&mut other, Look::Boresight, (-1.0, 0.0), 2.0);
        assert!((other.lean_yaw - AUTO_LEAN_YAW).abs() < 1e-3);

        // Pitch is the same story on the other axis.
        let mut nose_up = Head::default();
        fly(&mut nose_up, Look::Boresight, (0.0, 1.0), 2.0);
        assert!((nose_up.lean_pitch + AUTO_LEAN_PITCH).abs() < 1e-3);

        // Stick centred: the lean bleeds off.
        fly(&mut head, Look::Boresight, (0.0, 0.0), 2.0);
        assert!(head.lean_yaw.abs() < 1e-3, "lean stuck: {}", head.lean_yaw);
    }

    /// Free-look is clamped, and the clamp is per axis. Getting the two
    /// crossed would let the pilot roll their head 110 degrees vertically —
    /// which looks like the horizon coming apart, not like a bug.
    #[test]
    fn free_look_stays_inside_its_clamp() {
        let mut head = Head::default();

        // A mouse flung far past the stop, in both directions, on both axes.
        fly(
            &mut head,
            Look::Free(Vec2::new(-4000.0, -4000.0)),
            (0.0, 0.0),
            1.0,
        );
        assert!(
            (head.yaw - MAX_YAW).abs() < 1e-6,
            "yaw ran past: {}",
            head.yaw
        );
        assert!(
            (head.pitch - MAX_PITCH).abs() < 1e-6,
            "pitch ran past: {}",
            head.pitch
        );

        fly(
            &mut head,
            Look::Free(Vec2::new(4000.0, 4000.0)),
            (0.0, 0.0),
            1.0,
        );
        assert!((head.yaw + MAX_YAW).abs() < 1e-6);
        assert!((head.pitch + MAX_PITCH).abs() < 1e-6);

        // Releasing the button walks it home.
        fly(&mut head, Look::Boresight, (0.0, 0.0), 3.0);
        assert!(head.yaw.abs() < 1e-3 && head.pitch.abs() < 1e-3);
    }

    /// `Alt` looks over the shoulder and holds there, and the pitch centres so
    /// the pilot is not craning at the floor while they do it.
    #[test]
    fn looking_back_swings_all_the_way_round() {
        let mut head = Head::default();
        fly(
            &mut head,
            Look::Free(Vec2::new(0.0, -400.0)),
            (0.0, 0.0),
            0.1,
        );
        assert!(head.pitch > 0.0);

        fly(&mut head, Look::Back, (0.0, 0.0), 2.0);
        assert!((head.yaw - LOOK_BACK_YAW).abs() < 1e-3, "{}", head.yaw);
        assert!(
            head.pitch.abs() < 1e-3,
            "pitch did not centre: {}",
            head.pitch
        );
    }

    /// Frame-rate independence is the whole reason `damp` is not a `lerp`: one
    /// 100 ms step and ten 10 ms steps must land in the same place.
    #[test]
    fn damping_is_frame_rate_independent() {
        let one = damp(0.0, 1.0, 7.0, 0.1);
        let mut many = 0.0;
        for _ in 0..10 {
            many = damp(many, 1.0, 7.0, 0.01);
        }
        assert!((one - many).abs() < 1e-6, "{one} != {many}");
    }

    #[test]
    fn a_bar_fills_from_its_left_edge() {
        let bar = Bar {
            fill: Entity::PLACEHOLDER,
            x0: -0.1,
            w: 0.2,
        };
        let mut tf = Transform::IDENTITY;

        bar.set(&mut tf, 1.0);
        assert!((tf.scale.x - 0.2).abs() < 1e-6);
        assert!((tf.translation.x - 0.0).abs() < 1e-6, "full bar is centred");

        bar.set(&mut tf, 0.5);
        assert!((tf.scale.x - 0.1).abs() < 1e-6);
        assert!(
            (tf.translation.x + 0.05).abs() < 1e-6,
            "half a bar hugs the left edge, not the centre"
        );

        // The degenerate case that would hand the shader NaNs.
        bar.set(&mut tf, 0.0);
        assert!(tf.scale.x > 0.0);
    }

    #[test]
    fn a_canvas_point_maps_into_the_face() {
        let f = Panel::new(0.2784, 256.0, 128.0);
        assert_eq!(
            f.at(128.0, 64.0),
            Vec2::ZERO,
            "the canvas centre is local 0"
        );
        assert!(f.at(0.0, 64.0).x < 0.0, "canvas left is local left");
        assert!(f.at(128.0, 0.0).y > 0.0, "canvas top is local up");
        assert!((f.dim(256.0, 128.0).x - 0.2784).abs() < 1e-6);
    }

    #[test]
    fn every_caption_the_panel_uses_has_glyphs() {
        for word in [
            "SPD", "THR", "HULL", "BOOST", "GUN", "BEAM", "MSL", "FLR", "CHARGE", "TGT",
        ] {
            for ch in word.chars() {
                assert_ne!(glyph(ch), 0, "{ch} in {word} is blank");
            }
            let mesh = caption_mesh(word);
            assert!(mesh.count_vertices() > 0, "{word} baked to an empty mesh");
        }
        assert_eq!(caption_cells("TGT"), 11.0);
    }

    #[test]
    fn the_canopy_test_matches_the_glb_and_nothing_else() {
        let name = |s: &str| Name::new(s.to_owned());
        for hit in ["Cockpit", "canopy_glass", "Windshield", "WINDOW.001"] {
            assert!(is_canopy((Some(&name(hit)), None, None)), "{hit}");
        }
        for miss in ["Body", "Engines", "Tail", "gun", "bottom"] {
            assert!(!is_canopy((Some(&name(miss)), None, None)), "{miss}");
        }
        // The other two places three.js would have looked.
        assert!(is_canopy((None, Some(&GltfMeshName("Glass".into())), None)));
        assert!(is_canopy((
            None,
            None,
            Some(&GltfMaterialName("canopy".into()))
        )));
    }

    /// The EMP contract: one call takes everything down, and nothing brings it
    /// back early.
    #[test]
    fn an_emp_darkens_the_panel_until_it_expires() {
        let mut power = CockpitPower::default();
        assert!(!power.dark());

        power.emp(4.0);
        power.level = 0.0;
        assert!(power.dark());

        // A second, shorter hit cannot shorten the first.
        power.emp(1.0);
        assert!((power.blackout - 4.0).abs() < 1e-6);
    }

    #[test]
    fn the_scope_and_hoop_meshes_are_well_formed() {
        for mesh in [scope_mesh(), hoop_mesh(0.6, 0.017)] {
            let verts = mesh.count_vertices();
            assert!(verts > 0);
            let Some(Indices::U32(idx)) = mesh.indices() else {
                panic!("expected u32 indices");
            };
            assert!(idx.len() % 3 == 0, "not triangles");
            assert!(
                idx.iter().all(|i| (*i as usize) < verts),
                "index out of range"
            );
        }
    }

    /// The hoop is an arch over the pilot, not a full ring around them: a ring
    /// would pass straight through the floor.
    #[test]
    fn the_canopy_hoop_is_the_upper_half_only() {
        let mesh = hoop_mesh(0.6, 0.017);
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(pos)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("no positions");
        };
        assert!(
            pos.iter().all(|p| p[1] >= -0.018),
            "the hoop dips below the rail"
        );
        assert!(pos.iter().any(|p| p[1] > 0.5), "the hoop has no arch");
    }
}
