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

## 2. EMP

Blind them instead of killing them. The cockpit goes dark, the aim cone
disappears, and for a few seconds everyone in range has to fly and shoot by
eye.

### Why it fits

The counterplay pattern that makes missiles the best weapon in the game is a
decision on *both* sides. An EMP is the same shape: the attacker spends a
charged resource and picks a moment; the victim has to keep fighting without
the crutches they had a second ago.

It also justifies the cockpit view. `cockpit.js` and `dash.js` already render
an instrument panel, a radar with live contact blips, and TGT/MSL annunciators
— all of which can go dark. That is a far better EMP than a screen flash.

### What it disables

- Cockpit lighting, instrument panel, radar (`cockpit.js`, `dash.js`)
- Aim assist entirely — no cone, no pull, no lead marker (`main.js`, grep
  `solveIntercept` and the aim-assist block)
- Target boxes, target labels, missile lock, and the lock warning
- HUD bars, if we want it to bite harder

**Not** flight controls. Taking away someone's ability to steer is frustrating,
not tense. Take away their *information*, leave them their hands.

### Charge mechanic

Cannot be spammed and cannot be respawn-cycled: the EMP builds charge over time
and the meter does **not** reset on death, so dying does not refund it and a
fresh spawn does not arrive armed. Full charge is the cost; the timing is the
skill.

### Open design questions

- **Does it blind allies too?** "Everyone's cockpit going dark" is the more
  interesting version — it makes an EMP genuinely risky to fire inside a
  furball and self-limiting without a balance patch. Worth trying friendly
  blinding first.
- **Aim assist is forced on for keyboard and mobile schemes.** So an EMP hurts
  those players much harder than mouse players, who lose a crutch rather than
  their aim. Either soften the assist loss on those schemes, or lean in and
  make it a known matchup — but decide deliberately rather than shipping the
  accident.
- **Give the victim something to do.** A "reboot" input — mash a key to restore
  systems faster — turns dead time into agency. Otherwise the victim is just
  waiting, which is the least fun state in any game.
- Should it kill the audio warnings below? A dead cockpit that also goes silent
  is a genuinely unsettling few seconds.

---

## 3. Audio warnings ("Bitchin' Betty")

F-16-style spoken warnings: *"PULL UP"*, *"TERRAIN"*, *"CAUTION TERRAIN"*,
*"ALTITUDE"*, *"MISSILE LOCK"*, *"OVER-G"*.

### Why this is the best effort-to-payoff item in the backlog

Almost every trigger already exists in the code and is currently only expressed
visually. This is mostly wiring plus audio clips.

| Warning | Trigger that already exists |
|---|---|
| **PULL UP** / **TERRAIN** / **CAUTION TERRAIN** | `getTerrainHeight` and `TERRAIN_KILL_CLEARANCE` in `terrain.js` — ground contact is instant death on Sierras |
| **ALTITUDE, ALTITUDE** | same, at a softer threshold |
| **MISSILE LOCK** | `#missile-lock-warning` already blinks red at 0.25 s |
| **OVER-G** | the brake-charge overcharge — `BRAKE_OVERCHARGE_WARN` at 1.0 s, damage at 2.0 s, 10 HP/s. This mapping is exact. |
| **LOW FUEL** | `MAX_BOOST` 10 s tank |
| **WINCHESTER** / low ammo | `MAX_AMMO` 90 |
| **WARNING** at low HP | existing hit vignette threshold |

It also solves a real problem rather than just adding flavour: in the cockpit
view, and any time you are using free-look, you cannot see the HUD. Audio is
the only channel that still reaches you while you are looking over your
shoulder — which is exactly when you are being shot at.

### Design notes that matter

- **Priority hierarchy, not a queue.** Real aircraft rank warnings and let the
  urgent one interrupt. *PULL UP* must cut off *low fuel* mid-word. Without
  this, five warnings talk over each other and the feature becomes noise.
- **Per-warning cooldown**, or it nags. The classic failure mode is *"PULL UP"*
  firing forty times in a canyon run until players mute the game.
- **Duck music, not SFX.** Music and SFX already have separate volume sliders;
  the warning should sit above the music and below a missile detonation.
- Pairs with the existing cockpit annunciators — the light and the voice should
  fire together, so the same information reaches you whether you are looking in
  or out.

---

## 4. Intro cinematic

The thing that plays before the menu. Two admin jets, one trickshot, one cut.

### The sequence

1. **Exterior.** An admin jet pulls a **Kvochur's Bell** — vertical, bleed to
   zero airspeed, hang on thrust, then the nose falls through tail-first —
   rolling into a spin on the way out.
2. It **dumps flares** mid-pivot and a missile goes wide through the burn.
3. A **second admin jet blasts past** close enough to shake the camera.
4. **Hard cut to the interior** of the jet that fired the missile — cockpit
   view, instruments live.
5. That pilot **free-looks around and up**, tracking the other jet.
6. **MISSILE LOCK.** The reticle goes red, the warning blinks, the voice
   callout fires.
7. They **explode.** Everything goes dark. Title.

The joke is the reversal: you follow the shooter's missile, then find out you
were watching the wrong aircraft the whole time.

### Why it is cheaper than it looks

**The intro is a replay.** Everything above is either the sim running or a
camera looking at it, and section 1 already builds both:

| Beat | What it needs | Status |
|---|---|---|
| The Bell, spin, flares, missile | a recorded input log, replayed | replay system |
| Exterior chase and the fly-by | keyframed camera path | replay system |
| The cut to the other pilot | "click a player to switch view" | replay system |
| Cockpit interior, free-look up | `cockpit.js`, `fpcamera.js` | **exists** |
| Missile lock reticle + warning | `#missile-lock-warning`, `lock.mp3` | **exists** |
| Explosion, fade to black | `shipdeath`, `#campaign-warp-flash` | **exists** |
| Both jets in admin skins | `spaceshipADMIN.glb` | **exists** |

