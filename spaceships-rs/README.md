# spaceships-rs

A Rust workspace for the in-progress port of the Spaceships game server and
simulation. **Nothing here is wired into the running game yet.** The JavaScript
game in `../server/` and `../public/` is untouched and remains the thing that
actually serves and renders Spaceships; this directory is entirely additive and
can be deleted without affecting it.

```
spaceships-rs/
├── Cargo.toml              workspace root, resolver 2, shared dependency versions
├── rustfmt.toml
├── clippy.toml
├── .gitignore              /target
└── crates/
    ├── protocol/           the wire contract — the part that is actually done
    │   ├── src/lib.rs      ClientMessage / ServerMessage + payload structs
    │   ├── src/consts.rs   server constants observable through the protocol
    │   └── tests/roundtrip.rs
    ├── sim/                deterministic simulation core — skeleton
    │   ├── src/lib.rs      module skeleton + the determinism rules
    │   ├── src/math.rs     Vec3 (replaces THREE.Vector3)
    │   └── src/rng.rs      seeded PCG32
    └── server/             binary stub — prints a placeholder, serves nothing
        └── src/main.rs
```

## Build and test

```sh
cd spaceships-rs

cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets
cargo fmt --all
```

Requires Rust 1.85+ (developed against 1.97.1). `wasm-pack` is **not** required
and is not used yet — the eventual browser build of `sim` will need it, but no
crate here targets `wasm32` today.

## The crates

### `spaceships-protocol`

A transcription of the JSON messages the current JS server and browser already
exchange over `/ws`. Two internally-tagged enums, `ClientMessage` (browser ->
server, 16 variants) and `ServerMessage` (server -> browser, 19 variants), plus
the payload structs they carry.

This crate is **descriptive, not aspirational**. Every field, name, and casing
was read out of `../server/index.js`, `../public/src/main.js`, and
`../public/src/lobby.js`. It exists so a Rust server can serve the existing
unmodified browser client, and a future Rust/WASM client can talk to the
existing unmodified Node server. Changing a name here is a protocol break, not a
refactor.

`tests/roundtrip.rs` covers every variant: it deserializes a JSON literal shaped
like what the JS actually sends, re-serializes it, and asserts the two documents
match structurally. `tag_spelling_is_exact` additionally pins the full list of
`type` tags in both directions, so adding a variant without thinking about the
wire fails the build.

Anything that could not be pinned down with certainty is marked `TODO(verify):`
in the source rather than guessed at silently.

### `spaceships-sim`

The deterministic simulation core. **The rule for this crate: bit-identical
output from bit-identical input, on every machine, every run.** The same code is
meant to run in the authoritative server and, compiled to WASM, in the browser
for client-side prediction — and the two only agree if the simulation is
perfectly reproducible.

That means this crate contains, and must keep containing:

- **no I/O** — no files, no sockets;
- **no rendering** — rendering stays in Three.js in `../public/`, this crate only
  produces the numbers the renderer draws;
- **no networking** — it does not depend on `spaceships-protocol`, and
  `spaceships-protocol` does not depend on it, so the wire format cannot drift
  when the sim's math changes;
- **no wall-clock time** — time advances only through an explicit fixed timestep
  passed into the tick function;
- **no unseeded randomness** — everything comes from `rng::Rng`, seeded by the
  caller;
- **no third-party dependencies** — its `[dependencies]` table is deliberately
  empty, and `#![forbid(unsafe_code)]` is on.

Two pieces are real and fully tested; the rest is an intentionally empty
skeleton, because the `World` struct and tick signature are being designed
separately.

- `math::Vec3` — the `THREE.Vector3` replacement, `f64` so values survive the
  JSON round trip unchanged. Add/sub/scale/dot/cross/length/normalize/distance
  plus the helpers the port will want (`add_scaled`, `lerp`, `clamp_length`,
  `project_onto`, `reflect`). `normalize()` deliberately returns zero for a zero
  vector, matching `THREE.Vector3.normalize`, so ported JS keeps its behaviour.
- `rng::Rng` — seeded PCG-XSH-RR 64/32, hand-rolled, verified against the
  reference algorithm. This is what makes a reproducible asteroid field
  possible: instead of the server generating 60 asteroid records and shipping
  them all inside `start`, it can ship one `u64` seed and both sides generate the
  identical field. `golden_sequence_is_pinned` locks the output so the algorithm
  cannot change silently — a changed algorithm desynchronizes clients from
  servers.

### `spaceships-server`

A stub binary. `main()` prints a placeholder; the module docs enumerate what it
will eventually replace — the Express routes, the hand-rolled RFC 6455 WebSocket
implementation at `../server/index.js:198-398`, the in-process room/lobby state
machine, and the SQLite persistence in `../server/db.js`. No `axum`, no `tokio`,
no `rusqlite` yet: those choices are downstream of a design that has not been
made, and adding a runtime now would bias it.

## What stays in JavaScript

Rendering. All of `../public/` — Three.js scene graph, models, materials,
shaders, HUD, audio — stays exactly where it is. The boundary is that the
simulation produces numbers and `../public/` draws them.
