# Spaceships → Rust Sim Port Inventory

Scope: `public/src/main.js` (3469 lines, one `startGame(opts)` closure), plus
`ship.js`, `bullets.js`, `beams.js`, `missiles.js`, `asteroids.js`, `bot.js`,
`terrain.js`, `airfield.js`, and `server/index.js` for duplication analysis.

Legend: **SIM** = portable pure logic · **RENDER** = Three.js, stays JS ·
**IO** = input/audio/DOM/localStorage/WebSocket, stays JS · **MIXED** = needs splitting.

---

## 1. Region table — `public/src/main.js`

| Lines | Class | What it does | Notes / risks |
|---|---|---|---|
| 1–23 | IO/RENDER | ES module imports | `bullets.js` exports `BULLET_SPEED` (the only sim constant imported) |
| 25–27 | — | `started` re-entry guard | |
| 28–68 | RENDER | `WebGLRenderer`, `PerspectiveCamera`, pixel-filter render target + `renderFrame()` | `pixelEnabled` read from localStorage (`main.js:37`) |
| 69–81 | RENDER | `THREE.Clock`, warp effect, `loadingLoop()`, `await loadShipModel()` | Loading loop is a second render loop; sim never runs here |
| 82–111 | RENDER | Admin GLB preload, `MAP_TYPE`, lights, skybox/fog | `MAP_TYPE = opts.map \|\| 'space'` (`main.js:84`) is a sim input |
| 112–114 | SIM | `isTrialsMode`, `isCampaign`, `CAMPAIGN_MISSION` | Mode discriminant for the whole state machine |
| 115–148 | MIXED | Builds mothership / airfield meshes **and** `motherships[]` (`main.js:138–141`) | **Seam:** only `{pos, halfSize}` is sim. `MOTHERSHIP_HALF = (45,18,35)` at `:115`; terrain uses `AIRFIELD_HALF = (280,4,190)` (`airfield.js:2`). Positions z=∓600 (space) / ∓1500 (terrain) |
| 149–162 | MIXED | Terrain mesh, trees, clouds, moon | **Seam:** `obstacles = [{pos: moon.pos, radius: 80}]` (`:159`) and `moonAvoid` (`:160–162`) are sim; the meshes are not. `MOON_RADIUS = 80` at `:156` |
| 163–206 | MIXED | `createShip`, admin model hot-swap, initial spawn pose | **Seam:** `main.js:195–204` sets spawn position/quat = sim. `SHIP_SCALE = 1.5` (`:163`) feeds `shipRadius` (`:955`) |
| 207–210 | SIM | `_trialRockCount`: trials4=210, trials3=180, trials2=150, trials1=120, else 60 | |
| 212–246 | SIM | `genCampaignAsteroids()` — 3 zones × 90/100/90 = 280 rocks, local tier table | Pure `Math.random()` data gen, no Three.js. **Tier table duplicated 3×** (see §4) |
| 247–254 | MIXED | Chooses one of 3 asteroid constructors, adds group to scene | Constructor choice is sim; mesh building is render |
| 255–326 | SIM (data) | `TRIAL1_CPS`…`TRIAL4_CPS` checkpoint rings (12/14/16/18 points), `TRIAL_CPS` select | Stored as `THREE.Vector3` but pure data → `[f32;3]` |
| 327–331 | IO | `TRIAL_BEST_KEY` localStorage key, `TRIAL_NUM` | |
| 332–343 | SIM | `CP_TRIGGER_DIST = 55`, trials mutable state (`trialsNextCp`, `trialsTimer`, `trialsRunning`, `trialsBestLap`, `trialsLastLap`, `trialsLap`, `cpCooldown`, `trialsCountdown*`) | |
| 344–379 | MIXED | Trials init: reads best lap from localStorage (IO), builds torus + tracer-dot meshes (RENDER), `cpCooldown = 1.5`, `trialsCountdown = 3.0` (SIM), DOM countdown (IO) | |
| 380–388 | MIXED | `createBullets({shipHitRadius: coarseAim ? 7.0 : 6.0})`, beams, missiles, trails | **`shipHitRadius` at `:381` is a sim constant hidden in a render constructor** |
| 389–403 | MIXED | `myAlive`, third/first-person cameras, cockpit group | `myAlive` (`:393`) is sim; everything else render |
| 404–421 | RENDER/IO | `camTel` telemetry struct, `RADAR_RANGE = 1200`, canopy regex | `camTel` is the existing (partial) sim→render snapshot; useful template for the Rust output struct |
| 422–469 | RENDER | `applyExteriorMode`, `syncShipVisibility`, `setViewMode`, `updateCamera` | |
| 470–489 | IO | `new Input(...)`, control scheme, touch HUD, audio volumes | |
| 490–506 | IO | `distanceVol()` + SFX volume constants | `ZERO_VEC` (`:490`) is **dead** |
| 507–513 | IO+SIM | `ws`, `myId`, `isSolo`, `remotePlayers` Map, `remoteColors`, `PALETTE` | `remotePlayers` is the entity table — sim's core collection |
| 514–544 | RENDER | Canvas dot textures, marker sprite materials | |
| 545–550 | SIM | **`SHIP_MAX_HP = 100`, `RESPAWN_DELAY = 2.5`, `SPAWN_INVULN_DURATION = 2.0`**, `myHp`, `myRespawnTimer`, `myInvulnTimer` | Duplicated on server — see §4 |
| 551–572 | SIM | `scores` Map (name/team/kills/deaths) seeded from `opts.players` | `if (isSolo) {}` at `:571–572` is an empty dead block |
| 573–607 | IO | `renderScoreboard()` — DOM table build | |
| 608–631 | **SIM** | `solveIntercept()` — quadratic intercept solver | **Zero dependencies. Port this first.** |
| 632–702 | MIXED | `getOrCreateRemote(id)` — creates sim record (`hp`, `alive`, `team`, `vel`, `targetPos/Quat`, `hasTarget`) **and** a Three.js ship + 2 DOM divs + marker sprite | **Seam:** entity spawn vs. view spawn. Rust owns the record; JS keeps an id→mesh map |
| 703–712 | MIXED | `explodeAt()` (render), `killRemote()` (sim `alive=false` + render) | |
| 713–752 | MIXED | `reviveRemote`, `killSelf`, `reviveSelf` (resets `myHp`/`missilesLeft`/`flaresLeft`/throttle — sim), `removeRemote` | |
| 753–762 | IO | Sends `colors` / `ship-model` on connect | |
| 763–951 | IO (+sim mutations) | WebSocket `message` handler. Branches: `colors`(767) `ship-model`(775) `state`(802) `disconnect`(829) `players`(833) `match-state`(850) `match-end`(857) `hp`(863) `death`(876) `respawn`(892) `fire`(901) `flare`(930) `match-credits`(934) `asteroid-hp`(940) `asteroid-destroyed`(942) | **Seam:** this is the authoritative-state ingress. Remote velocity estimation at `:806–818` (uses `performance.now()`) is SIM and must move |
| 953–1129 | SIM (constants+state) | All flight, weapon, ammo, boost, regen constants and their mutable counters | Interleaved DOM lookups at `:1003–04`, `:1069–74`, `:1078–82`, `:1094–98`, `:1122–25` must be stripped |
| **1130–1948** | **MIXED** | **`update(dt)` — the monolith.** Broken down below | ~820 lines mixing every concern |
| 1131–1152 | IO | Gamepad poll, pause-menu navigation | |
| 1153 | RENDER | `warpEffect.update(dt)` | |
| 1154–1174 | MIXED | Trials countdown: `trialsCountdown -= dt` (sim) + DOM text/colour (IO) + early `return` | Early return skips the whole sim tick — a real control-flow rule |
| 1175–1235 | MIXED | Input → attitude. Throttle damp (`:1192`), steer deadzone/curve (`:1200–1203`), arrow-key ramp (`:1204–1215`), pitch/yaw/roll quaternion integration (`:1218–1230`), aim-assist call (`:1231–1234`) | **Seam:** `input.keys.has(...)` reads must become an `Input` struct field. Everything after that is pure |
| 1236–1250 | SIM | Boost meter drain/recharge + brake-release boost timer | |
| 1251–1272 | SIM | Velocity integration: drift branch (`:1252–1260`) vs. thrust branch (`:1261–1270`), `position += vel*dt` | Uses `lerp(t, 1 - 0.001^(dt*k/6))` idiom — reproduce exactly |
| 1273–1303 | MIXED | Brake charge, overcharge self-damage (`BRAKE_OVERCHARGE_DPS = 10` after 2.0 s) + ws send | **Seam:** damage accumulation is sim, the `ws.send`/`applyPlayerDamageLocal` split at `:1293–1297` is IO |
| 1304–1328 | IO | Charge/boost/heat bars, missile & flare pips, missile-lock warning | |
| 1329–1363 | MIXED | Edge-detected keys: `P` gun toggle (SIM), `C` aim assist (SIM+localStorage), `O` pointer lock (IO), `L` fullscreen (IO), `V` view mode (RENDER) | |
| 1364–1419 | MIXED | Missile lock-on: nearest non-teammate with LOS check against asteroids + obstacles (SIM), then `missileSystem.fire` (render) + audio + ws | Pure target-selection logic buried in a keypress handler |
| 1420–1433 | MIXED | Flare deploy (`flaresLeft--`) + render burst + ws | |
| 1434–1506 | MIXED | Gun fire: cooldown/ammo cost (SIM), beam `castWorldRay` (SIM), boss sphere test radius 95 (`:1451–1454`, SIM), beam/bullet spawn (RENDER), damage dispatch (SIM), ws send (IO) | **The single most tangled block.** Ammo cost 3 (beam) / 1 (bullet); `BEAM_COOLDOWN 0.25` / `BULLET_COOLDOWN 0.05` |
| 1507–1519 | SIM | Health regen: 2.0 s idle from both damage and firing, +1 HP / 0.1 s | |
| 1520–1562 | IO/RENDER | Audio ducking (`moveVol`/`boostVol`) + engine trail particle emission | |
| 1563–1592 | RENDER | Remote-player trail emission | |
| 1593–1595 | RENDER | `asteroids.update` (spin + hit flash), `moon.update`, `beams.update` | Asteroid spin is cosmetic-only — keep in JS |
| 1596–1655 | MIXED | `bullets.update(...)` and `missileSystem.update(...)` with damage callbacks | **Seam:** ballistics+collision live in `bullets.js`/`missiles.js` and are sim; the callbacks branch 3 ways (boss / ws / solo) |
| 1656–1662 | RENDER | Trails, clouds, sun-follows-ship | |
| 1663–1666 | SIM | `resolveCollisions()` + `resolveMothershipCollisions()` | |
| 1667 | RENDER | `updateCamera(dt)` | |
| 1668–1695 | IO | 20 Hz `state` send; MP bot AI tick + `bot-state` send | `STATE_INTERVAL = 1/20` (`:953`) |
| 1696–1701 | SIM | Remote-player position/quaternion interpolation (`1 - 0.001^(dt*8)`) | |
| 1702–1774 | SIM | Solo block: bot AI ticks, bot respawn timers, player respawn timer, match timer, **trials checkpoint logic (`:1720–1750`)**, tracer dots (render, `:1755–1769`), `updateCampaign` call | Checkpoint award: `+3.5` boost, `cpCooldown = 1.5`, lap timing + localStorage best write (`:1740`) |
| 1775 | IO | `tutorial.update(dt)` | |
| 1776–1879 | IO/RENDER | Reticle world→screen projection, per-target DOM boxes, LOS occlusion tests, lead markers, alignment/lock | LOS raycasts (`:1827–1845`) are sim math used for a purely visual result |
| 1880–1911 | RENDER | Cockpit telemetry fill + radar contact transform | Good model for the render snapshot |
| 1912–1947 | IO | HUD text, health bar, hit vignette, invuln strobe, death banner | |
| 1949–1992 | IO | Aim-assist toast, kill feed DOM | |
| 1993–2097 | **SIM** | `applyAimAssist()` — intercept lead, cone/sticky target select, LOS occlusion, damped rotation onto target | Pure math; only reads positions. High-value early port |
| 2098–2139 | **SIM** | `collideSphereWithBox()` + `resolveMothershipCollisions()` | Restitution 1.4 |
| 2140–2166 | MIXED | `dealSelfDamage` (ws vs. local branch), `damageAsteroidLocal` (SIM: −1 HP, destroy at 0) + explosion/audio | `damageAsteroidLocal` (`:2154`) is the client mirror of `server/index.js:808–820` |
| 2167–2238 | **SIM** | `resolveCollisions()` — asteroid sphere pushout + `[15,29]` random damage on first contact, moon contact = instant death, terrain kill plane = instant death | `touchingAsteroids` Set gives edge-triggered damage; restitution 1.3 |
| 2239–2246 | SIM | `SOLO_MODE`, `myTeam`, `MATCH_DURATION` (180 train / 300 else), `teamKills`, `matchTimer`, `matchOver`, `matchActive`, `soloBotsKilled` | |
| 2247–2294 | SIM (data) | `BOSS_ID_BASE=9000`, `BOSS_HITBOX_COUNT=20`, `BOSS_MAX_HP=2500`, `CAMPAIGN_WAVES` per mission, `MISSION_BRIEFINGS` (IO strings), `BOSS_HB_OFFSETS_WORLD` | |
| 2295–2314 | SIM | Campaign mutable state (`campaignPhase`, `campaignBotsAlive`, `campaignBetween*`, `bossHp`, `bossActive`, `bossBullets`, `campaignOver`, `campaignLives=3`, `campaignCheckpointPos`, `campaignWarp*`) | `bossFireTimer` (`:2303`, `:2705`) is written but never read — **dead** |
| 2315–2353 | IO | Achievement toast queue + localStorage stash | |
| 2354–2416 | IO | `reportSoloResult` / `reportCampaignResult` / `reportTrialTime` — `fetch()` to REST API | |
| 2417–2442 | SIM | `makeBotEntity`, `playerEntity`, `localShipRecord` — getter-based adapters bots use | These exist only because state is scattered; in Rust they collapse into `World` indices |
| 2443–2497 | MIXED | `spawnBot()` — sim record init + `createBotAI` wiring + render/DOM via `getOrCreateRemote` | |
| 2498–2535 | IO | Campaign HUD, message banner, lives display | |
| 2536–2630 | RENDER | `buildCapitalShip()` — pure geometry (~95 lines) | |
| 2631–2674 | MIXED | `updateCapitalShip()` — boss drift (`sin` of `capitalShipTime`), hitbox repositioning, turret aim yaw/pitch, turret fire timers scaled by HP fraction, `bossBullets.push` (all SIM) + `pivot.rotation`/muzzle light/`setTimeout` (RENDER) | **Seam:** turret solve + fire schedule is sim; the pivot transform is render |
| 2675–2693 | SIM | `spawnCampaignWave()` — spawns `wave.count` bots around `(0,20,wave.spawnZ)` | |
| 2694–2723 | MIXED | `applyBossHit()` (SIM), `activateBossPhase()` (SIM + DOM + light) | |
| 2724–2741 | **DEAD** | `fireFromBoss()` — never called anywhere | Confirm before deleting |
| 2742–2761 | SIM | `updateBoss()` — boss bullet integration, `PLAYER_HIT_R = 7.0`, `BOSS_BULLET_DMG = 14` | |
| 2762–2808 | MIXED | `endCampaignVictory()` — sets `campaignOver`/phase 4 (SIM), localStorage, REST report, explosion `setInterval`, result DOM | |
| 2809–2870 | SIM | `updateCampaign(dt)` — the mission state machine (wave-clear detection → between-wave timer → next wave / boss phase) | Has DOM peeks at `:2814–2815`, `:2866–2867` to strip |
| 2871–2892 | SIM | `spawnSoloEntities()` — train (1 bot), skirmish (4 allies + 5 enemies), campaign (wave 0) | |
| 2893–2928 | MIXED | Boss hitbox pseudo-players inserted into `remotePlayers` with `hitRadius: 28` | `hitRadius` is honoured by `bullets.js:144` but **not** by `missiles.js:402` — see §4 |
| 2929–3028 | MIXED | `spawnMultiplayerBot()` — opponent adapters that send `hit` messages instead of applying damage | |
| 3029–3173 | IO | `createTutorial()` — DOM-driven step machine reading sim state | |
| 3174–3204 | MIXED | **`applyHitToBot()`** — HP, death, respawn timer, teamKills, kills/deaths, kill feed, scoreboard | **Core damage rule; duplicates `server/index.js:901–960`.** No spawn-invuln check (server has one) |
| 3205–3252 | MIXED | **`applyPlayerDamageLocal()`** — invuln gate, HP, death, campaign lives, respawn scheduling | Campaign death path (`:3213–3231`) sets `myRespawnTimer = 1.5`, else `RESPAWN_DELAY` |
| 3253–3281 | SIM | `reviveBotLocal()` — anchor by mode + jitter, reset HP/vel, `notifyRespawn` | |
| 3282–3320 | MIXED | `revivePlayerLocal()` — trials reset (+ ring mesh recolour), campaign checkpoint respawn at 55% HP (`:3318`), skirmish/train jittered spawn | |
| 3321–3372 | IO | Match HUD, trials HUD formatters, campaign HUD bootstrap | |
| 3373–3402 | MIXED | `endMatch()` — `matchOver = true` + REST report + result DOM | |
| 3403–3446 | IO | Pause overlay, Tab scoreboard toggle | |
| 3447–3470 | MIXED | `loop()` — `dt = min(0.05, clock.getDelta())`, `update`, `touchHud`, `renderFrame`; resize handler | **The tick boundary.** `dt` clamp is a sim rule |

