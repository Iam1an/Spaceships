//! Deterministic simulation core for Spaceships.
//!
//! # The one rule
//!
//! **This crate must produce bit-identical output from bit-identical input, on
//! every machine, every run.** The same code is intended to run in two places at
//! once — inside the authoritative server and inside the browser (compiled to
//! WASM) for client-side prediction — and the two only agree if the simulation
//! is fully deterministic. Every constraint below follows from that.
//!
//! ## What must never appear in this crate
//!
//! - **No I/O.** No files, no sockets, no `std::net`, no logging that changes
//!   behaviour. The simulation takes inputs and returns state; the caller
//!   decides where those come from and where they go.
//! - **No rendering.** No THREE.js analogue, no meshes, no materials, no
//!   colors, no asset loading. Rendering stays in `public/` on Three.js; this
//!   crate only produces the numbers the renderer draws.
//! - **No networking.** This crate does not know that `spaceships-protocol`
//!   exists and must not depend on it. Serialization is the transport layer's
//!   job. (The reverse dependency is also banned: `protocol` must not depend on
//!   `sim`, so the wire format can never drift when the sim's math changes.)
//! - **No wall-clock time.** No `SystemTime`, no `Instant`, no `Date.now()`
//!   equivalent. Time advances only through an explicit fixed timestep passed
//!   into the tick function. Variable `dt` from a render loop is *not* allowed
//!   to reach the simulation — the caller accumulates real time and calls tick a
//!   whole number of times.
//! - **No unseeded randomness.** No `rand::thread_rng`, no OS entropy. All
//!   randomness comes from [`rng::Rng`], seeded explicitly by the caller. The
//!   server picks a seed, ships it to the clients, and both regenerate the same
//!   asteroid field locally instead of streaming 60 asteroid records.
//! - **No `unsafe`.** Enforced below.
//! - **No third-party dependencies.** `Cargo.toml` is deliberately empty. A
//!   dependency is the easiest way to smuggle in a `HashMap` iteration order, a
//!   clock, or a platform-specific float path. If something here needs a
//!   dependency, that is a design discussion, not a drive-by `cargo add`.
//!
//! ## Determinism hazards to watch for
//!
//! - Iterating a `HashMap`/`HashSet` and letting the order affect results. Use
//!   `BTreeMap`/`Vec` for anything the simulation reads in order.
//!   `std`'s default hasher is randomly seeded per process.
//! - `f32`/`f64` transcendental functions (`sin`, `cos`, `powf`, ...) are *not*
//!   guaranteed bit-identical across platforms or libm versions. Server-vs-WASM
//!   agreement may require hand-rolled or explicitly-specified implementations.
//!   Basic arithmetic (`+ - * /`) and `sqrt` **are** IEEE-754 exact and safe.
//! - Accumulating into a float in a nondeterministic order. Floating point
//!   addition is not associative.
//!
//! # Status
//!
//! Skeleton. The [`world`] module is intentionally empty: the `World` struct and
//! the tick signature are being designed separately, and guessing at them here
//! would create a shape the real design has to fight. What *is* here is the
//! infrastructure that is needed regardless of how the rest lands:
//!
//! - [`math::Vec3`] — replaces `THREE.Vector3`, which the JS uses everywhere.
//! - [`rng::Rng`] — the seeded generator that makes reproducible asteroid fields
//!   possible.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub mod math;
pub mod rng;

pub mod world {
    //! Simulation state and the fixed-timestep tick.
    //!
    //! Deliberately empty pending a separate design pass. When it lands it will
    //! own:
    //!
    //! - `World` — the full authoritative state of one match: ships, bullets,
    //!   missiles, flares, asteroids, team scores, match clock.
    //! - `Input` — one frame of player intent (throttle, pitch/yaw/roll, fire,
    //!   boost, brake, missile, flare). Intent only, never derived state.
    //! - `tick(&mut World, &[Input], dt: f64)` or similar — one fixed step. `dt`
    //!   is a constant supplied by the caller ([`TICK_HZ`] below), never a
    //!   measured frame time.
    //! - `Event` — what the tick produced that the outside world cares about
    //!   (hits, deaths, asteroid destruction), returned to the caller rather
    //!   than sent anywhere from in here.
    //!
    //! Ports of the existing JS live in `public/src/`: ship flight model and
    //! collision in `main.js`, projectiles in `bullets.js` / `missiles.js` /
    //! `beams.js`, field generation in `asteroids.js`, and the bot AI in
    //! `bot.js`.

    /// Fixed simulation rate.
    ///
    /// The JS client sends `state` updates at 20 Hz (`STATE_INTERVAL = 1 / 20`
    /// in `main.js`) while rendering at display rate. The simulation step is a
    /// separate, faster fixed rate; callers accumulate real elapsed time and run
    /// a whole number of ticks, keeping the remainder for the next frame.
    ///
    /// The value is a placeholder until the tick design lands.
    pub const TICK_HZ: f64 = 60.0;

    /// Duration of one fixed simulation step, in seconds.
    pub const TICK_DT: f64 = 1.0 / TICK_HZ;
}

pub mod entity {
    //! Entity identity and per-entity state.
    //!
    //! Deliberately empty pending the `World` design. It will hold the ship,
    //! projectile, and asteroid records, plus the id type shared with the
    //! transport layer (positive for players, negative for bots — see
    //! `spaceships-protocol`'s `PlayerId`).
}

pub mod collision {
    //! Broad- and narrow-phase collision queries.
    //!
    //! Deliberately empty pending the `World` design. It will hold the
    //! sphere/sphere, sphere/box, and ray/sphere tests that `main.js` currently
    //! implements inline (`collideSphereWithBox`, `raySphereDist`,
    //! `castWorldRay`).
}
