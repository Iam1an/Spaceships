# Spaceships

3D multiplayer space combat. Three.js client, Node/Express + SQLite server,
hand-rolled WebSocket protocol.

**Currently mid-rewrite to Rust.** See `spaceships-rs/` and `BACKLOG.md`.

---

## Testing the game

### Getting into a quiet match — do this before anything else

Most gameplay changes cannot be tested in a normal match, because you get
killed before you can observe anything. To get an **empty lobby** with nothing
shooting at you:

> **Multiplayer → Create Game → uncheck "Auto-fill uneven teams with bot" →
> Recruit Players**

That toggle is `#autoBotInput` (checked by default), read by `autoBotEnabled()`
in `public/src/lobby/rooms.js:59` and sent as `allowBot` on the `create`
message. Unchecked, no bot is added and you are alone in the map — free to fly
around, line up specific shots, and watch what actually happens.

Use this for anything that needs sustained observation: weapon behavior,
collision, hit registration, camera work, HUD state, performance profiling.

### Other entry points

- **Solo → Train with Robot** — one bot. Good for testing combat *with* an
  opponent, bad for anything requiring you to stay alive.
- **Solo → Trials** — checkpoint courses, no combat at all. Best for testing
  flight model and terrain.
- **Solo → Campaign** — three missions; mission 3 has the capital-ship boss.
  The only way to reach boss code.

### Automated testing

`playwright` is a devDependency. Drive the game headlessly for regression
checks — guest login works without credentials, so no fixture accounts are
needed.

Run **headed** when measuring anything GPU-related: headless Chromium falls
back to SwiftShader (software GL) and the numbers are meaningless.

---

## Running

```bash
npm start        # builds with Vite, serves on :4000
npm run dev      # Vite dev server with HMR, proxies API + WebSocket to :4000
npm run build    # build to dist/
```

The server prefers `dist/` and falls back to `public/`. `three` is pinned to
exactly 0.160.0 and bundled — it is **not** loaded from a CDN.

### Known local-only breakage

The client calls `/spaceships/api/*` but the server registers `/api/*`. These
404 under `npm start`; production sits behind a proxy that strips the prefix.
`npm run dev` works around it with a rewrite.

---

## Layout

```
public/src/       Three.js client. main.js is the game loop; lobby.js is the
                  entry point and splits into lobby/.
server/           Express + hand-rolled WebSocket + SQLite (pilots.db).
spaceships-rs/    Rust rewrite in progress.
  crates/sim        Deterministic simulation. Zero dependencies by design.
  crates/protocol   The 35 WebSocket message types.
  crates/server     Replacing server/.
  crates/client     Bevy renderer, native macOS + wasm.
BACKLOG.md        Post-rewrite ideas and known gameplay issues.
```

## Rules for the Rust crates

- `crates/sim` must stay deterministic: no I/O, no wall-clock, no unseeded RNG,
  no third-party dependencies, no `HashMap` iteration affecting results. Same
  seed and inputs must produce bit-identical output on every platform, because
  the same code runs on the server and in a WASM client. This is also what
  makes the planned replay system cheap.
- Game constants live in `crates/sim/src/rules.rs`, defined exactly once. The JS
  wrote its rules twice — client and server — and they drifted. Never hardcode a
  number that belongs there.
- `protocol` must not depend on `sim`, and `sim` must not depend on `protocol`.
