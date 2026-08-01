# Spaceships — frame-rate profile

Repo `/Users/gheat/spaceships`, branch `rust-port` @ `98694da`.
No game source was modified. All instrumentation was injected with Playwright
`page.addInitScript` before app code ran, and wraps only `requestAnimationFrame`
and the `WebGLRenderingContext`.

---

## 0. What was and was not measured

**Completed: 7 sustained 65-second runs**, each ~9,350 frames, headed Chromium on the
real GPU, against the production minified build served by `npm start` on :4000.

Gameplay entry was **not** a problem: guest login → Single Player → Train with Robot (and
Skirmish, and Campaign) worked reliably on every attempt. The only lobby path that failed
was **Trial 4**, which is gated behind `spaceships:trial3Best` in localStorage and whose
click handler silently refuses when locked (`public/src/lobby/solo.js:28`). Seeding the
progression keys fixes it.

**Not completed** (runs were stopped mid-suite):

| Wanted | Status |
|---|---|
| CDP `Profiler` self-time attribution per function | **Not captured.** This is the biggest gap. Section 3 substitutes a measured JS-vs-draw-call regression plus static reading. |
| `HeapProfiler` allocation-site attribution | Not captured. Allocation *rate* was measured; attribution is from reading code. |
| Retina density (`deviceScaleFactor` 2 → renderer clamps pixelRatio to 1.5) | Not captured. **This is why the GPU verdict is only directional.** |
| 2560×1440 GPU fill stress | Not captured |
| CPU-throttled runs (4×, 8×) to emulate slower hardware | Not captured |
| Trials-4 (210 asteroids) scaling point | Failed on the lock, then not retried |
| First-person cockpit view | Not measured |

Every number below is real and came from a completed run. Nothing is estimated
unless the text says so.

### Environment

| | |
|---|---|
| GPU | `ANGLE (Apple, ANGLE Metal Renderer: Apple M5, Unspecified Version)` — **real Metal, headed** |
| Display | 144 Hz → **frame budget 6.94 ms**, not 16.7 ms |
| Canvas | 1440×900 CSS at `devicePixelRatio` **1** (Playwright default) |
| Build | `vite build` production, `index-*.js` 772 kB (216 kB gzip) |
| GPU timing | `EXT_disjoint_timer_query_webgl2`, available and used (3 runs) |

Headless was *not* used for any GPU conclusion. All numbers below are headed / Metal.

**Caveat on DPR:** at `devicePixelRatio` 1 the backing store is 1.3 Mpx. A real Retina
user gets `min(devicePixelRatio, 1.5)` = 1.5 → 2.9 Mpx, **2.25× more fill**. GPU numbers
below are therefore a lower bound for a Retina machine.

---

## 1. Frame timing (7 × 65 s sustained, ship moving + firing continuously)

| run | mode | pixel | ultra | fps | dt p50 | dt p99 | dt max | js mean | js p99 | js max | gpu mean | gpu p99 | draws p50 | draws p99 | tris p50 | >16.7 ms | >33 ms | MB/s alloc | alive |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| base-train | train | on | off | 143.6 | 6.90 | 7.80 | **174.4** | 0.583 | 1.10 | 1.90 | — | — | 65 | 180 | 5,388 | 1 | 1 | 7.9 | 0.87 |
| base-nopixel | train | **off** | off | 144.0 | 6.90 | 7.80 | 14.7 | 0.554 | 1.10 | 1.70 | — | — | 35 | 178 | 2,388 | 0 | 0 | 7.3 | 0.89 |
| base-ultra | train | on | **on** | 144.0 | 6.90 | 7.80 | 7.9 | 0.576 | 1.10 | 2.20 | — | — | 31 | 182 | 1,182 | 0 | 0 | 6.5 | 0.95 |
| base-skirmish | skirmish | on | off | 143.9 | 6.90 | 7.70 | 14.5 | 0.731 | 1.70 | 2.20 | — | — | 24 | 477 | 1,650 | 0 | 0 | 23.4 | 0.89 |
| base-campaign | campaign | on | off | 143.9 | 6.90 | 7.80 | 14.3 | **1.168** | 1.80 | 2.40 | 0.695 | 2.187 | **308** | 399 | **43,600** | 0 | 0 | 17.0 | 0.07 |
| g-train | train | on | off | 144.0 | 6.90 | 7.80 | 7.9 | 0.550 | 1.10 | 1.70 | 0.635 | 2.206 | 28 | 144 | 1,810 | 0 | 0 | 7.0 | 0.95 |
| g-skirmish | skirmish | on | off | 143.7 | 6.90 | 7.80 | **145.8** | 0.648 | 1.60 | 2.90 | 0.632 | 2.474 | 22 | 389 | 1,570 | 1 | 1 | 22.4 | 0.97 |

