# First-Person Cockpit — Implementation Plan

Goal: a real first-person mode with head turning, a live instrument dash, and a full
interior — working on both `spaceship.glb` and `spaceshipADMIN.glb`.

## Decisions

**Head control — hybrid.** Ship heading is never changed by head movement.

| Input | Behavior |
|---|---|
| Mouse | Steers ship (existing scheme, unchanged) |
| Steer input | Head auto-leans into the turn (~12°, subtle) |
| Hold RMB | Free head-look, clamped ~110° yaw / ~70° pitch |
| Release RMB | Damped return to boresight |
| Gamepad right stick | Same free-look |
| Hold Alt | Look back over shoulder |

**Interior — shared kit + per-ship profiles.** One geometry kit (canopy ribs, seat,
harness, floor pan, side consoles, overhead panel, animated stick + throttle lever),
two tuned profiles.

## Measured facts

Both GLBs face **+X**. `ship.js:42` applies `rotation.y = -π/2`, mapping model
`(x,y,z)` → ship `(-z, y, x)`, which is why the codebase treats `(0,0,1)` as forward.
`ship.scale = 1.5` (`SHIP_SCALE`), so author interior geometry in **ship-local units**.

| | world size (X×Y×Z) | canopy node | eye anchor (ship-local, pre-scale) |
|---|---|---|---|
| `spaceship.glb` | 5.62 × 1.74 × 5.43 | `Cockpit` (Icosphere) | `(0, 0.55, 1.5)` |
| `spaceshipADMIN.glb` | 10.09 × 2.60 × 6.75 | `Cylinder.002` (`glass` mat) | `(0, 0.75, 3.9)` |

Neither model contains interior geometry. Both "cockpits" are exterior canopy shells.

## Known landmines

1. **`main.js:175` deletes ship children.** When the 4.7 MB admin GLB lands late it runs
   `ship.children.slice().forEach(c => ship.remove(c))` to swap models — this silently
   destroys an attached cockpit. Re-attach inside that `.then()`, or tag and skip.
2. **Admin ship is `DoubleSide`** (`main.js:170` → `ship.js:47`). Hull renders opaque
   from inside and blocks the entire view. Swap to `FrontSide` in FP, restore on exit.
3. **Regular ship is `FrontSide`** — backface-culled, so you see out, but also through
   the floor. Interior must be a closed shell either way.
4. **Customization repaints the interior.** `isAccentMesh` (`ship.js:31-36`) matches any
   name containing `cockpit`/`glass`/`window` *and* any material under 0.35 luma — dark
   interior panels qualify. Needs a `userData.isInterior` bail-out in both
   `applyColorsToShip` and `createShip`'s traverse.
5. **Invuln strobes the cockpit.** `main.js:1804` sets `ship.visible` per frame on
   respawn. Must flicker exterior meshes only in FP.
6. **Pixel filter is on by default** (`main.js:34`, `PIXEL_SCALE = 3`). Dash renders at
   1/3 res — design it chunky and high-contrast.
7. **`warp.js:38-60` mutates `camera.fov`**, capturing `baseFov` on start and restoring
   on finish. A warp spanning a mode switch restores the wrong FOV. Single owner for FOV.
8. **Camera roll.** `ThirdPersonCamera` uses `lookAt` + lerped `camera.up`. FP must copy
   `ship.quaternion` composed with the head quaternion, or rolls look wrong.
9. **`consumeMouseDelta()` is destructive** (`input.js:142`) — FP must own it in FP mode.
10. `camera.near = 0.1` (`main.js:33`) — keep dash panels clear of it.
11. `tpCam.snap()` has two call sites: `main.js:384` and `:657` (`reviveSelf`).
12. `main.js:200` sets `castShadow` on all ship meshes on terrain maps — exclude interior.

## Landmines found DURING the build (not predicted)

13. **`TP_FOV` captured the warp's max, not the base.** `createWarpEffect` (`main.js:67`)
    sets `camera.fov = 175` at construction, so capturing `camera.fov` after it yields 175.
    Third person then re-asserted 175 every frame and the intro warp never visually ended.
    Fixed by capturing `BASE_FOV` immediately after the camera is constructed.
14. **The pilot's right is `-X`.** Forward `+Z` and up `+Y` in a right-handed frame means
    `right = forward × up = -X`. Every console, stick and lamp placement was mirrored until
    this was accounted for.
15. **`lookAt` mirrors canvas textures.** Orienting a panel at the eye rotates it ~180° about
    Y, so texture-space left renders on screen-left while a world-space sibling at the same
    sign renders on screen-right. The lamps and their labels disagreed until the lamp X was
    flipped.
16. **Displays must be laid out from the width BETWEEN the consoles**, not from `HW` — the
    outer screens were being swallowed inside the console boxes.
17. **Playwright taps are sub-frame.** The game polls `input.keys` in its update loop, so
    `keyboard.press()` is missed entirely; the harness holds each key ~150 ms.

## Phases

- [x] **0 — Harness.** `npm install`, server on **:4000**, Playwright driving guest →
      Single Player → Time Trials (no combat, so the ship stays alive). Admin via
      `localStorage['spaceships:unlock_admin_ship'] = '1'`. Existing Chromium at
      `ms-playwright/chromium-1217` avoids a browser download.
- [x] **1 — Camera core.** `fpcamera.js`, same `snap()` / `update()` surface, drops into all
      four call sites. `V` toggles, persisted to `spaceships:viewMode`.
- [x] **2 — Visibility.** Exterior hull culled in first person (replaces the per-material
      `FrontSide` swap — simpler and immune to the `DoubleSide` problem), interior preserved
      across the admin late-swap, invuln flicker and customization repaint both fixed.
      Dying falls back to third person so you watch your own wreck.
- [x] **3 — Interior.** Shared kit driven by the profile `tub`: floor, walls, bulkhead, roof,
      side consoles with glowing accent strips, canopy A-pillars and rails, ejection seat with
      harness, animated side-stick and throttle lever.
- [x] **4 — Dash.** Three canvas displays (speed/throttle/hull, weapons, boost/charge) plus
      **TGT LOCK** and **MSL WARN** annunciators. DOM meters hidden via `body.cockpit-view`.
- [x] **5 — Head feel.** Clamped free-look (110°/70°), damped recenter, auto-lean into turns,
      Alt look-back, boost rumble + damage kick reusing the existing vignette envelope.
      Gamepad free-look on R3 (the right stick already steers the ship, so it needs a modifier).
- [x] **6 — Polish.** Controls guide entries, "Start in Cockpit" setting, terrain map verified,
      third-person regression verified at FOV 75.

## Still open

- Mobile/touch has no free-look affordance (no RMB). `V` works; head-look does not.
- `window.__fpDebug()` / `window.__fpForce` are dev hooks left in `main.js` — remove if unwanted.
- Pre-existing, unrelated: in "Train with Robot" the player is destroyed within seconds of
  spawn with no input. Present on the untouched baseline too.

## New files

    public/src/fpcamera.js   camera + head model
    public/src/cockpit.js    interior geometry + per-ship profiles
    public/src/dash.js       live instruments

Touched: `main.js`, `ship.js`, `input.js`, `index.html`, `lobby.js`.