### Region table — sim-relevant modules

| File:lines | Class | What it does | Notes |
|---|---|---|---|
| `ship.js:1–92` | RENDER | GLB load/cache, colour application, primitive fallback | Nothing to port |
| `bullets.js:2` | SIM | `BULLET_SPEED = 780` | Duplicated in `bot.js:26` |
| `bullets.js:3–73` | RENDER | Geometry, materials, explosion pool | |
| `bullets.js:74–87` | SIM | `SHIP_HIT_RADIUS`, `sweptHit()` swept-sphere test | Swept test used **only** for `obstacles` (moon), not for ships/asteroids — fast bullets can tunnel through rocks |
| `bullets.js:88–157` | MIXED | `update()` — integration, asteroid/obstacle/ship collision, callbacks | **Position lives in `b.mesh.position`.** Seam = give bullets plain vectors |
| `bullets.js:158–170` | RENDER | Explosion animation | |
| `beams.js:1–49` | RENDER | Beam cylinder spawn + fade. **No collision at all** — hit detection is `castWorldRay` in main.js | Pure render |
| `missiles.js:2–24` | SIM | `MISSILE_SPEED 160`, `TURN_RATE 1.4`, `LIFE 8.0`, `HIT_RADIUS 6.0`, avoidance constants, `FLARE_SPEED 140`, `FLARE_LIFE 1.8`, `FLARE_COUNT 20`, `FLARE_SEDUCTION_DIST 180` | |
| `missiles.js:25–62` | RENDER | Static geometry | |
| `missiles.js:80–142` | SIM | `computeAvoidance()`, `insideObstacle()` | |
| `missiles.js:143–235` | RENDER | Mesh factory, trails, explosion layers | |
| `missiles.js:236–281` | MIXED | `deployFlare()` — 20 random-direction flares (SIM: dir/speed/life) + meshes | |
| `missiles.js:282–307` | SIM | `isTargetingLocal()`, `fire()` | |
| `missiles.js:308–429` | **SIM** | Missile update: **flare seduction (`:316–326`)**, target tracking, avoidance blending, angle-limited turn, obstacle detonation, flare/ship hit tests | The highest-value single sim block after collisions |
| `missiles.js:430–482` | RENDER | Trail/explosion/flare particle animation (flare *motion* at `:466–467` is sim) | |
| `asteroids.js:14–28` | SIM | `TIERS` table + `pickTier()` | Duplicated in `main.js:220–225` and `server/index.js:511–516` |
| `asteroids.js:29–57` | RENDER | Icosahedron variant deformation, shared material | |
| `asteroids.js:59–77` | SIM | `makeMutators` → `destroy(id)`, `setHp(id, hp)` | |
| `asteroids.js:79–109` | MIXED | `createAsteroidFieldFromData()` — builds list + meshes from server data | `radius = size * 0.95` (`:93`) is the sim collision radius |
| `asteroids.js:110–185` | MIXED | `createAsteroidField()` — **local field generation** (placement, avoidance, tiering) | Sim generation duplicated on the server |
| `asteroids.js:202–205` | SIM | `pseudoNoise()` | Render-only use today |
| `bot.js:13–56` | SIM | All AI tuning constants | `BULLET_SPEED 780` / `BULLET_LIFE 2.0` re-declared from `bullets.js` |
| `bot.js:57–129` | SIM | `chooseEvadeDir`, `rotateToward`, `computeAvoidance`, `pickTarget` | |
| `bot.js:130–300` | **SIM** | `update(dt)` — seek/attack/evade FSM, aim wander, intercept lead, avoidance, terrain clearance, kinematics, sphere pushout vs. obstacles+asteroids, stuck detection, gun & missile fire gates | Mutates `record.ship.position/quaternion` (Three.js objects) — the only Three.js coupling |
| `bot.js:301–313` | MIXED | `fireBullet()` — spawns a *visual* bullet **and** a shadow projectile | See §4: two parallel bullet sims |
| `bot.js:314–357` | **SIM** | `updateProjectiles()` — the authoritative bot bullet sim (`BULLET_HIT_R = 4.0`) | |
| `bot.js:358–374` | SIM | `notifyHit`, `notifyRespawn` | |
| `terrain.js:2–4` | SIM | `TERRAIN_SIZE 3600`, `TERRAIN_KILL_CLEARANCE 5` | |
| `terrain.js:5–40` | **SIM** | `airfieldBlend`, `rawHeight` (7 sin/cos octaves), `getTerrainHeight` | Pure; used by collision (`main.js:2225`) and bot clearance (`bot.js:197`) |
| `terrain.js:41–87` | RENDER | Mesh + vertex colours | Must call the *same* height fn as Rust or the ground desyncs from the visual |
| `airfield.js:2` | SIM | `AIRFIELD_HALF = (280, 4, 190)` | |
| `airfield.js:3–89` | RENDER | Airfield props | |