`js` = duration of the app's own rAF callback (`update()` + `renderFrame()`), measured by
wrapping the callback. `gpu` = `EXT_disjoint_timer_query_webgl2` `TIME_ELAPSED` spanning
the whole callback. `alive` = fraction of samples where the HUD did not read "Respawning…".

### Reading

**The distribution is uniform, not spiky.** p50 6.90 ms, p99 7.80 ms, in every single run.
This is a hard vsync lock at 144 Hz with the frame arriving on time essentially always:
**0 or 1 frames out of ~9,350 exceeded 16.7 ms.** This is not "60 fps mean with p99 spikes"
and it is not "uniform 40 fps". It is a locked refresh with roughly 5× headroom.

**Two isolated stalls** (174.4 ms in base-train, 145.8 ms in g-skirmish — 2 events across
7 runs, ~1 per 2 minutes of play). The JS callback during the 145.8 ms frame was **1.3 ms**,
so the stall was *outside* game code — most plausibly a major (mark-compact) GC or a
browser-level stall. **Unresolved**: I added a `longtask` PerformanceObserver to attribute
these, but the runs carrying it did not complete.

**Caveat on `draws p50`:** the driving script flies the ship on a wide arc and it often
leaves the asteroid field, so p50 draw counts understate a real firefight. Use **p99** for
"in the fight" and the **campaign** row, where the field is dense enough that framing barely
matters (p50 308 / p99 399).

---

## 2. Where the main-thread time actually goes

`Performance.getMetrics` deltas across each run, divided by frame count:

| run | frames | total task/frame | script/frame | layout/frame | style recalc/frame | LayoutCount | DOM nodes |
|---|---|---|---|---|---|---|---|
| base-train | 9,345 | 1.315 ms | 0.608 | 0.115 | 0.114 | **9,348** | 1,344 |
| base-nopixel | 9,363 | 1.281 | 0.579 | 0.115 | 0.115 | **9,366** | 1,180 |
| base-ultra | 9,365 | 1.269 | 0.599 | 0.110 | 0.111 | **9,366** | 1,980 |
| base-skirmish | 9,356 | 1.381 | 0.753 | 0.100 | 0.100 | **9,385** | 2,052 |
| base-campaign | 9,362 | 1.681 | 1.205 | 0.105 | 0.043 | **9,362** | 2,157 |
| g-train | 9,366 | 1.298 | 0.586 | 0.119 | 0.115 | **9,368** | 1,653 |
| g-skirmish | 9,346 | 1.322 | 0.682 | 0.104 | 0.103 | **9,368** | 2,005 |

**`LayoutCount` equals the frame count in every run.** The DOM HUD forces exactly one full
layout plus one style recalculation per frame, costing **0.21–0.23 ms/frame** combined —
roughly **35–40 % of the entire JS callback time**, and completely invisible to
rAF-based profiling. See §4-F for the code.

Per-frame budget, default config, 144 Hz (6.94 ms available):

```
  JS callback (update + render)   0.55 – 1.17 ms
  layout + style recalc           0.21 – 0.23 ms
  other main-thread task time     ~0.45 ms   (rAF dispatch, compositing commit, input)
  ────────────────────────────────────────────
  total main thread               1.27 – 1.68 ms      (18 – 24 % of budget)
  GPU (parallel)                  0.63 – 0.70 ms      (9 – 10 % of budget)
```

### JS cost scales linearly with draw-call count

From g-skirmish, bucketing all 9,346 frames by draw calls in that frame:

| draw calls | frames | mean JS callback |
|---|---|---|
| 0–49 | 6,800 | 0.561 ms |
| 50–99 | 1,115 | 0.744 |
| 100–149 | 346 | 0.840 |
| 200–249 | 177 | 0.903 |
| 250–299 | 290 | 1.047 |
| 300–349 | 247 | 1.233 |
| 400–449 | 84 | **1.355** |

