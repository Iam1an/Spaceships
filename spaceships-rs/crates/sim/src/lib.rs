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
//! Foundations, plus the rules and state layer. What exists today:
//!
//! - [`math::Vec3`] — replaces `THREE.Vector3`, which the JS uses everywhere.
//! - [`rng::Rng`] — the seeded generator that makes reproducible asteroid fields
//!   possible.
//! - [`collision`] — swept sphere/sphere and sphere/box tests. These replace the
//!   once-per-frame point-in-sphere checks in `bullets.js`, which a 780 u/s
//!   bullet outruns: it covers 39 units per step and the things it is shot at
//!   are 4–7 units across, so it passes through most of them without ever being
//!   tested against them.
//! - [`rules`] — every game constant, defined exactly once. The JS writes its
//!   rules twice, on the client and on the server, and they have drifted; this
//!   module resolves each divergence and records the decision next to the value.
//! - [`world`] — the simulation state, the tick contract, and the flat
//!   [`world::Frame`] the JS renderer consumes.
//!
//! And the behaviour, one module per subsystem:
//!
//! - [`ship`] — the flight model, the clocks, damage, death, respawn, and
//!   ship-versus-world collision.
//! - [`bullets`] — bolt ballistics, the hitscan beam, and every projectile
//!   impact, all resolved as swept segments.
//! - [`missiles`] — lock-on, homing, obstacle avoidance, flare seduction, and
//!   detonation.
//! - [`asteroids`] — seeded field generation, damage, and spin.
//! - [`bot`] — target selection, pursuit, evasion, and weapon use.
//! - [`campaign`] — waves, lives, checkpoints, and the capital ship.
//!
//! - [`tick`] — **the assembly.** [`tick::tick`] is the
//!   [`world::TickFn`]: it decides what order those six run in, reconciles the
//!   assumptions they were each written under, and produces the [`world::Frame`]
//!   and the [`world::NetIntent`]s. Its module docs carry the phase order and
//!   the reasoning behind every placement that is load-bearing.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub mod asteroids;
pub mod bot;
pub mod bullets;
pub mod campaign;
pub mod collision;
pub mod math;
pub mod missiles;
pub mod rng;
pub mod rules;
pub mod ship;
pub mod tick;
pub mod world;