---

## 2. Sim surface — proposed Rust `World`

```rust
// ---------- ids ----------
pub type EntityId = i32;          // players & bots share the id space; boss hitboxes are 9000..9020
pub const BOSS_ID_BASE: EntityId = 9000;

// ---------- tunables (frozen from main.js; single source of truth) ----------
pub struct Rules {
    pub ship_max_hp: i32,               // 100          main.js:545 / server:415
    pub respawn_delay: f32,             // 2.5 (CLIENT) vs 2.0 (SERVER) — see §4
    pub spawn_invuln: f32,              // 2.0          main.js:547
    pub max_throttle: f32,              // 80           main.js:957
    pub boost_factor: f32,              // 1.7          main.js:958
    pub pitch_rate: f32,                // 1.75         main.js:961
    pub pitch_up_boost: f32,            // 1.25         main.js:962
    pub yaw_rate: f32,                  // 1.3          main.js:963
    pub roll_rate: f32,                 // 1.4          main.js:964
    pub velocity_blend: f32,            // 4            main.js:965
    pub velocity_blend_release: f32,    // 1.5          main.js:993
    pub drift_drag: f32,                // 0.9          main.js:990
    pub drift_grip: f32,                // 0.3          main.js:991
    pub drift_brake: f32,               // 0.1          main.js:992
    pub brake_full_time: f32,           // 1.4          main.js:986
    pub brake_boost_min: f32,           // 0.18         main.js:987
    pub brake_boost_duration_max: f32,  // 1.0          main.js:988
    pub brake_boost_bonus_max: f32,     // 50           main.js:989
    pub brake_overcharge_damage_t: f32, // 2.0          main.js:995
    pub brake_overcharge_dps: f32,      // 10           main.js:996
    pub bullet_cooldown: f32,           // 0.05         main.js:1005
    pub beam_cooldown: f32,             // 0.25         main.js:1006
    pub beam_range: f32,                // 1000         main.js:1014
    pub max_ammo: f32,                  // 90           main.js:1062
    pub ammo_regen: f32,                // 36/s         main.js:1063
    pub ammo_regen_delay: f32,          // 1.0          main.js:1061
    pub missile_max: u8,                // 4            main.js:1066
    pub flare_max: u8,                  // 3            main.js:1075
    pub max_boost: f32,                 // 10           main.js:1083
    pub boost_drain: f32,               // 2/s          main.js:1084
    pub boost_recharge: f32,            // 4/s          main.js:1085
    pub health_regen_delay: f32,        // 2.0          main.js:1089
    pub health_regen_interval: f32,     // 0.1          main.js:1090
    pub dmg_bullet: i32,                // 10           main.js:1607 / server:936
    pub dmg_beam: i32,                  // 10           main.js:1472
    pub dmg_missile: i32,               // 50           main.js:1630 / server:936
    pub asteroid_dmg_per_hit: i32,      // 1            main.js:2158 / server:813
    // hit radii — currently FIVE different values, see §4
    pub hit_r_bullet_ship: f32,         // 6.0 or 7.0   main.js:381
    pub hit_r_beam_ship: f32,           // 5.5          main.js:1015
    pub hit_r_missile_ship: f32,        // 6.0          missiles.js:5
    pub hit_r_bot_bullet: f32,          // 4.0          bot.js:31+52
    pub hit_r_boss_bullet_player: f32,  // 7.0          main.js:2744
    pub ship_collide_radius: f32,       // 3.3 = 2.2*1.5  main.js:955
}

// ---------- ship kinematics + combat state ----------
pub struct Ship {
    pub id: EntityId,
    pub team: Option<u8>,
    pub pos: Vec3,
    pub quat: Quat,
    pub vel: Vec3,
    pub throttle: f32,          // smoothed        main.js:1121
    pub target_throttle: f32,   //                 main.js:1120
    pub hp: i32,
    pub alive: bool,
    pub respawn_timer: f32,
    pub invuln_timer: f32,
    pub ammo: f32,
    pub ammo_idle: f32,
    pub fire_timer: f32,
    pub gun_mode: GunMode,      // Bullet | Beam   main.js:1009
    pub missiles_left: u8,
    pub flares_left: u8,
    pub boost_meter: f32,
    pub boost_idle: f32,
    pub health_idle_damage: f32,
    pub health_idle_shot: f32,
    pub health_regen_tick: f32,
    pub brake_charge: f32,
    pub brake_boost_timer: f32,
    pub brake_boost_charge: f32,
    pub brake_overcharge_time: f32,
    pub self_damage_accum: f32,
    pub prev_braking: bool,
    pub arrow_kx: f32,          // ramped keyboard steer  main.js:967
    pub arrow_ky: f32,
    pub kind: ShipKind,         // Local | Remote { interp: RemoteInterp } | Bot(BotState) | BossHitbox { hit_radius: f32 }
    pub touching_asteroids: HashSet<u32>,   // edge-trigger for collision damage main.js:2140
    pub touching_moon: bool,
    pub touching_water: bool,
}

pub struct RemoteInterp {          // main.js:802–828, 1696–1701
    pub target_pos: Vec3,
    pub target_quat: Quat,
    pub has_target: bool,
    pub last_state_time: f64,
    pub last_state_pos: Vec3,
    pub vel_seeded: bool,
    pub boost: bool,
}

// ---------- weapons in flight ----------
pub struct Bullet {                 // bullets.js:45 + bot.js:307
    pub pos: Vec3, pub prev_pos: Vec3, pub vel: Vec3,
    pub life: f32,                  // 2.0
    pub owner: EntityId,
    pub owner_team: Option<u8>,
    pub damaging: bool,             // JS `isLocal`: only damaging bullets test ships/asteroids
}

pub struct Missile {                // missiles.js:295
    pub pos: Vec3, pub dir: Vec3, pub vel: Vec3,
    pub target: Option<MissileTarget>,   // Ship(EntityId) | Flare(u32) | None
    pub life: f32,                  // 8.0
    pub age: f32,
    pub owner: EntityId,
    pub owner_team: Option<u8>,
}

pub struct Flare {                  // missiles.js:263
    pub id: u32, pub pos: Vec3, pub vel: Vec3,
    pub life: f32, pub age: f32,    // 1.8
    pub owner: EntityId,
    pub alive: bool,
}

// ---------- world geometry ----------
pub struct Asteroid {               // asteroids.js:93 / :184
    pub id: u32,
    pub pos: Vec3,
    pub radius: f32,                // size * 0.95
    pub size: f32,
    pub hp: i32,
    pub tier: AsteroidTier,         // Small | Medium | Big | Huge
    pub variant: u8,                // render hint
    pub rot: Vec3, pub spin: Vec3,  // cosmetic, could stay in JS
}

pub struct Obstacle { pub pos: Vec3, pub radius: f32 }        // moon: r=80  main.js:159
pub struct BoxVolume { pub pos: Vec3, pub half: Vec3 }        // motherships / airfields  main.js:138

// ---------- bot AI ----------
pub struct BotState {               // bot.js:41–56
    pub state: BotFsm,              // Seek | Attack | Evade
    pub state_timer: f32,
    pub fire_timer: f32,
    pub missiles_left: u8,
    pub missile_timer: f32,
    pub stuck_time: f32,
    pub evade_axis: Vec3,
    pub aim_offset: Vec3,
    pub tracked_lead: Vec3,
    pub tracked_lead_seeded: bool,
    pub hard_mode: bool,
    pub faction_team: Option<u8>,
}

// ---------- modes ----------
pub struct TrialsState {            // main.js:333–343
    pub trial_num: u8,
    pub checkpoints: Vec<Vec3>,
    pub next_cp: usize,
    pub timer: f32, pub lap: u32,
    pub running: bool,
    pub best_lap: Option<f32>, pub last_lap: Option<f32>,
    pub cp_cooldown: f32,
    pub countdown: f32, pub countdown_active: bool,
}

pub struct CampaignState {          // main.js:2295–2314
    pub mission: u8,
    pub phase: u8,                  // 0..2 waves, 3 boss, 4 victory
    pub wave_bot_ids: HashSet<EntityId>,
    pub bots_alive: u32,
    pub between: bool, pub between_timer: f32,
    pub boss_hp: i32, pub boss_active: bool,
    pub boss_bullets: Vec<Bullet>,
    pub boss_time: f32, pub boss_pos: Vec3,
    pub turrets: [Turret; 4],       // { local_pos, yaw, pitch, fire_timer }
    pub over: bool,
    pub lives: i32,                 // 3
    pub checkpoint_pos: Vec3,
    pub next_bot_id: EntityId,      // 100
}

pub struct MatchState {             // main.js:2239–2246
    pub mode: Mode,                 // Train | Skirmish | Trials(u8) | Campaign(u8) | Tutorial | Multiplayer
    pub map: MapKind,               // Space | Terrain
    pub my_team: u8,
    pub team_kills: [u32; 2],
    pub timer: f32,
    pub over: bool,
    pub active: bool,
    pub solo_bots_killed: u32,
    pub scores: IndexMap<EntityId, Score>,   // { name, team, kills, deaths }
}

// ---------- root ----------
pub struct World {
    pub rules: Rules,
    pub rng: Pcg32,                 // REQUIRED: replaces every Math.random() call
    pub time: f64,
    pub local_id: EntityId,
    pub ships: IndexMap<EntityId, Ship>,   // insertion-ordered — JS Map iteration order is load-bearing
    pub bullets: Vec<Bullet>,
    pub missiles: Vec<Missile>,
    pub flares: Vec<Flare>,
    pub asteroids: Vec<Asteroid>,
    pub obstacles: Vec<Obstacle>,
    pub boxes: Vec<BoxVolume>,
    pub aim_assist: AimAssistState, // { enabled, strength_smoothed, has_target, last_target_id, target_dir }
    pub trials: Option<TrialsState>,
    pub campaign: Option<CampaignState>,
    pub matchs: MatchState,
    pub events: Vec<SimEvent>,      // drained each tick by JS
}
```