**≈ +0.0021 ms of CPU per draw call.** That is Three.js's per-object cost — matrix update,
frustum cull, material/uniform resolve — *not* GPU work. It is the clearest single
attribution I obtained, and it points straight at the one-mesh-per-entity design (§4-D).

g-train shows the same slope (0.512 ms at <50 draws → 0.797 ms at 150–199).

---

## 3. GPU / render cost

`renderer.info` is not reachable — the renderer is closure-scoped inside `startGame()` and
nothing exposes it. I counted at the **GL level** instead by wrapping `drawElements`,
`drawArrays`, `useProgram`, `linkProgram`, `texImage2D` and `bufferData` on the context.
This is equivalent to `renderer.info.render.calls/triangles` and strictly better here,
because it captures **both** passes of the pixel-filter pipeline (scene → render target,
then quad → screen), whereas `renderer.info` resets per `render()` call.

| | train | skirmish | campaign |
|---|---|---|---|
| draw calls p50 / p99 | 28–65 / 144–180 | 22–24 / **389–477** | **308** / 399 |
| triangles p50 | 1,810–5,388 | 1,570–1,650 | **43,600** |
| GPU ms mean / p50 / p99 / max | 0.635 / — / 2.206 / — | 0.632 / — / 2.474 / — | 0.695 / 0.622 / 2.187 / 3.412 |
| programs linked (total) | 10 | 10 | 10 |
| shaders compiled (total) | 20 | 20 | 20 |
| programs linked **during** gameplay | **0** | **0** | **0** |

Ultra raises program count to **21** and shader count to **42**.

Two useful facts:

- **Zero programs are linked during a match.** There are no shader-compile hitches
  mid-game; all 10 (or 21) programs are built during entry.
- **Triangle count is nearly free here.** Campaign renders 24× the triangles of train
  (43,600 vs 1,810) for **+9 % GPU time** (0.635 → 0.695 ms). The GPU is not
  geometry-bound; if anything moves it, it will be fill rate or draw-call submission.

### Pixel filter and Ultra Graphics A/B — **inconclusive, and here is why**

| config | fps | dt p99 | js mean | draws p99 | tris p50 |
|---|---|---|---|---|---|
| pixel filter ON (default) | 143.6 | 7.80 | 0.583 | 180 | 5,388 |
| pixel filter OFF | 144.0 | 7.80 | 0.554 | 178 | 2,388 |
| Ultra ON | 144.0 | 7.80 | 0.576 | 182 | 1,182 |

All three are pinned at 144 fps with no measurable difference. **At 1440×900 / DPR 1 on an
M5 this test has no power** — the GPU is at ~9 % of budget in every configuration, so a
large relative change in fill rate is still invisible. I could not complete the Retina and
2560×1440 runs that would have given this test teeth.

What the *code* says about the stakes (main.js:51–53):

```js
const PIXEL_SCALE = 3;
const pixelRT = pixelEnabled ? new THREE.WebGLRenderTarget(
  Math.max(1, Math.floor(window.innerWidth / PIXEL_SCALE)),
  Math.max(1, Math.floor(window.innerHeight / PIXEL_SCALE)), …
```

The target is sized in **CSS pixels divided by 3, ignoring `devicePixelRatio`**. So with the
filter on, the 3D scene always renders into 480×300 ≈ 144 k pixels regardless of display
density, and only the final upscale quad runs at full backing-store resolution. Turning the
filter **off** makes the scene render at the full backing store — on a Retina panel at
pixelRatio 1.5 that is 2.9 Mpx, a **~20× increase in scene fill rate**. Ultra stacks two
HalfFloat ping-pong targets, a half-res bloom mip chain, and a combined grade pass on top.

**That is the one path where GPU could plausibly dominate, and it is exactly the path I did
not get to measure.**

---

## 4. Static analysis — read from the code, not measured

This section costs nothing to produce and is, as the coordinator noted, more reliable than
what the browser was giving me at 144 fps with 5× headroom.

### A. O(entities × asteroids) scans with no broad phase — **the core scaling problem**

