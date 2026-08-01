# Backlog

Ideas parked until the Rust rewrite lands. Nothing here should start before the
Bevy client reaches parity with the Three.js version — with one flagged
exception in the replay section.

---

## 1. Replay system

The headline feature. Record a match, scrub it, fly through it freely, cut
clips.

### Why this is cheap

`crates/sim` is deterministic by construction: same seed plus same inputs
produces bit-identical output, pinned by tests
(`asteroids::generation_sequence_is_pinned`, `worlds_from_the_same_seed_are_identical`).

So a replay is **not** a recording of positions. It is:

```
seed + rules snapshot + [(tick, player, Input)]
```

Re-run the sim and the exact match happens again. A 5-minute match at 60 Hz
with 8 players is on the order of tens of KB, versus hundreds of MB for
recorded state. It also means a replay stays correct if you re-render it at a
higher graphics setting.

The camera was never part of the simulation, so every viewing feature below is
a rendering concern over a re-simulated world — no extra recording required.

### Features

- **Timeline scrub.** Seek to any tick. Requires periodic `World` keyframes
  (say every 10 s) so seeking doesn't re-simulate from tick 0 — snapshot, then
  fast-forward the remainder.
- **Free camera.** Detach from the ship and fly anywhere. Trivial, since the
  camera is not simulation state.
- **Click a player to switch views.** Third-person, first-person cockpit, or
  chase. All existing camera code already supports it — it just needs a target
  that isn't "me".
- **Slow motion.** Decouple render rate from tick rate. The sim runs at a fixed
  timestep, so 0.25× is "render 4 frames per tick with interpolation," not a
  physics change. Interpolating between ticks is what makes it look smooth
  rather than stepped.
- **Keyframed camera paths.** Set camera keyframes along the timeline, spline
  between them, scrub to preview. This is the clip-production feature.
- **Export.** Offscreen render to a video file at a fixed step, so output is
  smooth regardless of what the machine can do in real time.

### Do this bit *during* the rewrite, not after

Everything above is post-rewrite work except one thing: make the **input log a
first-class part of the tick design**. `tick(&mut World, &[Input], &[NetEvent], dt)`
already takes inputs as an explicit slice, so recording them is nearly free
now. Retrofitting it later means touching every system. Cheap insurance —
record the log even if nothing reads it yet.

### Open questions

- Rules change between versions. A replay must store the `Rules` it ran under,
  or old replays desync after a balance patch. Version the format from day one.
- Do replays record multiplayer matches server-side, or does each client record
  its own? Server-side is authoritative and enables sharing.

---

## 2. Falls out of the replay system nearly free

- **Killcam.** A replay bounded to the 5 seconds before your death, from the
  killer's view.
- **Spectator mode.** A live replay with zero seek offset. Same camera code.
- **Trials ghosts.** Trials already track best times per mission
  (`spaceships:trial4Best` etc.). A ghost is a stored input log replayed
  alongside you. Race yourself.
- **Match history.** Store replays server-side, browse past matches from the
  profile screen.

---

## 3. Netcode: rollback

A deterministic simulation is the hard prerequisite for rollback netcode — the
thing that makes fighting games feel lagless online. The client predicts
forward, and when a late input arrives it rewinds and re-simulates. Only
possible because re-simulation is exact.

Worth doing if the game ever has real concurrent players. Large job; the
foundation is already in place.

---

## 4. Known gameplay issues found during the port

Real behaviors in the current game, each verified against the source. Some are
bugs, some are probably-unintended design. Decide individually.

| Issue | Where | Note |
|---|---|---|
| Aim assist over-leads | `main.js:2047` | Passes `shipVelocity` as shooter velocity, but `bullets.js:44` gives bolts no velocity inheritance. Error grows with your speed. `bot.js:172` does it correctly — the AI aims better than your assist. |
| Lock-on has no cone and no range | `main.js:1629`+ | Nearest living enemy with line of sight, full stop. You can lock a target directly behind you. There is no range cap even though a missile only reaches ~1280 units. |
| Missiles do not damage asteroids | `missiles.js` | Only bullets do. Contradicts the `asteroid_damage_per_hit` docs. |
| Your own flares detonate your own missiles | `missiles.js` | Seduction skips own-owner flares; proximity detonation doesn't. |
| Missiles have no body radius | `missiles.js:402` | `bullets.js` adds 0.5; missiles add nothing, despite a 3.5-unit body. Confirmed oversight. One-line change in `rules.rs`. |
| Lock-on through hulls | `missiles.js` | Line of sight ignores `World::boxes`, so you can lock through a mothership. |
| Boss lockability | `main.js:2738` | `hasTarget = (i === 0)` means a HUD-marker flag makes hitbox 0 the only lockable point on the capital ship. Currently overridden in the Rust port — needs a decision. |
| Bridge damage zone misplaced | `main.js` | The one off-plane hitbox is ~105 units from the bridge it represents; the offset is added unrotated but the group carries a π yaw. Fixed by the AABB hull, listed for the record. |
| Server trusts client hit reports | `server/index.js:901`+ | No validation at all. Fine with friends; an open door if the game ever gets strangers. The Rust server should validate against `sim`. |
| `/spaceships/api/*` vs `/api/*` | `auth.js`, `lobby.js`, `main.js` | Client calls a prefixed path the server doesn't register. Works in production behind a proxy; 404s locally. |
| `#nameInput` sanitiser is unreachable | `index.html` | Declared `type="button"`, which never fires `input` events, so the profanity/charset filter never runs on it. |

---

## 5. Other ideas

- **Replace the pixel filter with a real post chain.** `PIXEL_SCALE = 3` renders
  to a third-res target and upscales. In Bevy this is a post-processing pass,
  and worth measuring against the profiler results before porting as-is.
- **Level of detail on asteroids.** 60 rocks all render at full detail
  regardless of distance.
- **Bigger asteroid fields.** Now that generation is seeded, the server ships a
  seed instead of 60 records — field size stops being a bandwidth question.
- **More maps.** Terrain and space both exist; generation is parameterised in
  `rules.rs`.
- **Cross-platform native builds.** Bevy targets Windows and Linux from the same
  source once macOS works.