**Deliberately NOT in `World`:** any Three.js object, DOM node, audio handle,
material, camera, colour, texture, trail particle, explosion, or WebSocket.

---

## 3. Proposed tick signature

```rust
pub fn tick(world: &mut World, inputs: &[(EntityId, Input)], net: &[NetEvent], dt: f32) -> Frame;
```

### Input (replaces `input.keys.has(...)` reads at `main.js:1175–1235`, `:1329–1438`)

```rust
pub struct Input {
    pub steer_x: f32, pub steer_y: f32,   // already deadzoned/curved OR raw — decide once
    pub roll: f32,                        // -1..1 (A/D or gp.rollAxis)
    pub throttle_delta: f32,              // wheel notches * THROTTLE_STEP
    pub throttle_axis: f32,               // W/S or gamepad, applied at KEY_THROTTLE_RATE
    pub throttle_override: Option<f32>,   // touch HUD  main.js:1180
    pub arrow_x: f32, pub arrow_y: f32,   // -1/0/+1 targets for the ramp
    pub arrow_fine: bool,                 // KeyQ modifier  main.js:1209
    pub fire: bool,
    pub braking: bool,
    pub boost: bool,
    pub free_look: bool,
    // edge-triggered, already debounced by JS:
    pub fire_missile: bool,
    pub deploy_flare: bool,
    pub toggle_gun: bool,
    pub toggle_aim_assist: bool,
}
```