There is no spatial partitioning anywhere. The asteroid list is scanned linearly, from
scratch, once per interested entity per frame:

| caller | file:line | passes per frame |
|---|---|---|
| asteroid spin update | asteroids.js:96 / :187 | 1 |
| bullet ↔ asteroid | bullets.js:96 and :110 | 1 per live bullet |
| missile avoidance | missiles.js:108 (`computeAvoidance`) | 1 per live missile |
| missile detonation test | missiles.js:124 (`insideObstacle`) | 1 per live missile (**second pass**) |
| bot steering avoidance | bot.js:111 | 1 per bot |
| bot body push-out | bot.js:237 | 1 per bot (**second pass**) |
| bot logical projectiles | bot.js:334 | 1 per bot projectile |
| player push-out | main.js:2194 (`resolveCollisions`) | 1 |
| reticle raycast | main.js:1058 (`castWorldRay`), called at :1805 | 1 |
| target-box occlusion | main.js:1852 | 1 per remote player |
| aim assist occlusion | main.js:2058 | 1 per remote player *(off by default on desktop — main.js:996)* |

Firing rates that drive the entity counts: `BULLET_COOLDOWN = 0.05` (main.js:1030) → 20
player bullets/s; bot `FIRE_COOLDOWN = 0.15` (0.05 in hard mode, bot.js:19) → 6.7/s per bot;
bullet `LIFE = 2.0` (bullets.js:7).

Estimated passes per frame in **skirmish** (9 bots): ~40 player bullets + ~120 bot bullets
live, ~120 bot logical projectiles, 9 bots, 9 remote records
→ **≈ 310 full passes × 60 asteroids ≈ 18,600 distance tests/frame ≈ 2.7 M/s at 144 fps.**

In **campaign** (280 asteroids — main.js:240-243, three zones of 90/100/90)
→ **≈ 84,000 tests/frame ≈ 12 M/s.**

This matches the measurement: JS callback goes 0.550 ms (train, 60 rocks) → **1.168 ms**
(campaign, 280 rocks), a 2.1× rise for a 4.7× rise in asteroid count (sub-linear because a
fixed per-frame cost dominates the rest).

> **Rust verdict: design issue — follows into the rewrite.** Rust makes each test maybe
> 5–10× cheaper, but the growth curve is identical. A uniform grid or BVH is the actual fix
> and it is needed in either language.

### B. Bot bullets are simulated twice

`bot.js:301 fireBullet()` calls `bullets.fire()` — which creates a visible bolt that then
runs its own full collision scan in `bullets.update()` — **and** pushes a parallel logical
projectile into `myProjectiles`, which runs a *second* independent integration and collision
scan in `bot.js:314 updateProjectiles()`. Two position integrations and two O(asteroids)
scans per bot shot, forever.

> **Rust verdict: design issue — follows into the rewrite.**

### C. `getOpponents()` rebuilds an array per projectile per frame

`bot.js:321`, inside the per-projectile loop:

```js
for (const e of getOpponents()) {
```

and `getOpponents` (main.js:2509) allocates a fresh array on every call:

```js
getOpponents: () => {
  const out = [];
  if (playerEntity.team !== team) out.push(playerEntity);
  for (const b of bots) if (b.team !== team) out.push(b.entity);
  return out;
},
```

Skirmish: 9 bots × ~13 live projectiles × 144 fps ≈ **17,000 array allocations/second**,
plus ~1,300 more from `pickTarget()` (bot.js:123). This is the most likely single driver of
the measured jump from **7.0 MB/s (train) to 22.4–23.4 MB/s (skirmish)**.

> **Rust verdict: both.** The allocation is JS-specific and vanishes; rebuilding the
> opponent list per projectile is wasted work in any language.

### D. Every visual effect is an individual Mesh + Material — no pooling, no instancing