So this is not an animation to author frame by frame — it is a *saved match*
plus a camera track. Build the replay system and the intro becomes a
content-authoring job rather than an engineering one. Determinism means it
plays identically every time, on every machine, forever.

### The Bell is already flyable

The manoeuvre needs the aircraft to rotate independently of where its momentum
is carrying it. That is exactly what drift mode does — `DRIFT_GRIP` 0.3 and
`DRIFT_DRAG` 0.9 decouple facing from velocity. A Bell is: pitch vertical,
enter drift, let throttle bleed, hold the hang, then let the nose fall through
while the velocity vector keeps pointing up.

Worth flying by hand first to confirm it reads well on screen. If it does not,
the flight model may need a touch more authority at low speed — better to find
that out before building a cinematic around it.

### Notes

- **Skippable from frame one.** Any key. No exceptions, no "hold to skip".
- Play it on first launch and from a menu item, not on every boot.
- The audio does a lot of work here: engine doppler on the fly-by, then the
  lock tone, then near-silence for the cut to black. `stopWarnings()` exists
  for exactly that kind of hard cut.
- Consider ending on the same nebula skybox the menu uses, so the intro
  dissolves into the lobby rather than cutting to it.

---

## 5. Co-op bombing missions

An open-map strike mission. A bomber run against a defended ground target, with
escorts and interceptors, and every role fillable by a player *or* an AI.

### The shape

**Strike side.** One or two bombers fly to a target and put ordnance on it.
Two-crew is the interesting version: one player flies, the other takes the
bombardier seat, looks down, finds the aim point and releases. Escorts — human
or AI — keep the interceptors off them.

**Defence side.** Patrol fighters scramble to intercept. Also human or AI.

**Any seat can be a bot.** This is the feature that makes the mode work at all,
because the game usually has one or two people in it. Two players plus AI in
every other role should play as a complete mission, and the same mission should
scale up to a full lobby without changing.

### What already exists

| Piece | Status |
|---|---|
| Open map with terrain, trees, cloud layer, airfields at Z=+/-1500 | **exists** (Sierras) |
| Ground contact is instant death — the low run is genuinely dangerous | **exists** |
| Terrain proximity voice warnings on the bomb run | **exists** |
| Bombardier view: cockpit + free-look pointed down | **exists** (`fpcamera.js`, `cockpit.js`) |
| AI that seeks, attacks and evades | **exists** (`bot.js`) |
| Filling empty slots with bots | **exists** (`allowBot` on `create`) |
| Two teams with separate spawns | **exists** |
| A defended target with aiming turrets | **exists** (the capital ship) |

### What is genuinely new

- **Bombs.** An unguided, gravity-affected weapon. Different enough from every
  existing weapon to be interesting: no lock, no homing, and the skill is the
  release solution rather than the aim.
- **Ground targets** with damage state and a destruction condition.
- **Two players in one aircraft.** This is the real engineering cost. `sim`
  currently takes one `Input` per ship; a crewed aircraft needs pilot input and
  bombardier input against the same entity, which is a `World`/`Input` change,
  not a client feature.
- Mission flow: objectives, success and failure conditions, scoring.

### Design tensions worth deciding early

- **The passenger problem.** Sitting in a seat you cannot fly is boring unless
  the role has real agency. The instinct to let one player do both is the safer
  default — so make the second seat a **buff, not a requirement**: solo you can
  bomb, but the release solution is harder and less accurate; crewed, the
  bombardier gets a proper sight and a better drop. Nobody is ever stuck being
  cargo, and two-up is genuinely better.
- **Interceptors need a real window.** If bombers just fly to a coordinate, the
  defenders have nothing to do. The tension comes from the bomber being slow,
  low and committed during the run — that is the interceptors' moment, and the
  escorts' job is to make it survivable.
- **Bombing has to be a skill.** If it is "fly over, press key", it is a chore.
  Altitude, speed and a lead solution should all matter, so getting good at it
  is visible.
- **AI has to be adequate, not equal.** A bot escort that cannot hold its own
  makes the mode feel empty. `bot.js` currently only knows seek/attack/evade —
  escorting, patrolling a route, and defending a point are new behaviours.

### Why it fits this game

Every other mode is a dogfight. This is the first one where the objective is
somewhere else and the fighting is *in the way of it* — which is what makes
combined-arms missions read as a real combat situation rather than a scoreboard.

It also gives the terrain map a reason to exist. Right now Sierras is an
alternate dogfight arena; here the ground is the point.

---

## 6. Falls out of the replay system nearly free

- **Killcam.** A replay bounded to the 5 seconds before your death, from the
  killer's view.
- **Spectator mode.** A live replay with zero seek offset. Same camera code.
- **Trials ghosts.** Trials already track best times per mission
  (`spaceships:trial4Best` etc.). A ghost is a stored input log replayed
  alongside you. Race yourself.
- **Match history.** Store replays server-side, browse past matches from the
  profile screen.

---

## 7. Netcode: rollback

A deterministic simulation is the hard prerequisite for rollback netcode — the
thing that makes fighting games feel lagless online. The client predicts
forward, and when a late input arrives it rewinds and re-simulates. Only
possible because re-simulation is exact.

Worth doing if the game ever has real concurrent players. Large job; the
foundation is already in place.

---

## 8. Known gameplay issues found during the port

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

## 9. Other ideas

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