`dt` must be pre-clamped to `min(0.05, real_dt)` exactly as `main.js:3449`, or
match behaviour changes. Recommend also offering a fixed-step accumulator wrapper.

### Net events (ingress from `main.js:763–951`)

```rust
pub enum NetEvent {
    RemoteState { id, pos, quat, boost, recv_time },
    Hp { id, hp },
    Death { id, killer: Option<EntityId> },
    Respawn { id, pos, quat },
    Fire { id, kind, shots },
    Flare { id, pos, quat },
    Players(Vec<PlayerRow>),
    MatchState { timer, team_kills: [u32;2] },
    MatchEnd { team_kills: [u32;2] },
    AsteroidHp { id, hp },
    AsteroidDestroyed { id },
    Disconnect { id },
}
```

### Output — what the JS renderer needs to draw one frame

```rust
pub struct Frame {
    // per-entity transforms — JS maps id -> Object3D and copies these
    pub ships: Vec<ShipView>,      // { id, pos, quat, visible, alive, hp, team, hit_flash, boosting, speed }
    pub bullets: Vec<ProjView>,    // { key: u64, pos, dir }   key lets JS reuse meshes
    pub missiles: Vec<ProjView>,
    pub flares: Vec<FlareView>,    // { key, pos, age, life }
    pub asteroids: Vec<RockView>,  // { id, pos, rot, size, hp, hit_flash }  (only on change if you diff)
    pub boss: Option<BossView>,    // { pos, turret_yaw_pitch: [(f32,f32);4], hp, max_hp }

    // one-shot events → particles, audio, HUD toasts, and outbound WS frames
    pub events: Vec<SimEvent>,

    // HUD / cockpit telemetry (mirrors today's `camTel`, main.js:405–409)
    pub hud: HudState,             // { throttle01, speed, hp, hp_frac, ammo01, boost01, charge01,
                                   //   missiles, flares, gun_mode, invuln, target_lock, missile_lock,
                                   //   radar_contacts, reticle_world_point, match_timer, team_kills,
                                   //   trials: Option<TrialsHud>, campaign: Option<CampaignHud> }

    // outbound network intents — JS serialises and sends
    pub net_out: Vec<NetIntent>,   // StateUpdate, Fire, Flare, Hit{target,kind}, AsteroidHit, SelfDamage, BotState
}

pub enum SimEvent {
    BulletFired { owner, origin, dir, faction },
    BeamFired { owner, start, end, faction },
    MissileFired { owner, origin, dir, target },
    FlareBurst { owner, origin, quat },
    Explosion { pos, scale, kind },     // kind: Small | ShipDeath | AsteroidBreak | MissileHit | FlareBurst
    ShipDestroyed { id, killer: Option<EntityId>, pos },
    ShipRespawned { id, pos },
    AsteroidDestroyed { id, pos, radius },
    AsteroidDamaged { id, hp },
    HitMarker { on: EntityId },
    DamageTaken { id, amount, new_hp },
    CheckpointPassed { index, lap_time: Option<f32> },
    LapComplete { time, is_best },
    WaveComplete { phase },
    BossPhaseStarted,
    CampaignFailed,
    CampaignVictory { lives_left: i32 },
    MatchEnded { winner: Option<u8> },
}
```