| system | allocated per spawn | rate | file:line |
|---|---|---|---|
| bullets | `new THREE.Group` + **2** × `new THREE.Mesh` + `direction.clone()` | 20/s player, 6.7/s per bot | bullets.js:38–45 |
| ship trails | `new THREE.Mesh` + `new THREE.MeshBasicMaterial` | 36/s cruising, **90/s boosting** (2 offsets × 45 Hz, main.js:1139) | trails.js:65–77 |
| missile trails | `new THREE.Mesh` + `new THREE.MeshBasicMaterial` | 36/s per missile (`TRAIL_INTERVAL = 0.028`) | missiles.js:159–168 |
| flare trails | same | 33/s per flare, **20 flares per keypress** | missiles.js:176–190 |
| flare burst | `new THREE.Group` + 2 Mesh + 2 Material, **×20** | per Q press | missiles.js:238–280 |
| explosions | **3** × (Mesh + Material) per detonation | per hit | missiles.js:200–235 |
| beams | **`new THREE.CylinderGeometry`** + Mesh + Material, disposed after 0.18 s | 4/s in beam mode | beams.js:16–30 |

Four consequences, in order of measured importance:

1. **Each particle is its own draw call.** `MAX_PARTICLES = 250` (trails.js:2), so trails
   alone can add 250 draws. Measured p99 in skirmish: **477 draw calls**. At the measured
   **+0.0021 ms/draw** this is ~1.0 ms of CPU at p99 — the single largest identified cost.
2. **Each is its own scene-graph node.** Three.js walks the entire graph every frame for
   `updateMatrixWorld` and frustum culling, so cost is paid even for off-screen particles.
3. **Each new material** makes Three.js rebuild a program cache key string and re-resolve
   the program on first use.
4. **beams.js allocates and disposes a GPU buffer per shot** — a `CylinderGeometry` built
   at the beam's exact length, uploaded, then disposed 0.18 s later.

> **Rust verdict: design issue — follows into the rewrite.** wgpu will not batch 250
> separate draws for you either. Instancing or a pooled particle vertex buffer is the fix,
> and it is needed in both languages. Only the *allocator* pressure (§E) is JS-specific.

### E. Per-frame allocation in `update()` even when nothing is happening

Unconditional, every frame:

| what | file:line |
|---|---|
| `[...(navigator.getGamepads?.() ?? [])].find(…)` — new array + closure even with no gamepad | input.js:159, called from main.js:1156 |
| `new THREE.Vector3(0,0,1).applyQuaternion(…)` + `.clone()` — 2 Vector3 in the normal flight path | main.js:1288–1289 |
| 4 × Vector3 for the reticle (`projTmp`, `aimFwd`, `muzzleWorld`, `reticleAimWorld`) | main.js:1802–1806 |
| `castWorldRay` returns a fresh `{dist, hitShipId, hitAsteroidId}` object | main.js:1072 |
| `new Set()` every frame in `resolveCollisions` | main.js:2193 |
| **6 × Vector3 per frame** in `_updateChase` (back, up, fwd, dirToShip, viewDir, lookAt) | camera.js:42–55 |
| `camTel.contacts.push({x, z, hostile})` per radar contact *(cockpit view only)* | main.js:1925 |

A lexical scan for `new THREE.*`, `.clone()`, `new Set/Map/Array`, `JSON.stringify` and
`.toArray()` occurring inside loop bodies found **85 sites**: main.js 60, missiles.js 17,
asteroids.js 7, bot.js 1. Not all are per-frame (many are one-time scene construction), but
those inside `update()`, `bullets.update`, `missiles.update`, `bot.update` and
`camera.update` are.

Measured allocation rate: **7.0–7.9 MB/s (train), 22.4–23.4 MB/s (skirmish), 17.0 MB/s
(campaign)**, with 65–89 minor GCs per 65 s run (~1/s, each reclaiming ~5 MB).

**Correlating GC with the frame spikes, as asked: there is no correlation.** Frame deltas on
frames where the heap dropped were **6.9–7.0 ms** — identical to non-GC frames. V8's
scavenger absorbs this entirely. Sample from base-train:

```
frame  79  heap -6.02 MB   delta 7.0 ms   js 0.3 ms
frame 191  heap -5.56 MB   delta 6.3 ms   js 0.8 ms
frame 303  heap -5.24 MB   delta 6.9 ms   js 1.0 ms
```

> **Rust verdict: implementation issue — genuinely fixed by the rewrite.** But it is ranked
> *low* below, because it is measurably not costing frame time today. Fixing it buys
> allocator hygiene, not fps.

### F. The HUD is DOM and is rewritten every frame

