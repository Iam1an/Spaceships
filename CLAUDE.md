# Spaceships — CLAUDE.md

A browser-based 3D space shooter built with vanilla ES6 and Three.js. The server is Node.js with Express + a hand-rolled WebSocket layer.

> **Currently being rewritten in Rust.** The simulation, protocol, server, and renderer are all being ported — see [Rust Rewrite](#rust-rewrite) at the bottom and `BACKLOG.md`. Everything above that section describes the **current JS implementation**, which still runs the game. Where the Rust port has deliberately changed a rule, it is flagged inline.

---

## Testing

### Getting a quiet match — read this before testing anything

Most gameplay changes cannot be observed in a normal match, because you get killed before you can see anything. To get an **empty lobby**:

> **Multiplayer → Create Game → uncheck "Auto-fill uneven teams with bot" → Recruit Players**

No bot is added and you have the map to yourself — free to fly around, line up specific shots, and watch what actually happens. Use it for weapon behavior, collision, hit registration, camera work, HUD state, and performance profiling.

The toggle is `#autoBotInput` (checked by default), read by `autoBotEnabled()` in `public/src/lobby/rooms.js` and sent as `allowBot` on the `create` message.

### Other entry points

| Route | Good for |
|---|---|
| **Solo → Tutorial** | Player is immortal, no enemies — safest place to test flight and HUD |
| **Solo → Train with Robot** | Combat *with* an opponent; bad if you need to stay alive |
| **Solo → Time Trials** | Flight model and terrain; checkpoint courses, no combat |
| **Solo → Campaign** | Mission 3 is the only route to the capital-ship boss |

`playwright` is a devDependency. Guest login needs no credentials, so automated checks need no fixture accounts. Run **headed** when measuring anything GPU-related — headless Chromium falls back to SwiftShader (software GL) and the numbers are meaningless.

### Known local-only breakage

The client calls `/spaceships/api/*` but the server registers `/api/*`. These 404 under `npm start`; production sits behind a proxy that strips the prefix, and `npm run dev` works around it with a rewrite.

---

## Tech Stack

| Layer | Technology |
|---|---|
| 3D engine | Three.js **0.160.0, pinned exact in `package.json` and bundled** (was a CDN importmap; changed 2026-08) |
| Build | **Vite 5** — `npm run build` → `dist/` |
| Server framework | Express 4 + raw `http.createServer` |
| WebSocket | Custom `WSConn` class (RFC 6455, replaces `ws` package due to Node 25 regression) |
| Database | SQLite via `better-sqlite3` (sync) — file: `pilots.db` at project root |
| Auth | `bcryptjs` + `jsonwebtoken` (7-day tokens, `JWT_SECRET` env var) |
| Language | Vanilla ES6 modules everywhere — no TypeScript, no Babel |
| Dev tooling | Vite dev server with HMR; Playwright as devDependency |

```bash
npm start        # builds with Vite, then serves on :4000
npm run dev      # Vite dev server + HMR, proxies API and WebSocket to :4000
npm run dev:server  # the old behavior: node --watch server/index.js
npm run build    # build to dist/
```

The server prefers `dist/` and falls back to `public/`. **`three` and `vite` are regular dependencies, not devDependencies** — `prestart` runs the build, so `npm ci --omit=dev` would otherwise leave `npm start` unable to build, and with the importmap gone an unbuilt `public/` cannot resolve the bare `three` specifier (blank page, no error).

---

## Directory Structure

```
Spaceships/
├── server/
│   ├── index.js        — Express routes, WebSocket upgrade handler, all room/match logic
│   └── db.js           — better-sqlite3, schema, achievements, credits, rank system
├── spaceships-rs/      — Rust rewrite in progress (see bottom of this file)
├── public/
│   ├── index.html      — Single-page app; all CSS is inline in <style>
│   ├── spaceship.glb   — Default ship model
│   ├── spaceshipADMIN.glb — Unlockable admin ship (4.9 MB)
│   ├── moon Texture.jpg
│   ├── favicon.png
│   ├── sounds/         — boost, shoot, impact, rockbreak, shipdeath, hitmarker, move, etc.
│   └── src/            — All client-side ES6 modules
│       ├── main.js         — Game loop, Three.js scene, all gameplay logic
│       ├── lobby.js        — Entry-point module loaded by index.html (32 lines; splits into lobby/)
│       ├── lobby/          — dom, screens, settings, pilot, credits, profile, unlocks,
│       │                     customize, net, rooms, solo, launch, gamepadnav
│       ├── auth.js         — Login/register UI, JWT localStorage handling
│       ├── ship.js         — GLB loading, hull/accent color application
│       ├── camera.js       — ThirdPersonCamera
│       ├── fpcamera.js     — FirstPersonCamera (cockpit view)
│       ├── cockpit.js      — Cockpit interior geometry + profiles
│       ├── dash.js         — Cockpit instrument panel
│       ├── input.js        — Keyboard/mouse/gamepad/mobile input abstraction
│       ├── touchhud.js     — Mobile on-screen controls
│       ├── bullets.js      — Laser bolt projectile system
│       ├── beams.js        — Hitscan beam weapon system
│       ├── missiles.js     — Homing missile system
│       ├── trails.js       — Engine trail particle system
│       ├── asteroids.js    — Asteroid field rendering + hit detection
│       ├── bot.js          — Client-side bot AI (seek/attack/evade FSM)
│       ├── mothership.js   — Multiplayer team motherships
│       ├── graphics.js     — Opt-in "Ultra Graphics" renderer path
│       ├── warp.js         — Campaign warp transition effect
│       ├── audio.js        — Web Audio API wrapper
│       ├── customization.js— Ship color/trail/unlock UI with live 3D preview
│       ├── filter.js       — Profanity filter for callsign input (NOT a visual filter)
│       ├── moon.js / skybox.js / clouds.js
│       └── terrain.js / trees.js / airfield.js
├── BACKLOG.md          — Post-rewrite ideas (replay system) + known gameplay issues
└── CLAUDE.md
```

> `carrier.js` and `water.js` were deleted 2026-08 — neither was imported anywhere. The campaign boss lives in `main.js` (`buildCapitalShip`), not in `carrier.js`.

---

## Server (`server/index.js`)

- **Port**: `process.env.PORT || 4000`
- **Static**: `dist/` preferred, falling back to `public/`, with `Cache-Control: no-store`
- **WebSocket endpoint**: `/ws` — token via `?token=<jwt>`; guests allowed without token
- **Match timer**: 300 s (5 min) per multiplayer match
- **State broadcast**: 20 Hz tick, 1 Hz match-state broadcast
- **Respawn delay**: 2 s

### REST API

| Method | Path | Auth | Description |
|---|---|---|---|
| POST | `/api/register` | No | Register pilot (3–20 alphanum username, 6+ char password) |
| POST | `/api/login` | No | Returns JWT + full pilot stats |
| PUT | `/api/colors` | JWT | Save ship/accent hex colors |
| GET | `/api/profile/:username` | No | Full public stats + achievements |
| GET | `/api/leaderboard` | No | Top 50 by kills then wins |
| GET | `/api/unlocks` | JWT | Unlock status + costs map |
| POST | `/api/unlock/:feature` | JWT | Purchase unlock (deducts credits) |
| GET | `/api/credits` | JWT | Current balance |
| GET | `/api/credits/history` | JWT | Transaction log (`?limit=` 1–100) |
| POST | `/api/credits/spend` | JWT | Spend credits |
| POST | `/api/solo-result` | JWT | Record solo/skirmish match |
| POST | `/api/trial-result` | JWT | Record time trial (saves personal best only) |
| POST | `/api/campaign-result` | JWT | Record campaign mission completion |

**Important**: `/api/solo-result`, `/api/trial-result`, `/api/campaign-result` trust client-reported stats (kills, times, lives). This is a known, accepted risk — not a bug to fix.

### WebSocket Events

**Client → Server:**

| Event | Key fields | Description |
|---|---|---|
| `name` | `name` | Set callsign (16 chars) |
| `list-rooms` | — | Get public non-started rooms |
| `create` | `private`, `map`, `allowBot` | Create room; sender becomes host |
| `join` | `code` | Join by 4-letter code |
| `start` | — | Host-only; assigns teams, generates asteroids, broadcasts start |
| `state` | `pos`, `quat`, `boost` | Position update at ~20 Hz |
| `fire` | `kind`, `shots` | Weapon fired |
| `flare` | `pos`, `quat` | Flare deployed |
| `hit` | `targetId`, `fromBotId?`, `kind?` | Hit report; server validates + applies damage |
| `self-damage` | `dmg` | Self-inflicted damage (asteroid/terrain) |
| `asteroid-hit` | `id` | Bullet hit an asteroid |
| `bot-state` | `botId`, `pos`, `quat` | Host reports bot position |
| `bot-fire` | `botId`, `kind`, `shots` | Host reports bot firing |
| `colors` | `hullColor`, `accentColor` | Broadcast ship colors |
| `ship-model` | `modelUrl` | Broadcast model URL (external URLs rejected) |
| `leave` | — | Leave room |

**Server → Client:**

| Event | Key fields | Description |
|---|---|---|
| `room` | `code`, `host`, `you`, `private` | Confirms room entry |
| `players` | `players[]` | Roster update |
| `rooms-list` | `rooms[]` | Response to list-rooms |
| `start` | `spawns`, `asteroids`, `map`, `botAssignments` | Match begins |
| `state` | `id`, `pos`, `quat`, `boost` | Remote player position |
| `fire` / `flare` | `id`, … | Remote player fired/flared |
| `hp` | `id`, `hp` | HP after a hit |
| `death` | `id`, `killerId` | Ship destroyed |
| `respawn` | `id`, `pos`, `quat` | Ship respawned (2 s spawn protection) |
| `disconnect` | `id` | Player left mid-match |
| `match-state` | `timer`, `teamKills` | 1 Hz countdown + scores |
| `match-end` | `winner`, `teamKills` | Match over (-1 = draw) |
| `match-credits` | `creditsEarned`, `totalCredits`, `earned?` | Per-pilot reward after match |
| `asteroid-hp` / `asteroid-destroyed` | `id` | Asteroid damaged/destroyed |
| `colors` / `ship-model` | `id`, … | Remote player appearance |
| `error` | `message` | Room error |

These 35 messages are mirrored exactly in `spaceships-rs/crates/protocol`, with round-trip tests pinning every tag spelling. **Changing a message shape means changing both.**

One message exists in `protocol` and *not* here: `emp`, in both directions. See [The EMP](#the-emp-rust-port-only). An addition is only allowed when it is inert to the JS on both sides — this server drops unknown tags and both JS clients ignore them — and anything that is not inert is a change to `server/index.js` instead.

### Hit Validation (server-side)
- Target must be alive
- No friendly fire
- Spawn protection window respected (`invulnUntil`)
- Rate limit: 40 ms between bullet/beam hits, 400 ms between missiles
- Shooter must be alive (except missiles already in flight)
- Damage: 10 HP (bullet/beam), 50 HP (missile)

> The server validates the *circumstances* of a hit but never the *geometry* — it does not check that the shot could have connected. Positions are client-side, so a client can claim a hit it did not earn.

### Database Schema (`pilots.db`)

**`pilots`** — id, username (UNIQUE NOCASE), hashed_password, rank, high_score, games_played, created_at, ship_color, ship_accent_color, total_kills, total_deaths, matches_won, matches_lost, bots_killed, trial1_best–trial4_best (REAL, nullable), credits, unlock_colors, unlock_trail, unlock_hull, unlock_accent, unlock_trail_shape, campaign1_best_lives–campaign3_best_lives (INT, nullable), campaign_boss_kills, campaign_total_completions, unlock_admin_ship

**`achievements`** — id, pilot_id (FK), type (key string e.g. `kills_100`), earned_at, credited (0/1). UNIQUE on (pilot_id, type).

**`credit_transactions`** — id, pilot_id (FK), amount (positive=earned / negative=spent), reason, created_at

Schema migrations run as additive `ALTER TABLE ADD COLUMN` on every server start (idempotent).

`pilots.db` is gitignored and holds **real accounts** — never migrate or write to it during development. Copy it first.

### Rank Tiers (computed from `total_kills`)
13 tiers: Cadet → Grand Admiral.

### Credit Economy
| Action | Credits |
|---|---|
| Kill (MP) | ~variable per kill |
| Win bonus | bonus on match win |
| Trial completion | variable |
| Campaign mission 1/2/3 | 500 / 1000 / 2000 |
| Unlock hull color | 250 ⬡ |
| Unlock accent color | 400 ⬡ |
| Unlock trail | 500 ⬡ |
| Save colors | 50 ⬡ |
| Trail shape | 200 ⬡ |
| Admin ship | 125,000 ⬡ |

---

## Game Client (`public/src/main.js`)

All gameplay state lives as `let` variables closed over inside the `startGame()` async function — no external store or module-level globals.

### Game Modes

| Mode | `opts` fields | Description |
|---|---|---|
| Tutorial | `solo:true, mode:'tutorial'` | 10 guided steps, player immortal, no enemies |
| Train | `solo:true, mode:'train'` | 1v1 vs bot, 3-min timer |
| Skirmish | `solo:true, mode:'skirmish'` | 5v5 (4 allied + 5 enemy bots), 5-min timer |
| Time Trial | `solo:true, mode:'trials'/'trials2'/'trials3'/'trials4'` | Lap timing through rings, no enemies |
| Campaign | `solo:true, mode:'campaign', missionId:1/2/3` | 3 waves + boss, 3 lives, checkpoints |
| Multiplayer | `ws:<WebSocket>` | Real-time PvP, server semi-authoritative |

### Maps
- **Space** (default): Skybox, moon (radius 80 at origin), 2 motherships at Z=±600, asteroid field.
- **Terrain (Sierras)**: Heightmap ground, trees, clouds, fog (`Fog(0xbbd5f0, 1400, 4800)`), 2 airfields at Z=±1500. Ground contact = instant death. *Rebuilt from scratch in the Rust port — see [The Sierras](#the-sierras-rust-port-only) below. The JS version described here is unchanged and still what `public/src/terrain.js` draws.*

### Physics & Movement

| Constant | Value | Notes |
|---|---|---|
| `MAX_THROTTLE` | 80 u/s | Base top speed |
| `BOOST_FACTOR` | 1.7× | Speed multiplier while boosting |
| `KEY_THROTTLE_RATE` | 30 u/s² | W/S key acceleration |
| `THROTTLE_STEP` | 6 u/s | Scroll wheel increment |
| `VELOCITY_BLEND` | 4 | Normal velocity convergence damping |
| `PITCH_RATE` | 1.75 rad/s | |
| `YAW_RATE` | 1.3 rad/s | |
| `ROLL_RATE` | 1.4 rad/s | |
| `STEER_DEADZONE` | 0.05 | Input raised to power 1.6 for feel |
| `MAX_BOOST` | 10 s | Boost fuel tank |
| `BOOST_DRAIN` | 2/s | |
| `BOOST_RECHARGE` | 4/s | After 1.0 s idle |
| `DRIFT_DRAG` | 0.9/s | Momentum bleed in drift mode |
| `DRIFT_GRIP` | 0.3 | Velocity rotation toward facing in drift |
| `DRIFT_BRAKE` | 0.1 | Hard brake during S+drift |
| `BRAKE_FULL_TIME` | 1.4 s | Time to build full brake charge |
| `BRAKE_BOOST_DURATION_MAX` | 1.0 s | Duration of brake-release boost |
| `BRAKE_BOOST_BONUS_MAX` | 50 u/s | Extra speed from brake-release |
| `BRAKE_OVERCHARGE_WARN` | 1.0 s | Yellow overload warning threshold |
| `BRAKE_OVERCHARGE_DAMAGE` | 2.0 s | Overcharge damage begins |
| `BRAKE_OVERCHARGE_DPS` | 10 HP/s | Overcharge self-damage rate |

### Weapons

**Bullets (default)**
- Speed: 780 u/s | Cooldown: 0.05 s | Ammo cost: 1 | Damage: 10 HP | Range: ~1560 u (2 s life)
- Hit radius: 6.0 u (mouse) / 7.0 u (keyboard/mobile)

**Beams (press P to toggle)**
- Cooldown: 0.25 s | Ammo cost: 3 | Damage: 10 HP | Instant hitscan | Range: 1000 u
- Hit radius: 5.5 u

**Ammo**
- `MAX_AMMO = 90` | Regen: 36/s after 1.0 s idle
- "Overheat" is not a separate heat state: `heat01 = ammo / MAX_AMMO`, and "overheated" means `ammo < cost`.

**Missiles (E key)**
- `MISSILE_MAX = 4` | Speed: 160 u/s | Turn rate: 1.4 rad/s | Life: 8.0 s | Damage: 50 HP
- Flare countermeasure range: 180 u | `FLARE_MAX = 3`
- Lock-on has **no cone and no range** — it is nearest living enemy with line of sight, so a target directly behind you is lockable.

### Health System

| Constant | Value |
|---|---|
| `SHIP_MAX_HP` | 100 |
| `RESPAWN_DELAY` | 2.5 s (local); 2 s (server) — *unified to 2.0 in the Rust port* |
| `SPAWN_INVULN_DURATION` | 2.0 s — *player-only in JS; universal in the Rust port* |
| `HEALTH_REGEN_DELAY` | 2.0 s (no damage AND no firing) |
| `HEALTH_REGEN_INTERVAL` | +1 HP per 0.1 s |
| Asteroid collision damage | 15–29 HP (random, rising edge only) |
| Campaign respawn HP | 55% (55 HP) |

### Asteroids

| Tier | Size | HP | Spawn weight |
|---|---|---|---|
| small | 5–7 | 5 | 45% |
| medium | 9–15 | 10 | 30% |
| big | 18–30 | 30 | 18% |
| huge | 38–55 | 50 | 7% |

- 6 icosahedron shape variants with procedural noise displacement
- Count by mode: deathmatch 60 | trials 1–4: 120/150/180/210 | campaign 280
- Asteroid ids are 1-based on the client and 0-based on the server (*unified to 0-based in the Rust port*)

### Bot AI (`bot.js`)

- States: `seek` → `attack` → `evade`
- Speed: 60 u/s | Turn rate: 1.3 rad/s
- Fire range: 600 u | Fire dot: 0.97 | Normal cooldown: 0.15 s | Hard mode: 0.05 s
- Seek distance: 250 u | Evade duration: 0.6 s
- Avoid lookahead: 80 u
- Missiles: 1 (normal) / 3 (hard mode), cooldown 8.0 s
- In multiplayer: host drives bots and sends `bot-state`/`bot-fire` over WebSocket
- Bots fire **two** projectiles per shot: a visual bolt that cannot damage (`bullets.js` gates damage on `isLocal`) and a private shadow projectile at radius 4.0 that does. *Collapsed to one path in the Rust port.*
- Bots never deploy flares, so they cannot be decoyed while players can. *Added in the Rust port.*

### Campaign Boss (Capital Ship)

- `BOSS_MAX_HP = 2500` | 20 hitboxes in 4×5 grid (x = ±85/±28, z = 0/±75/±150), all at y=0 except one
- 4 turrets that aim + fire at player
- Fire rate by HP: >65% → 2.8–3.5 s | 35–65% → 1.6–2.1 s | <35% → 0.9–1.2 s
- Bullet: 14 HP damage, 430 u/s, 4.2 s life, ±0.09 jitter
- Patrols: 88-unit X swing + 9-unit Y bob (sinusoidal)
- Key functions: `buildCapitalShip()`, `updateCapitalShip(dt)`
- Wave positions (Z): Wave 1 = -280 | Wave 2 = +20 | Wave 3 = +330 | Boss = +600
- `fireFromBoss()` is **dead code** — never called, superseded by the turret path. `bossFireTimer` is written and never read.
- The sphere grid leaves gaps a shot can thread (75-unit rows against 56-unit diameters). The beam hid this by testing one radius-95 sphere over the whole ship. *Replaced by an AABB hull in the Rust port.*

### Campaign Mode

- 3 missions unlocked sequentially (`localStorage['spaceships:campaign{1/2/3}Beat']`)
- Player has 3 lives (`campaignLives`). Death → warp flash → respawn at `campaignCheckpointPos`
- Campaign phases 0–4: waves then boss
- Mission bot counts: M1 = 3/5/4 | M2 = 4/6/5 | M3 = 5/7/6 (wave1/wave2/wave3)
- `genCampaignAsteroids()` — 280 asteroids in 3 linear zones along Z axis. Performs **no** avoidance test, and its middle zone is centred on the moon.
- `#campaign-warp-flash` CSS animation on death

### Time Trials

- 4 circuits: 12 / 14 / 16 / 18 checkpoints
- Checkpoint ring: `TorusGeometry(48, 3.5)`, trigger at 55 u
- Timer starts on second crossing of CP0
- Crossing a checkpoint grants +3.5 s boost fuel
- Bests stored in `localStorage`

### Aim Assist

- Toggle: C key (forced on for keyboard/mobile schemes)
- Cone: 53° (mouse) / 60° (keyboard) | Max range: 1000 u
- Max pull: 2.6 rad/s (mouse) / 2.2 rad/s (keyboard)
- Leads to bullet intercept point (`solveIntercept`)
- LOS raycasts respect asteroids + moon
- Sticky bonus +0.05 dot on current target to prevent flicker
- Pull scales down with deliberate steering input
- **Known bug**: `main.js` passes `shipVelocity` as the shooter velocity, but bullets get no velocity inheritance (`bullets.js` gives a bolt `direction * SPEED`). The assist over-leads, worse the faster you fly. `bot.js` passes a zero vector and is correct.

### Rendering

- `WebGLRenderer`, antialiasing on, pixel ratio capped at 1.5, BasicShadowMap
- `PerspectiveCamera` FOV 75, near 0.1, far 2500 (5000 on terrain)
- **PSX pixel filter** (default ON): renders to 1/3-res `WebGLRenderTarget` with `NearestFilter`, blits via fullscreen quad. Toggle stored in `localStorage['spaceships:pixelFilter']` (`'0'` = off).
- **Ultra Graphics** (`graphics.js`, opt-in): upgraded materials/shadows. Toggle at `localStorage['spaceships:ultraGraphics']`.
- Frame delta capped at 0.05 s to prevent physics explosions on tab-focus
- Space lighting: `AmbientLight(0xffffff, 0.35)` + `DirectionalLight(0xffffff, 1.1)` at (200,300,100)
- Terrain lighting: ambient `0xfff8e8` + directional `0xfff5cc` that follows player at Y+500

### Multiplayer vs Singleplayer

- **Position/rotation**: fully client-side, each player broadcasts at ~20 Hz. No server physics.
- **Hit detection**: hybrid — shooter detects collision, sends `hit`; server validates + applies.
- **Asteroids**: generated server-side at match start, HP tracked server-side. In solo they are purely client-side — that asymmetry has caused real bugs.
- **Match timer**: server-authoritative.
- **Bots (MP)**: host runs AI locally, broadcasts `bot-state`/`bot-fire`.
- **Stats**: server persists for all authenticated pilots at `match-end`.

---

## Frontend / UI (`index.html` + `public/src/`)

All CSS is inline in `index.html`. No external CSS files exist. Font: Orbitron (Google Fonts).

### CSS Design Tokens

| Variable | Value |
|---|---|
| `--glass-bg` | `rgba(6,12,24,0.65)` |
| `--glass-border` | `1px solid rgba(102,221,255,0.2)` |
| `--glass-blur` | `blur(16px)` |
| `--color-primary` | `#4aa3ff` |
| `--color-blue` | `#66ddff` |
| `--color-gold` | `#ffe07a` |
| `--color-red` | `#ff5566` |
| `--color-green` | `#66ff88` |

Body: `radial-gradient(ellipse at center, #1a2540 0%, #03050a 85%)`

### Lobby Screens (`#lobby` swaps `.screen` panels)

1. **`#lobby-main`** — Main menu: title, callsign input, Multiplayer / Single Player / Campaign buttons
2. **`#lobby-multi`** — Multiplayer hub: Create Game, Find Game, Back
3. **`#lobby-create`** — Room creation: Open/Private, Space/Sierras map, bot toggle
4. **`#lobby-find`** — Room browser: scrollable list + manual code entry
5. **`#lobby-room`** — Waiting room: room code, privacy badge, player list, Launch/Wait
6. **`#lobby-single`** — Singleplayer hub: map select, Tutorial/Train/Skirmish/Time Trials
7. **`#lobby-tutorial`** — Control scheme picker
8. **`#lobby-trials`** — Trial 1–4 buttons with lock state
9. **`#lobby-campaign`** — Mission 1–3 select (Operation Ironclad / Stormfront / Final Siege)

Screen wiring now lives in `lobby/screens.js`; room flow in `lobby/rooms.js`; solo/campaign gating in `lobby/solo.js`.

### Persistent Overlays

- **`#auth-overlay`** — Login/register (LOGIN / ENLIST tabs, "Play as Guest" link)
- **`#settingsPanel`** — Right-side drawer: music/SFX sliders, control scheme, Show Stats, Secret Hard Mode, Retro Pixel Filter, Enemy Trails, Log Out
- **`#customization`** — Right-side 440px drawer: live 3D preview canvas, HULL/ACCENT/TRAIL tabs, color wheel + brightness, trail shape picker, Admin Ship purchase
- **`#profile-overlay`** — PROFILE / LEADERBOARD tabs: stats grid, trial bests, achievements with progress bars
- **`#credits-display`** — Top-left lobby credits counter (⬡ symbol)

### In-Game HUD

| Element | Position | Content |
|---|---|---|
| `#hud-stats` | Top-left | Speed, position, player count (toggleable) |
| `#killfeed` | Top-left below stats | Last 5 kills, slide-in/out, 3.6 s fade |
| `#matchhud` | Center-top | Blue score \| timer \| Red score (MP only) |
| `#reticle` | Center | 16×16 cyan crosshair; `.locked` = red + scale up (missile lock) |
| `.target-box` / `.target-label` | Floating | Red corner brackets + pilot name on enemy |
| `.lead-marker` | Floating | Dashed red circle for lead aim; fills solid when aligned |
| `#missile-lock-warning` | Center-top | "⚠ MISSILE LOCK ⚠" red blink at 0.25 s |
| `#healthbar` | Bottom 32px | 400px wide, green→red at low HP, "X / 5" display |
| `#chargebar` | Bottom 124px | 8px, orange→yellow; `.overload` pulses red |
| `#boostbar` | Bottom 64px | 12px blue gradient; "BOOST" label |
| `#heatbar` | Bottom 84px | 12px orange gradient; "GUN" label; `.overheated` pulses red |
| `#missilehud` | Left-center-bottom | "MSL" + 4 orange pip bars |
| `#flarehud` | Right-center-bottom | "FLR" + 3 yellow pip bars |
| `#hit-vignette` | Fullscreen | Red radial vignette on damage |
| `#deathbanner` | Center | "DESTROYED" on death |
| `#campaign-hud` | Center-top | Wave, objective, enemy count, ❤❤❤ lives |
| `#campaign-boss-bar` | Bottom | Boss name, red HP bar |
| `#campaign-failed` | Fullscreen | "MISSION FAILED" + retry/return buttons |
| `#scoreboard` | Center (Tab) | Pilot / Kills / Deaths table, you-row gold |
| `#matchresult` | Center | Win/Loss result card post-match |
| `#achievement-toasts` | Bottom-right | Slide-in toasts, 3.5 s auto-fade |
| `#ad-overlay` | Fullscreen | AdSense slot shown between matches |

### Input System (`input.js`)

**Mouse + Keys (default):**
- Mouse → pitch/yaw | Left click / F → fire | Right drag → free-look | Scroll → throttle
- W/S → throttle | A/D → roll | Shift → boost | Space → drift
- E → missile | Q → flare | P → toggle gun mode | C → aim assist | Tab → scoreboard

**Keyboard only:**
- Arrow keys / WASD steering with aim-assist forced on

**Mobile:**
- Left virtual joystick (80px radius) | Right-side buttons: FIRE, DRIFT, BOOST, roll ×2, MSL, FLARE
- Vertical throttle slider (sticky)

**Gamepad (auto-detected):**
- Right stick → steer | Left stick → roll + throttle | RT/A → fire | LT → drift | LB → boost
- RB/X → missile | B → flare | Y → gun toggle | Start → menu | Deadzone: 0.12

### Ship Customization (`customization.js`)

- Three.js WebGLRenderer scene with rotating ship on metallic pedestal
- Hull vs accent split by luminance threshold < 0.35
- `setColor(hex)` / `setAccentColor(hex)` apply to respective mesh groups
- Auto-rotates at 0.007 rad/frame; torus ring pulses at ~1.8 Hz
- Colors stored in localStorage: `spaceships:shipColor`, `spaceships:shipAccentColor`, `spaceships:trailColor`, `spaceships:trailShape`
- Trail shapes: `circle`, `square`, `triangle`, `star`, `david`

---

## Authentication Flow

1. Client stores JWT in `localStorage['spaceships:token']`
2. HTTP routes: `Authorization: Bearer <token>` header
3. WebSocket: `?token=<jwt>` query param
4. Guest play fully supported — no token, no stats saved
5. JWT secret: `JWT_SECRET` env var (falls back to dev default — set in prod)

---

## Key localStorage Keys

| Key | Purpose |
|---|---|
| `spaceships:token` | JWT auth token |
| `spaceships:shipColor` | Hull color hex |
| `spaceships:shipAccentColor` | Accent color hex |
| `spaceships:trailColor` | Trail color hex |
| `spaceships:trailShape` | Trail shape string |
| `spaceships:pixelFilter` | `'0'` = disable retro filter |
| `spaceships:ultraGraphics` | Ultra Graphics toggle |
| `spaceships:campaign1Beat` / `2Beat` / `3Beat` | Campaign mission completion flags |
| `spaceships:trial1Best`–`trial4Best` | Time trial personal bests |

---

## Known Intentional Design Decisions

- **Stat farming**: `/api/solo-result`, `/api/trial-result`, `/api/campaign-result` trust client-reported values (kills, times, lives). No server validation. This is a known, accepted risk — do not flag as a security issue.
- **`ws` package in dependencies**: superseded by custom `WSConn` class (Node 25 regression workaround). The package remains in package.json but is not used.
- **All CSS inline**: in `index.html` — no external stylesheet. `filter.js` is a *profanity* filter, not a visual/CSS filter.
- ~~**No build pipeline**: intentional. Three.js via CDN importmap, no bundler. Don't suggest adding Vite/Webpack.~~ **Superseded 2026-08.** Vite was added deliberately: three.js is now bundled from node_modules rather than fetched from unpkg at runtime, and a build step is a prerequisite for loading the Rust/WASM client. `vite.config.js` documents what changes when `wasm-pack` output is added.

---

## Rust Rewrite

In progress on branch `rust-port`. The goal is an all-Rust game: a Bevy renderer targeting **both** native macOS (Metal) and the browser (WASM), on a Rust server. The JS in `public/src/` is retired once the Bevy client reaches parity.

```
spaceships-rs/
├── crates/sim        Deterministic simulation. 430 tests. ZERO dependencies.
├── crates/protocol   The 35 WebSocket messages. 48 round-trip tests.
├── crates/server     Replacing server/ — axum + tokio-tungstenite + rusqlite.
└── crates/client     Bevy 0.19 renderer, native + wasm.
```

### Rules for these crates

- **`crates/sim` must stay deterministic.** No I/O, no wall-clock, no unseeded RNG, no third-party dependencies, no `HashMap` iteration affecting results. Same seed and inputs must produce bit-identical output on every platform, because the same code runs on the server and in a WASM client. `sin`/`cos`/`acos`/`pow` from libm are **banned on simulation paths** — they differ in the last bits across glibc/musl/Apple/WASM. Hand-rolled deterministic versions exist; use those.
- **Game constants live in `crates/sim/src/rules.rs`, defined exactly once.** The JS wrote its rules twice — client and server — and they drifted apart, which is where a whole class of bugs came from. Never hardcode a number that belongs there.
- **`protocol` must not depend on `sim`, and `sim` must not depend on `protocol`**, so the wire format cannot drift when the sim's math changes.
- The `sim` crate is renderer-agnostic and Bevy consumes it as a plain resource — it does **not** need converting to ECS.

### Why determinism matters beyond netcode

It also makes the planned replay system nearly free: a replay is a seed plus an input log, re-simulated, rather than recorded state. See `BACKLOG.md`.

### The Sierras (Rust port only)

The terrain map was rebuilt and **deliberately diverges from `public/src/terrain.js`**. The JS map is untouched and still runs the JS game; nothing below applies to it.

**The heightfield is simulation, not rendering.** It lives in `crates/sim/src/terrain.rs`. `ship::terrain_height` is a forward to it. A new map means changing it *there* — changing only the renderer gives you ground that looks different and still kills you at the old altitude.

**The terrain is a triangulated lattice, and the lattice is the definition.** `node_height` evaluates the map at a lattice node (150 quads a side, 24-unit cells); between nodes the height is the plane of the containing triangle. The client draws one triangle per lattice face, so the drawn surface *is* the collision surface rather than an approximation of it — which is where the low-poly look comes from and why there is no LOD scheme. `terrain::ground_height` and `client/terrain/ground.rs` must keep the same cell split; both carry tests that pin it.

**No transcendentals.** The noise is hash-based value noise with a quintic fade — integer mixing plus `+ - * / sqrt`. The old sine sum was the crate's largest determinism hazard. Do not reintroduce `sin`/`cos` here; `math::det` is not needed either.

| | JS | Rust port |
|---|---|---|
| Height function | 11 `sin`/`cos` per sample | hash noise, no transcendentals |
| Client mesh | 384² segments, 295k triangles, smooth | 150² lattice, 45k triangles, flat-shaded |
| Below sea level | clamped to 0 | allowed — `water_level` (0) is a real surface |
| Airfields | flat at `y = 0`, in a pit | mesas at `airfield_elevation` (210) |
| `airfield_z` | ∓1500 | ∓1400, so the mesa ramp finishes inside the map |
| Spawn `terrain_y` | 40 | `airfield_elevation + 40` |
| Sun | straight down | fixed raking direction |

New `WorldRules` fields: `water_level`, `airfield_elevation`. `SpawnRules::terrain_y`/`terrain_z` are now derived from them rather than written out.

**Water is solid.** `terrain::ground_height` is the *bed* and may be negative; `surface_height` (what `ship::terrain_height` returns) is `max(bed, water_level)`. The kill plane and the bots' `height_at` both use the surface, so a lake stops a ship exactly as a hillside does. Scenery placement wants the bed.

**Dev tools.** `cargo run -p spaceships-sim --example heightmap -- out.png` renders a plan view of the map — the fastest way to see a layout mistake. `SPACESHIPS_START=x,y,z` puts the player anywhere, for screenshots.

### The EMP (Rust port only)

A weapon the JS game does not have: it deals no damage and takes a pilot's *information* instead. `BACKLOG.md` §2 is the spec; `crates/sim/src/emp.rs` is the implementation and its module docs carry every decision.

`G` fires it. It is a **sphere centred on the firing ship** — no aim, no travel time, `emp.radius` 300 units — and it costs the whole `emp.charge` meter, which fills over `emp.charge_time` (60 s) and is deliberately **not** reset by `ship::respawn`, so dying neither refunds it nor arms a fresh spawn.

Everyone caught flies blind for `emp.blind_duration` (4 s), which switches off, in five places:

| What | Where |
|---|---|
| Aim assist — cone, pull, lead marker | `aim_assist::update` treats blind exactly as dead |
| Missile lock, and therefore launches | `missiles::acquire_lock` returns `None` |
| Cockpit lighting, instruments, radar, annunciators | `cockpit.rs`'s `CockpitPower::emp` |
| The whole head-up display — tapes, meters, pips, brackets, boresight | `hud.rs`'s `HudModel::unpowered` |
| The voice warnings | `audio.rs`, after one `JAMMER` callout |

**Not** the flight model and **not** the guns. The match scoreline, the kill feed, the death banner and the hit vignette also survive — the aeroplane stops talking, the match does not.

Allies are caught (`emp.friendly_blind`), the firing pilot is not (`emp.blinds_owner`), and a ship inside its spawn-protection window is not. Bots are caught too: no missiles, no flares, and `emp.bot_aim_error_scale` (6x) on their aim wander.

**Multiplayer is partial against the JS server, on purpose.** `emp` is the only message in `crates/protocol` with no `server/index.js` counterpart. That server drops unknown tags, so an EMP fired in a Node-hosted match blinds only the ships the firing client simulates — the host's bots — and no remote human. `crates/server` relays it and cross-Rust play is complete. Do not "fix" this by editing `server/`.

**Dev tools.** `SPACESHIPS_EMP=<seconds>` fires one at yourself at that time, with `blinds_owner` on and `charge_time` at zero; `SPACESHIPS_SHOT_AT=<seconds>` moves `SPACESHIPS_SCREENSHOT`'s shutter, which is what makes a four-second effect photographable. `SPACESHIPS_FX_SCENE=emp@0.5` stages the wavefront alone, from outside.