**Determinism requirements** (all currently violated):
1. Every `Math.random()` in a sim path must become `world.rng`. Call sites include
   asteroid generation (`asteroids.js:148–181`, `main.js:228–241`, `server/index.js:546–572`),
   asteroid collision damage `15 + floor(rand*15)` (`main.js:2190`), flare directions
   (`missiles.js:239–247`), bot aim wander (`bot.js:167–169`), bot evade axis (`bot.js:57–62`),
   bot missile delay (`bot.js:25`), boss turret spread + fire interval (`main.js:2659`, `:2667`),
   and every spawn jitter (`main.js:2683–2686`, `:3267–3271`, `:3306–3309`; `server/index.js:490–504`).
2. `performance.now()` in the remote-velocity estimator (`main.js:806`) must become `world.time`.
3. `remotePlayers` iteration order (a JS `Map`) affects "first hit wins" in
   `bullets.js:138` and `missiles.js:395`. Use `IndexMap`/`Vec`, not `HashMap`.
4. `THREE.MathUtils.damp(x, y, λ, dt) = lerp(x, y, 1 - e^(-λ·dt))` and the
   `lerp(t, 1 - 0.001^(dt·k/6))` idiom must be reproduced bit-for-bit-ish (f32 vs f64:
   JS is f64 throughout — **use `f64` in Rust for the physics, or accept divergence**).

---

## 4. Client / server duplication — and where the values DISAGREE

The server is authoritative only for HP/death/respawn/asteroid-HP in multiplayer.
The client re-implements the same rules for solo. Every row below is a rule
implemented twice.

### 4a. Values that AGREE (still duplicated — should become one Rust constant)

| Rule | Client | Server | Value |
|---|---|---|---|
| Ship max HP | `main.js:545` | `index.js:415` | **100** ✅ |
| Bullet / beam damage | `main.js:1472`, `:1607` | `index.js:936` | **10** ✅ |
| Missile damage | `main.js:1630`, `:1648` | `index.js:936` | **50** ✅ |
| Bot bullet damage | `bot.js:20` | `index.js:936` | **10** ✅ |
| Asteroid damage per hit | `main.js:2158` | `index.js:813` | **1** ✅ |
| Asteroid tier table (size/hp/weight) | `asteroids.js:14–19` **and** `main.js:220–225` | `index.js:511–516` | small 5–7/5/.45, medium 9–15/10/.30, big 18–30/30/.18, huge 38–55/50/.07 ✅ (**triplicated**) |
| Asteroid count / radius (space MP) | `main.js:253` (60, 400) | `index.js:757` (60, 400) | ✅ |
| Asteroid placement: `minDist = 30 + size` | `asteroids.js:160` | `index.js:556` | ✅ |
| Mothership avoid half-size | `main.js:115` `(45,18,35)` | `index.js:518–519` `[45,18,35]` | ✅ |
| Mothership positions | `main.js:129`,`:132` z=∓600 | `index.js:518–519` z=∓600 | ✅ |
| Spawn invulnerability | `main.js:547` = 2.0 s | `index.js:732,751,893,954` = 2000 ms | ✅ numerically (see 4b-3 for the timing caveat) |
| Match duration (skirmish/MP) | `main.js:2241` = 300 s | `index.js:427` = 300 000 ms | ✅ |
| Team spawn Z (space) | `main.js:3303` −540 / `:203` −540 | `index.js:471–472` ∓540 | ✅ |
| Team spawn (terrain) | `main.js:201`,`:3303–3304` z −1400, y 40 | `index.js:477–478` z ∓1400, y 40 | ✅ |
| Friendly-fire rejection | `bullets.js:140`, `missiles.js:398`, `main.js:1444` | `index.js:932` | ✅ |
| No self-damage from own guns | implicit (`b.isLocal` never tests self) | `index.js:931` | ✅ |

### 4b. DISAGREEMENTS — these are bugs

> **🔴 1. RESPAWN DELAY: 2.5 s (client) vs 2.0 s (server) — 500 ms apart.**
> - `public/src/main.js:546` → `const RESPAWN_DELAY = 2.5;`
>   used at `main.js:3182` (solo bot respawn) and `main.js:3233` (solo player respawn).
> - `server/index.js:416` → `const RESPAWN_DELAY_MS = 2000;`
>   used at `index.js:896` and `index.js:957`.
>
> Effect: solo/campaign respawn is 25 % slower than multiplayer. Also note the
> client HUD shows the death banner off `myAlive` with no timer, so nothing
> visibly reconciles them. **Pick one; put it in `Rules::respawn_delay`.**

> **🔴 2. SERVER-GENERATED ASTEROIDS IGNORE THE MOON — they spawn inside it.**
> - Client solo generation avoids the moon: `main.js:160–162` builds `moonAvoid`
>   (`halfSize (80,80,80)`), `main.js:211` folds it into `_avoidList`, and
>   `asteroids.js:114–124 clipsAvoidance()` rejects those placements.
> - Server generation only avoids motherships: `index.js:532–542 clipsMothership()`;
>   `MOON_AVOID` does not exist. `index.js:556–558` places rocks at
>   `dist ∈ [30+size, 400]` from the **origin**, and the moon is at the origin with
>   `MOON_RADIUS = 80` (`main.js:156–158`).
>
> Effect: in every multiplayer space match, asteroids are generated inside the moon
> sphere. Bullets are eaten by the invisible-inside rocks (`bullets.js:96–121`
> runs before the obstacle test at `:122–136`), missiles detonate on entry
> (`missiles.js:355`), and players who clip a buried rock take `[15,29]` collision
> damage (`main.js:2190`) *and* the instant-death moon hit (`main.js:2219`).