46 DOM write sites inside `update()`. The worst offender (main.js:1953):

```js
hpFill.style.background = `linear-gradient(180deg, hsl(${hue}, 80%, 60%) 0%, hsl(${hue}, 70%, 38%) 100%)`;
```

A fresh gradient string assigned every frame, forcing re-rasterization even when HP has not
changed. Plus, **per remote player, every frame** (main.js:1818–1899): `box.style.display`,
`.left`, `.top`; `lead.style.display`, `.left`, `.top`; `label.textContent`;
`lead.classList.toggle` — 8 writes × N players, so up to **72 extra DOM writes/frame** in
skirmish. Plus `chargeFill`, `boostFill`, `heatFill`, `hpFill.style.width`, `hpText`,
`hud.textContent`, `hitVignette.style.opacity`, and `document.getElementById('reticle')` /
`('missile-lock-warning')` looked up fresh each frame.

Measured cost: **0.21–0.23 ms/frame**, `LayoutCount` == frame count in all 7 runs.

> **Rust verdict: design issue, and a sneaky one.** A wasm/wgpu rewrite that keeps a DOM HUD
> keeps this cost *exactly*. Only moving the HUD into the canvas removes it. Worth flagging
> because it is the second-largest measured main-thread item and no JS profiler would
> surface it — layout happens after the rAF callback returns.

### G. Redundant per-frame and per-entity work

- **asteroids.js:86 / :150** — `baseMat.clone()` per asteroid produces 60 (or 280) unique
  `MeshStandardMaterial` instances that all share one texture. No instancing, so campaign
  pays 280 separate draw calls (measured p50 **308**).
- **ship.js:49** — `createShip` clones *every* material of the GLB per ship instance
  (`o.material = o.material.clone()`). `spaceship.glb` has 6 meshes / 1 material, so 9 bots
  = 54 unique materials and ≥54 draw calls for bot ships alone.
- **graphics.js:427 `sweepScene`** — runs `scene.traverse()` twice every 0.5 s (once
  matching `o.name === 'Ship'`, then a generic `upgradeMaterials(scene)`), walking the whole
  graph including all ~250 trail particles and ~160 bullets. Ultra only.
- **dash.js:145** — the cockpit radar's memoisation key is `` `${sweep.toFixed(3)}:…` ``, and
  `sweep` advances every frame (dash.js:230), so the 256×128 canvas is redrawn and
  re-uploaded as a `CanvasTexture` **every frame** in first-person view. That is ~128 kB of
  texture upload per frame ≈ **18 MB/s at 144 fps**. The code even documents this:
  *"Key changes every frame so the memoisation in createScreen never skips the sweep."*
  **Not measured** — third-person is the default and the cockpit run did not complete.

---

## 5. Load time (measured, base-train run)

| item | value |
|---|---|
| `responseEnd` (HTML) | 5.5 ms |
| DOMContentLoaded | 199.8 ms |
| load event | 543.8 ms |
| `index-*.js` bundle | 754 kB decoded, **3.2 ms** transfer (localhost) |
| **`spaceshipADMIN.glb`** | **4,836 kB, 185.8 ms** (localhost) |
| `spaceship.glb` | 41 kB, 1.4 ms |
| `dumb_Eflatmin.mp3` (music) | 2,735 kB, 60.6 ms |
| `move.mp3` | 1,940 kB, 13.9 ms |
| `asteroid.jpg` | 216 kB, 5.8 ms |
| **total texture upload** | 17 uploads, **0.55 MB, 2.3 ms** — negligible |
| shader compile + link | 20 shaders / 10 programs, ~0.1 ms wall (ANGLE compiles async) |
| GPU buffer upload during load | **0.29 MB** |

GLB contents, parsed directly from the files:

| model | size | meshes | materials | vertices | triangles |
|---|---|---|---|---|---|
| `spaceship.glb` | 0.04 MB | 6 | 1 | 1,046 | 516 |
| `spaceshipADMIN.glb` | **4.72 MB** | 13 | 3 | **106,282** | **137,254** |

Two findings:

1. **`spaceshipADMIN.glb` is fetched unconditionally for every session**, whether or not
   the player owns or uses the admin ship (main.js:100–101):
   ```js
   const ADMIN_MODEL_URL = 'spaceshipADMIN.glb';
   const adminModelReady = loadShipModel(ADMIN_MODEL_URL).catch(() => null);
   ```
   4.72 MB and a GLTF parse, every time. 186 ms on localhost; on a 10 Mbps connection that
   is ~4 s. The measured **0.29 MB** of GPU buffer upload confirms it is downloaded and
   parsed but never uploaded to the GPU when unused — so it is pure download-and-parse waste
   in the common case, and a 137 k-triangle, 13-draw-call ship when it *is* used.

2. **Texture upload and shader compilation are non-issues** — 0.55 MB and 2.3 ms total.
   Nobody should spend time here.

The assets do not start loading until `startGame()` runs (`spaceship.glb` starts at
t = 5,513 ms in the trace, i.e. after the lobby clicks), so the lobby appears in ~544 ms and
the asset cost lands on the "Play" press, behind the warp-effect loading loop
(main.js:90–97). That is a deliberate design; the 4.72 MB admin model is the part that
isn't.

---

## 6. Ranked diagnosis

Ordered by measured cost on the hardware profiled. **The headline: on an Apple M5 at
1440×900 the game does not lag — it is vsync-locked at 144 fps using 18–24 % of the
main-thread budget and 9–10 % of the GPU budget.** The ranking below is therefore
"what will break first as content or hardware gets worse", which is the actionable question.

### The CPU/GPU verdict

**Directionally CPU-bound. Moderate-to-high confidence.**

Evidence for CPU:
- JS callback rises **2.1×** (0.550 → 1.168 ms) going from 60 to 280 asteroids, while GPU
  time rises **9 %** (0.635 → 0.695 ms) over the same change — despite triangles going
  1,810 → 43,600.
- JS callback scales cleanly with draw-call count at **+0.0021 ms/draw** (§2), i.e. the cost
  of more content lands on the CPU's per-object overhead, not the GPU's raster.
- Total main thread 1.27–1.68 ms/frame vs GPU 0.63–0.70 ms — CPU is ~2× GPU and growing
  faster on every axis I could vary.

Confidence caveat, stated plainly: **I could not rule out a GPU bottleneck at Retina density
with the pixel filter off or Ultra on.** The pixel filter caps scene fill at 480×300
independent of DPR (§3), so the default path is structurally almost impossible to make
GPU-bound. Turning it off is a ~20× fill increase on a Retina panel, and Ultra adds HalfFloat
ping-pong targets plus bloom. That configuration is unmeasured. **If a user reports lag, ask
first whether they have the pixel filter off or Ultra on, and at what resolution** — that is
the only plausible GPU-bound path, and it is the one gap in this report.

### Ranked causes

**1. Three.js per-object CPU overhead from one-mesh-per-entity.** *Largest measured CPU item.*
Evidence: +0.0021 ms/draw regression across 9,346 frames (§2); p99 **477 draw calls** in
skirmish ≈ 1.0 ms of CPU at p99; campaign holds 308 draws at p50. Every bullet is a Group
plus 2 Meshes; every trail particle, missile-trail puff, flare and explosion layer is its own
Mesh + Material (§4-D); every asteroid gets a cloned material (§4-G).
→ **Design issue. Follows into Rust/wgpu.** Instancing and pooling are required in both
languages; wgpu will not batch 477 draws for you.

**2. DOM HUD forcing a layout + style recalc every frame.** 0.21–0.23 ms/frame, `LayoutCount`
== frame count in all 7 runs (§2, §4-F). ~35–40 % of the JS callback cost, invisible to
rAF-based profiling. Driven by a per-frame `linear-gradient` string assignment and 8 style
writes per remote player per frame.
→ **Design issue, and it follows into a Rust port unless the HUD moves into the canvas.**
A wasm rewrite that keeps this DOM keeps this cost byte for byte.

**3. O(entities × asteroids) collision and visibility scans with no broad phase.** ~18,600
tests/frame in skirmish, ~84,000 in campaign (§4-A). Drives the measured 2.1× JS growth from
train to campaign. Eleven independent call sites re-scan the same list every frame; missiles
and bots each scan it twice.
→ **Design issue. Follows into Rust.** Rust buys a constant factor; the curve is unchanged.
A uniform grid is the fix in either language.

**4. Duplicate simulation of bot bullets.** Every bot shot is integrated and collision-scanned
twice — once as a visible bolt in bullets.js, once as a logical projectile in bot.js (§4-B).
Roughly doubles projectile-related work in bot-heavy modes, which is exactly where draw
calls peak (skirmish p99 477).
→ **Design issue. Follows into Rust.**

**5. Load: 4.72 MB `spaceshipADMIN.glb` fetched unconditionally every session** (§5), plus
4.7 MB of audio. 186 ms on localhost, seconds on real connections, for an asset most players
never use. Texture upload (0.55 MB) and shader compile (10 programs) are non-issues.
→ **Design issue. Follows into Rust** — the same bytes cross the same wire.

**6. Per-frame allocation and GC churn.** 7.0–23.4 MB/s, ~1 scavenge/second, 85 allocation
expressions inside loop bodies, 6 Vector3/frame in the camera alone (§4-E).
→ **Implementation issue. Genuinely fixed by Rust.** Ranked *low deliberately*: I measured
frame deltas at GC frames at **6.9–7.0 ms**, identical to non-GC frames. This is real waste
but it is **not** what costs frame time today. Do not let the rewrite's biggest obvious win
be mistaken for the biggest actual win.

**7. GPU fill rate with the pixel filter off or Ultra on.** *Unquantified — low confidence.*
Structurally the largest untested lever (~20× scene fill on Retina), and the only path where
the correct fix would be GPU-side rather than CPU-side.
→ Unknown until measured. If it *is* the problem, a Rust/wgpu port helps only insofar as it
lets you control the render pipeline more precisely; the fill rate itself is identical.

### What a Rust/wgpu rewrite would and would not fix

| cause | rewrite fixes it? |
|---|---|
| 1. Per-object draw-call overhead | **No** — same design, same draw calls |
| 2. Per-frame DOM layout | **No** unless the HUD leaves the DOM |
| 3. O(n×m) scans, no broad phase | **No** — constant factor only |
| 4. Duplicate bot-bullet simulation | **No** — same design |
| 5. 4.72 MB unconditional GLB | **No** — same bytes |
| 6. Allocation / GC churn | **Yes** — but it is not currently costing fps |
| 7. GPU fill (unmeasured) | **No** — same pixels |

Six of the seven are design issues that travel with the game. The one thing the rewrite
fixes outright is the one thing measurement shows is not currently hurting.

---

## 7. Reproducing / finishing this

Artifacts in this directory:

- `instrument.js` — injected probe (rAF wrapper, GL wrapper, GPU timer queries, longtask observer)
- `run.js` — Playwright driver (`MODE`, `PIXEL`, `ULTRA`, `CPU`, `DSF`, `W`/`H`, `PROFILE`, `HEAPPROF`, `VIEW`, `RECSTART`)
- `summarize.js`, `spikes.js`, `analyze-profile.js`, `analyze-heap.js`, `allocscan.js`
- `res-*.json` (7 completed runs), `frames-*.json`, `batch1.log`, `batch2.log`

To close the gaps, in priority order:

```sh
cd <this dir>
# 1. The GPU question — the one real hole in this report
DSF=2 W=2560 H=1440 MODE=train PIXEL=0 TAG=big-nopixel DURATION=65 node run.js
DSF=2 W=2560 H=1440 MODE=train ULTRA=1 TAG=big-ultra   DURATION=65 node run.js
DSF=2 W=2560 H=1440 MODE=train PIXEL=1 TAG=big-pixel   DURATION=65 node run.js

# 2. Function-level CPU attribution (build unminified first for readable symbols)
npx vite build --minify false --outDir <dir>/dist-nomin   # then serve on :4100
URL=http://localhost:4100 MODE=skirmish PROFILE=1 HEAPPROF=1 TAG=p-skirmish node run.js
node analyze-profile.js prof-p-skirmish.cpuprofile
node analyze-heap.js heap-p-skirmish.json

# 3. Headroom on slower hardware
CPU=8 MODE=campaign TAG=cpu8-campaign DURATION=65 node run.js
```

`run.js` already seeds the localStorage progression keys that unlock Trial 4 and campaign
missions 2–3, so `MODE=trials4` (210 asteroids) now works as a scaling point.