> **🔴 3. SOLO BOTS HAVE NO SPAWN INVULNERABILITY; SERVER PLAYERS DO.**
> - `server/index.js:933` → `if (target.invulnUntil && Date.now() < target.invulnUntil) return;`
> - `main.js:3174–3177 applyHitToBot()` checks only `!r.alive` — there is no
>   `invulnTimer` on bot records at all (`main.js:2451–2454` never sets one).
>   `main.js:3205–3207 applyPlayerDamageLocal()` *does* gate on `myInvulnTimer`.
>
> Effect: in solo, a freshly-respawned bot can be killed instantly at the spawn
> anchor; the player cannot. Asymmetric rule.

> **🟠 4. FIVE DIFFERENT SHIP HIT RADII inside the client (server has none).**
> | Weapon | Radius | Site |
> |---|---|---|
> | Player bullet vs. ship | **6.0** (7.0 with coarse aim) + 0.5 bullet radius | `main.js:381`, `bullets.js:74`,`:144` |
> | Player beam vs. ship | **5.5** | `main.js:1015`, used `main.js:1029` |
> | Missile vs. ship | **6.0** | `missiles.js:5`, used `:402`,`:417` |
> | Bot bullet vs. anything | **4.0** (`SHIP_RADIUS 3.5 + 0.5`) | `bot.js:31`, `bot.js:52`, used `:326` |
> | Boss bullet vs. player | **7.0** | `main.js:2744` |
>
> Bots must get ~35 % closer than the player to land a shot with identical geometry.
> The server does **zero** hit validation — `index.js:901–960` trusts the client's
> `hit` message entirely — so these radii are also the whole anti-cheat story.

> **🟠 5. BOSS HIT RADIUS IS THREE DIFFERENT NUMBERS: 28 / 6 / 95.**
> - Boss hitbox records set `hitRadius: 28` (`main.js:2920`), honoured by
>   `bullets.js:144` (`r.hitRadius !== undefined ? r.hitRadius : SHIP_HIT_RADIUS`).
> - `missiles.js:402` hard-codes `HIT_RADIUS` (6.0) and **ignores `hitRadius`**, so a
>   50-damage missile must pass within 6 units of a hitbox point instead of 28.
> - The beam takes a completely separate path: `main.js:1451–1454` ray-tests a single
>   sphere of radius **95** at the capital ship centre.
>
> Effect: against the campaign boss, beams hit trivially, bullets hit easily,
> missiles (the highest-damage weapon) mostly miss.

> **🟠 6. SPAWN JITTER: server ±(4, 2, 3), client solo ±(30, 10, 30).**
> - `server/index.js:498–504` (space): `x = (rand-0.5)*8`, `y = (rand-0.5)*4`, `z = ±540 + (rand-0.5)*6`.
> - `main.js:3305–3309` (solo space): `x = (rand-0.5)*60`, `y = (rand-0.5)*20`, `z = -540 + (rand-0.5)*60`.
> - Terrain: `server/index.js:490–494` `x ±30, y 40±5, z ±1400±20` vs `main.js:3305–3309` `x ±30, y 40±10, z ±1400±30`.
>
> Effect: solo spawns scatter ~10× wider on the space map, sometimes outside the
> mothership hangar mouth the server spawn was tuned for.

> **🟡 7. ASTEROID IDs: server 0-based, client 1-based.**
> - `server/index.js:565` → `id: i` (0…59).
> - `asteroids.js:184` → `id: list.length + 1` (1…N).
> - `main.js:214` (campaign) → `let id = 1`.
>
> Not currently a live bug (all comparisons use explicit `!== null/undefined`), but
> it means id `0` exists only in multiplayer. Any Rust `Option<NonZeroU32>` or
> truthiness-style check will break on the server field. Normalise during the port.

> **🟡 8. ASTEROID SPIN: campaign rocks spin at 20–40 % of everyone else's.**
> - `createAsteroidFieldFromData` multiplies the incoming `spin` by `spinScaleFor(tier)`
>   (`asteroids.js:90–91`).
> - Server supplies raw `±0.5` per axis (`index.js:572`) → final spin `±0.5·scale`. ✅
> - Local `createAsteroidField` produces `±0.5·scale` directly (`asteroids.js:177–181`). ✅
> - Campaign supplies `±0.2` (x,y) / `±0.1` (z) (`main.js:240`) → then multiplied *again*
>   by `spinScaleFor` → **0.4× / 0.2× of standard**.
>
> Cosmetic only (spin never affects collision), but it's the same rule written three
> times with three different results.

### 4c. Duplication *inside* the client (no server involvement)

> **🔴 9. BOT BULLETS ARE SIMULATED TWICE, WITH DIFFERENT RADII.**
> `bot.js:301–313 fireBullet()` spawns a **visual** bullet via `bullets.fire(...)`
> *and* pushes a **shadow** projectile onto `myProjectiles`. The visual one only
> damages ships when `b.isLocal` (`bullets.js:95`, `:137`) — i.e. never for bots —
> so all bot damage comes from the shadow sim (`bot.js:314–357`) with hit radius
> **4.0**, while the bullet you see on screen has effective radius **6.5/7.5**.
> The two also diverge on obstacle handling: shadow projectiles are consumed by
> asteroids/obstacles with radius `+0.5` (`bot.js:334–355`) but **never damage
> asteroids**, and they use no swept test, so at 780 u/s × 0.05 s = 39 units/frame
> they tunnel through everything smaller than 39 units. **Collapse these into one
> `Bullet` list in Rust — this is the single highest-value fix in the port.**

> **🟠 10. `BULLET_SPEED` and `BULLET_LIFE` are declared twice.**
> `bullets.js:2` (`780`) / `bot.js:26` (`780`); `bullets.js:7` (`2.0`) / `bot.js:27` (`2.0`).
> They agree today; nothing enforces that.

> **🟠 11. Swept collision is applied to the moon only.**
> `bullets.js:75–87 sweptHit()` is used at `bullets.js:129` for `obstacles`, but the
> asteroid test (`:96–121`) and ship test (`:137–152`) are point-in-sphere only.
> At 780 u/s a 5-unit "small" asteroid is missed on ~87 % of frames.

> **🟡 12. Player collision radius 3.3 vs. bot collision radius 3.5.**
> `main.js:955` (`2.2 * SHIP_SCALE 1.5 = 3.3`) vs `bot.js:31` (`3.5`).

> **🟡 13. Dead code to drop rather than port.**
> `fireFromBoss()` (`main.js:2724–2741`) is never called; `bossFireTimer`
> (`main.js:2303`, set at `:2705`) is never read; `ZERO_VEC` (`main.js:490`) is unused;
> `if (isSolo) {}` (`main.js:571–572`) is an empty block.

---

## 5. Port order

Ranked by (low Three.js coupling) × (high value). Each step should be shippable
behind a feature flag with the JS path still present for A/B comparison.

| # | Piece | Lines | Rationale |
|---|---|---|---|
| 1 | `solveIntercept` | `main.js:608–631` | Zero dependencies, pure scalar math, used by both aim assist and every bot — a perfect first cross-check of f64 parity. |
| 2 | `raySphereDist` + `castWorldRay` | `main.js:1017–1060` | Pure once asteroids/ships are plain arrays; unblocks beams, missile lock, LOS occlusion and the reticle in one go. |
| 3 | `getTerrainHeight` + `airfieldBlend` | `terrain.js:5–40` | Pure trig, no state; JS keeps calling it for mesh build so both sides provably agree. |
| 4 | Asteroid field generation + tier table | `asteroids.js:14–28`,`:110–185`; `main.js:212–246`; `server/index.js:511–576` | Currently written **three times**; one seeded Rust generator kills bugs #2, #7 and #8 at once and can be compiled to WASM for the server too. |
| 5 | Collision: `collideSphereWithBox`, `resolveCollisions`, `resolveMothershipCollisions` | `main.js:2098–2139`, `:2167–2238` | Self-contained, already operates on plain numbers, and owns three damage rules (asteroid `[15,29]`, moon instant-kill, terrain kill plane). |
| 6 | Ship kinematics: throttle damp, attitude integration, velocity blend, drift, brake charge/boost | `main.js:1175–1272`, `:1236–1250` | The heart of game feel. Only coupling is `input.keys.has(...)` and `ship.quaternion` — both trivially swapped for an `Input` struct and a `Quat` field. |
| 7 | Resource timers: ammo/cooldowns/boost/health regen | `main.js:1434–1519`, `:1236–1250` | Pure counters; no geometry. Immediately shrinks `update()` by ~90 lines. |
| 8 | Bullet ballistics + collision (**unified**, killing the bot shadow sim) | `bullets.js:88–157`, `bot.js:301–357` | Fixes bug #9 and #11. Requires the JS side to switch from `b.mesh.position` to an id→mesh map — the first real render refactor, hence step 8 not 3. |
| 9 | Missile homing + flare seduction | `missiles.js:80–142`, `:236–307`, `:308–429` | Already almost pure; the flare-seduction rule (`:316–326`) and avoidance blending are exactly the kind of thing you want deterministic and testable. |
| 10 | Aim assist | `main.js:1993–2097` | Pure math but depends on #1, #2 and the ship quaternion being in Rust. |
| 11 | Damage / kill / respawn / scoring rules | `main.js:3174–3252`, `:3253–3320`; mirror `server/index.js:901–960` | Reconciles bugs #1, #3 and #6. Do it once the entities are already in Rust, or you'll be shuffling data across the boundary. |
| 12 | Bot AI | `bot.js:130–300`, `:358–374` | Depends on #1, #5, #8. Big win: bots become deterministic and replayable. |
| 13 | Trials checkpoint / lap machine | `main.js:1720–1754`, `:333–343`, `:3286–3298` | Small, self-contained, high test value (lap times are a leaderboard input). Only the localStorage best-lap write stays JS. |
| 14 | Campaign mission machine + boss | `main.js:2675–2870`, `:2295–2314`, `:2742–2761`, `:2631–2674` (turret solve only) | Largest and most DOM-entangled; port last, after all its primitives exist. |

---

## 6. Do not port — stays JS forever

| Area | Where | Why |
|---|---|---|
| Renderer, scene graph, post-processing | `main.js:28–68`, `:3458–3468` | The whole point of the split. |
| All lights / skybox / fog / materials | `main.js:88–111`, `:514–544` | Pure presentation. |
| GLB loading, model cache, colour application | `ship.js:1–92`, `main.js:82–83`,`:175–193`,`:680–700`,`:775–801` | Asset pipeline; async and browser-only. |
| Cameras + cockpit | `camera.js`, `fpcamera.js`, `cockpit.js`, `main.js:394–469`,`:1880–1911` | Read-only consumers of sim state; `camTel` becomes `Frame::hud`. |
| Mesh builders: mothership, airfield, terrain mesh, capital ship, trees, clouds, moon, warp | `mothership.js`, `airfield.js:3–89`, `terrain.js:41–87`, `main.js:2536–2630`, `trees.js`, `clouds.js`, `moon.js`, `warp.js` | Geometry only. Sim needs just `{pos, half_size}` / `{pos, radius}` / a height function. |
| Particles: trails, explosions, beams, missile/flare trails | `trails.js`, `bullets.js:47–73`,`:158–170`, `beams.js`, `missiles.js:143–235`,`:430–482` | Cosmetic; driven by `SimEvent`s. Beams in particular carry **no** collision — the hit is decided by `castWorldRay`. |
| Asteroid spin + hit flash animation | `asteroids.js:95–106`,`:186–197` | Never affects collision (radius is constant). |
| All DOM/HUD | `main.js:573–607`, `:1304–1328`, `:1776–1879`, `:1912–1947`, `:1949–1992`, `:2315–2353`, `:2498–2535`, `:3321–3372`, `:3403–3446` | Browser-only. |
| Audio | `audio.js`, `main.js:484–506`, `:1520–1541` | Web Audio API. |
| Input capture | `input.js`, `touchhud.js`, `main.js:470–483`, `:1131–1152` | Browser events → an `Input` struct at the boundary. |
| localStorage | `main.js:37`,`:168`,`:345`,`:400`,`:485–486`,`:973`,`:1111`,`:1337`,`:1740`,`:2347–2352`,`:2770` | Persistence, not simulation. |
| REST + WebSocket transport | `main.js:753–951` (framing/parse), `:2354–2416`, `auth.js`, `server/index.js:1–350` (hand-rolled WS) | Sim consumes `NetEvent`, emits `NetIntent`; the wire stays JS. |
| Tutorial | `main.js:3029–3173` | A DOM step machine that only *reads* sim state. |
| Lobby | `lobby.js` (1301 lines) | Pre-match UI. |
| `server/db.js` | all | SQLite/auth/credits. |

---

## 7. Top three things to fix while porting

1. **One bullet simulation.** Bug #9 — bot bullets are simulated twice with radii
   4.0 vs 6.5, and neither uses a swept test. Unifying them in Rust is the
   single largest correctness + code-deletion win.
2. **One rules table.** `SHIP_MAX_HP`, respawn delay, damage numbers, asteroid
   tiers and hit radii are spread across `main.js`, `bullets.js`, `missiles.js`,
   `bot.js`, `asteroids.js` and `server/index.js`. Bug #1 (2.5 vs 2.0 s) and #4/#5
   (five and three hit radii) exist purely because there is no single table.
3. **One asteroid generator.** Bug #2 (server rocks spawn inside the moon) is a
   live multiplayer defect caused by the server owning a second, subtly different
   copy of `createAsteroidField`. Compile the Rust generator to WASM and have
   `server/index.js` call it — the field then agrees by construction.
