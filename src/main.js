import * as THREE from 'three';
import { createSkybox } from './skybox.js';
import { createShip, loadShipModel, applyColorsToShip, isModelCached } from './ship.js';
import { createAsteroidField, createAsteroidFieldFromData } from './asteroids.js';
import { createBullets, BULLET_SPEED } from './bullets.js';
import { createMissiles } from './missiles.js';
import { createBeams } from './beams.js';
import { createTrails } from './trails.js';
import { createMothership } from './mothership.js';
import { createMoon } from './moon.js';
import { createAirfield, AIRFIELD_HALF } from './airfield.js';
import { createTerrain, getTerrainHeight, TERRAIN_KILL_CLEARANCE } from './terrain.js';
import { createTrees } from './trees.js';
import { createClouds } from './clouds.js';
import { ThirdPersonCamera } from './camera.js';
import { Input } from './input.js';
import { createAudio } from './audio.js';
import { createBotAI } from './bot.js';
import { createTouchHud } from './touchhud.js';
import { getSavedShipColor, getSavedAccentColor, getSavedTrailColor, getSavedTrailShape } from './customization.js';

// Game entry point. Called by the lobby once the host clicks Start (or a
// non-host receives the `start` broadcast). The `opts.ws` socket is kept
// open for the multiplayer state-sync layer that lands next iteration.
let started = false;
export async function startGame(opts = {}) {
  if (started) return;
  started = true;

  // Preload the regular ship GLB before spawning any ships (same as before).
  try { await loadShipModel(); } catch (e) { console.warn('[ship] GLB load failed, using primitives', e); }

  // Kick off the admin model load in the background — NOT awaited here so
  // the game message handler (which receives friends' color updates) gets
  // registered without delay. We save the promise so getOrCreateRemote can
  // swap the model in-place if it finishes after a remote admin ship was
  // already created with the regular-model fallback.
  const ADMIN_MODEL_URL = 'public/spaceshipADMIN.glb';
  const adminModelReady = loadShipModel(ADMIN_MODEL_URL).catch(() => null);

  const scene = new THREE.Scene();
  const renderer = new THREE.WebGLRenderer({ antialias: true });
  // Cap pixel ratio so a 3× retina display doesn't render 9× the pixels of
  // a 1× display — the visual difference past 1.5 is marginal but the
  // fragment cost scales quadratically. Big win on integrated GPUs.
  renderer.setPixelRatio(Math.min(window.devicePixelRatio, 1.5));
  renderer.setSize(window.innerWidth, window.innerHeight);
  renderer.shadowMap.enabled = true;
  renderer.shadowMap.type = THREE.BasicShadowMap;
  document.body.appendChild(renderer.domElement);

  // Far plane sized to fit the gameplay world (motherships at z=±600 +
  // hangar offset, 400-unit asteroid field, ship visibility cap at
  // 1500). Lower than the original 5000 so the GPU isn't drawing scene
  // objects that are way past anything you'd see during a match.
  const camera = new THREE.PerspectiveCamera(75, window.innerWidth / window.innerHeight, 0.1, 2500);

  // ---- PSX-style pixelated render pipeline ----------------------------
  // Render the 3D scene to a low-res WebGLRenderTarget (1/PIXEL_SCALE on
  // each axis) with NearestFilter, then blit it to the canvas via a
  // fullscreen quad. Less fragment work = perf win on iGPUs, and the
  // nearest-neighbor upscale gives the chunky retro look. DOM HUD stays
  // crisp because it isn't part of the WebGL pipeline.
  const pixelEnabled = localStorage.getItem('spaceships:pixelFilter') !== '0';
  const PIXEL_SCALE = 3;
  const pixelRT = pixelEnabled ? new THREE.WebGLRenderTarget(
    Math.max(1, Math.floor(window.innerWidth / PIXEL_SCALE)),
    Math.max(1, Math.floor(window.innerHeight / PIXEL_SCALE)),
    {
      minFilter: THREE.NearestFilter,
      magFilter: THREE.NearestFilter,
      format: THREE.RGBAFormat,
      depthBuffer: true,
      stencilBuffer: false,
    },
  ) : null;
  const postScene = pixelEnabled ? new THREE.Scene() : null;
  const postCamera = pixelEnabled ? new THREE.OrthographicCamera(-1, 1, 1, -1, 0, 1) : null;
  if (pixelEnabled) {
    const quad = new THREE.Mesh(
      new THREE.PlaneGeometry(2, 2),
      new THREE.MeshBasicMaterial({ map: pixelRT.texture, depthTest: false, depthWrite: false }),
    );
    postScene.add(quad);
  }
  function renderFrame() {
    if (pixelEnabled) {
      renderer.setRenderTarget(pixelRT);
      renderer.render(scene, camera);
      renderer.setRenderTarget(null);
      renderer.render(postScene, postCamera);
    } else {
      renderer.render(scene, camera);
    }
  }

  const MAP_TYPE = opts.map || 'space';
  const isTerrainMap = MAP_TYPE === 'terrain';

  // Bump far plane for terrain map (3× larger world).
  if (isTerrainMap) camera.far = 5000;
  camera.updateProjectionMatrix();

  let terrainSun = null;
  if (isTerrainMap) {
    scene.add(new THREE.AmbientLight(0xfff8e8, 0.60));
    terrainSun = new THREE.DirectionalLight(0xfff5cc, 1.4);
    terrainSun.position.set(0, 500, 0);
    terrainSun.castShadow = true;
    terrainSun.shadow.mapSize.set(1024, 1024);
    terrainSun.shadow.camera.left   = -150;
    terrainSun.shadow.camera.right  =  150;
    terrainSun.shadow.camera.top    =  150;
    terrainSun.shadow.camera.bottom = -150;
    terrainSun.shadow.camera.near   = 1;
    terrainSun.shadow.camera.far    = 700;
    scene.add(terrainSun.target);
    scene.add(terrainSun);
    scene.background = new THREE.Color(0x6fa8d4);
    scene.fog = new THREE.Fog(0xbbd5f0, 1400, 4800);
  } else {
    scene.add(new THREE.AmbientLight(0xffffff, 0.35));
    const sun = new THREE.DirectionalLight(0xffffff, 1.1);
    sun.position.set(200, 300, 100);
    scene.add(sun);
    scene.background = createSkybox();
  }

  const isTrialsMode = !!(opts.solo && opts.mode && opts.mode.startsWith('trials'));

  // Base platforms: motherships for space, airfields for terrain. AABBs kept
  // separately for collision — same world-aligned box approach either way.
  const MOTHERSHIP_HALF = new THREE.Vector3(45, 18, 35);

  let platformA, platformB, platformHalf;
  if (isTerrainMap) {
    platformHalf = AIRFIELD_HALF;
    platformA = createAirfield(0);
    platformA.position.set(0, 0, -1500);
    scene.add(platformA);
    platformB = createAirfield(1);
    platformB.position.set(0, 0, 1500);
    platformB.quaternion.setFromAxisAngle(new THREE.Vector3(0, 1, 0), Math.PI);
    scene.add(platformB);
  } else {
    platformHalf = MOTHERSHIP_HALF;
    platformA = createMothership();
    platformA.position.set(0, 0, -600);
    scene.add(platformA);
    platformB = createMothership();
    platformB.position.set(0, 0, 600);
    platformB.quaternion.setFromAxisAngle(new THREE.Vector3(0, 1, 0), Math.PI);
    scene.add(platformB);
  }

  // Keep legacy name alias so all downstream collision code is unchanged.
  const mothershipA = platformA;
  const mothershipB = platformB;

  const motherships = [
    { pos: platformA.position, halfSize: platformHalf },
    { pos: platformB.position, halfSize: platformHalf },
  ];

  if (isTrialsMode) {
    platformA.visible = false;
    platformB.visible = false;
  }

  // Terrain map: heightmap ground, trees, clouds.
  const terrainMesh = isTerrainMap ? createTerrain() : null;
  if (terrainMesh) {
    terrainMesh.receiveShadow = true;
    scene.add(terrainMesh);
  }
  if (isTerrainMap) createTrees(scene);
  const clouds = isTerrainMap ? createClouds(scene) : null;



  // Indestructible obstacle at the origin (space only).
  const MOON_RADIUS = 80;
  const moon = isTerrainMap ? null : createMoon({ radius: MOON_RADIUS, position: [0, 0, 0] });
  if (moon) scene.add(moon.mesh);
  const obstacles = moon ? [{ pos: moon.pos, radius: MOON_RADIUS }] : [];
  const moonAvoid = moon
    ? { pos: moon.pos, halfSize: new THREE.Vector3(MOON_RADIUS, MOON_RADIUS, MOON_RADIUS) }
    : null;

  const SHIP_SCALE = 1.5;
  const savedHull   = parseInt(getSavedShipColor().replace('#', ''), 16);
  const savedAccent = parseInt(getSavedAccentColor().replace('#', ''), 16);
  // Names that get the admin ship model. Add more here as needed.
  const ADMIN_SHIP_NAMES = new Set(['Admin', 'ariairspeed']);
  const localPlayerName = (opts.pilotName || '').trim();
  const isLocalAdmin = ADMIN_SHIP_NAMES.has(localPlayerName);
  const ship = createShip({
    hullColor: savedHull,
    accentColor: savedAccent,
    modelUrl: isLocalAdmin ? ADMIN_MODEL_URL : 'public/spaceship.glb',
    doubleSided: isLocalAdmin,
  });
  // If we're admin but the model wasn't cached yet (still downloading),
  // swap the geometry in-place once it finishes — same pattern as remotes.
  if (isLocalAdmin && !isModelCached(ADMIN_MODEL_URL)) {
    adminModelReady.then((adminScene) => {
      if (!adminScene) return;
      ship.children.slice().forEach((c) => ship.remove(c));
      const newModel = adminScene.clone(true);
      newModel.rotation.y = -Math.PI / 2;
      newModel.traverse((o) => {
        if (o.isMesh && o.material) {
          o.material = o.material.clone();
          o.material.side = THREE.DoubleSide;
        }
      });
      ship.add(newModel);
      applyColorsToShip(ship, savedHull, savedAccent);
    });
  }
  ship.scale.setScalar(SHIP_SCALE);
  // Apply server-provided spawn (team-specific) or fall back to mothership A.
  if (opts.spawn) {
    ship.position.fromArray(opts.spawn.pos);
    ship.quaternion.fromArray(opts.spawn.quat);
  } else if (isTrialsMode) {
    ship.position.set(0, 20, -510);
  } else if (isTerrainMap) {
    ship.position.set(0, 40, -1400);
  } else {
    ship.position.set(0, 0, -540);
  }
  scene.add(ship);
  if (isTerrainMap) ship.traverse((o) => { if (o.isMesh) o.castShadow = true; });

  // Prefer the server-authoritative field if it was sent in the start
  // message; fall back to local random generation for offline runs.
  const _trialRockCount = opts.mode === 'trials4' ? 210
    : opts.mode === 'trials3' ? 180
    : opts.mode === 'trials2' ? 150
    : isTrialsMode ? 120 : 60;
  const _avoidList = moonAvoid ? [...motherships, moonAvoid] : [...motherships];
  const asteroids = isTerrainMap
    ? createAsteroidFieldFromData([])
    : (opts.asteroids
      ? createAsteroidFieldFromData(opts.asteroids)
      : createAsteroidField({ count: _trialRockCount, radius: 400, avoid: _avoidList }));
  scene.add(asteroids.group);

  // ── Time Trials checkpoint data ─────────────────────────────────────────
  // Each trial has its own set of checkpoint positions. Rings are torus
  // meshes oriented perpendicular to the path. Timer begins on the first
  // crossing of CP0 (start/finish); each full circuit records a lap time.
  const TRIAL1_CPS = [
    new THREE.Vector3(   0,  20, -380),  // CP0  start / finish
    new THREE.Vector3( 180,  60, -260),  // CP1  climb right
    new THREE.Vector3( 340,   0,  -80),  // CP2  east entry
    new THREE.Vector3( 360, -50,  120),  // CP3  east exit
    new THREE.Vector3( 220,  80,  280),  // CP4  back-right high
    new THREE.Vector3(  60, -60,  370),  // CP5  back centre low
    new THREE.Vector3(-150,  40,  360),  // CP6  back left
    new THREE.Vector3(-320, -40,  180),  // CP7  west entry
    new THREE.Vector3(-370,  60,  -60),  // CP8  west exit
    new THREE.Vector3(-260, -80, -240),  // CP9  south-west deep
    new THREE.Vector3(-100,  30, -360),  // CP10 approach left
    new THREE.Vector3( 100, -40, -350),  // CP11 final approach
  ];
  // Trial 2 — 14 CPs, tighter turns, closes in on the moon
  const TRIAL2_CPS = [
    new THREE.Vector3(   0,  20, -360),  // CP0  start
    new THREE.Vector3( 160,  80, -220),  // CP1
    new THREE.Vector3( 290, -40,  -80),  // CP2  tighter east entry
    new THREE.Vector3( 310, -80,  100),  // CP3
    new THREE.Vector3( 190, 100,  270),  // CP4
    new THREE.Vector3(  40, -90,  330),  // CP5  low back
    new THREE.Vector3(-120,  70,  310),  // CP6
    new THREE.Vector3(-270, -60,  190),  // CP7
    new THREE.Vector3(-300,  90,   20),  // CP8  close west pass
    new THREE.Vector3(-270,-100, -170),  // CP9
    new THREE.Vector3(-120,  60, -310),  // CP10
    new THREE.Vector3(  20, -80, -310),  // CP11
    new THREE.Vector3( 140,  90, -240),  // CP12
    new THREE.Vector3( 260, -60, -120),  // CP13 final
  ];
  // Trial 3 — 16 CPs, extreme height variation, very tight
  const TRIAL3_CPS = [
    new THREE.Vector3(   0, -30, -370),  // CP0  start
    new THREE.Vector3( 150, 100, -240),  // CP1  climb
    new THREE.Vector3( 300, -80,  -60),  // CP2  dive
    new THREE.Vector3( 350, 100,  120),  // CP3  climb
    new THREE.Vector3( 220,-110,  280),  // CP4  deep dive
    new THREE.Vector3(  60, 100,  350),  // CP5  high climb
    new THREE.Vector3( -80,-110,  300),  // CP6  deep dive
    new THREE.Vector3(-240, 100,  160),  // CP7  climb
    new THREE.Vector3(-330, -90,    0),  // CP8  close left of moon
    new THREE.Vector3(-260, 110, -180),  // CP9  climb
    new THREE.Vector3(-120,-100, -290),  // CP10 dive
    new THREE.Vector3(  20, 110, -350),  // CP11 climb
    new THREE.Vector3( 170,-100, -250),  // CP12 dive
    new THREE.Vector3( 310, 100,  -70),  // CP13 climb
    new THREE.Vector3( 220,-110,  120),  // CP14 dive
    new THREE.Vector3(  80,  80, -200),  // CP15 final approach
  ];
  // Trial 4 — 18 CPs, closest moon passes, maximum difficulty
  const TRIAL4_CPS = [
    new THREE.Vector3(   0,  50, -370),  // CP0  start
    new THREE.Vector3( 180,-100, -210),  // CP1
    new THREE.Vector3( 340, 110,  -40),  // CP2
    new THREE.Vector3( 210,-110,  240),  // CP3
    new THREE.Vector3(  40, 110,  340),  // CP4
    new THREE.Vector3(-180,-110,  210),  // CP5
    new THREE.Vector3(-160,  80,    0),  // CP6  close left of moon
    new THREE.Vector3(-200,-100, -210),  // CP7
    new THREE.Vector3(   0, 110, -180),  // CP8  above front of moon
    new THREE.Vector3( 200,-100,  -40),  // CP9
    new THREE.Vector3( 300, 100,  180),  // CP10
    new THREE.Vector3(  80,-110,  320),  // CP11
    new THREE.Vector3(-200, 100,  180),  // CP12
    new THREE.Vector3(-320,-100,  -40),  // CP13
    new THREE.Vector3(-200, 100, -220),  // CP14
    new THREE.Vector3(   0,-110, -340),  // CP15 south close
    new THREE.Vector3( 200, 100, -220),  // CP16
    new THREE.Vector3( 100, -80, -330),  // CP17 final
  ];
  const TRIAL_CPS = opts.mode === 'trials4' ? TRIAL4_CPS
    : opts.mode === 'trials3' ? TRIAL3_CPS
    : opts.mode === 'trials2' ? TRIAL2_CPS
    : TRIAL1_CPS;
  const TRIAL_BEST_KEY = opts.mode === 'trials4' ? 'spaceships:trial4Best'
    : opts.mode === 'trials3' ? 'spaceships:trial3Best'
    : opts.mode === 'trials2' ? 'spaceships:trial2Best'
    : 'spaceships:trial1Best';
  const TRIAL_NUM = opts.mode === 'trials4' ? 4 : opts.mode === 'trials3' ? 3 : opts.mode === 'trials2' ? 2 : 1;
  const CP_TRIGGER_DIST = 55;
  const cpMeshes = [];
  const tracerDots = [];
  let trialsNextCp = 0;
  let trialsTimer = 0;
  let trialsRunning = false;
  let trialsBestLap = null;
  let trialsLastLap = null;
  let trialsLap = 0;
  let cpCooldown = 0;
  let trialsCountdown = 0;
  let trialsCountdownActive = false;

  if (isTrialsMode) {
    const savedBest = parseFloat(localStorage.getItem(TRIAL_BEST_KEY));
    if (!isNaN(savedBest)) trialsBestLap = savedBest;

    cpCooldown = 1.5; // prevent CP0 triggering the instant the countdown ends

    const cpGeo = new THREE.TorusGeometry(48, 3.5, 8, 36);
    for (let i = 0; i < TRIAL_CPS.length; i++) {
      const isNext = i === 0;
      const mat = new THREE.MeshBasicMaterial({
        color: isNext ? 0x66ffcc : 0x224466,
        transparent: true,
        opacity: isNext ? 0.85 : 0.35,
        side: THREE.DoubleSide,
      });
      const mesh = new THREE.Mesh(cpGeo, mat);
      mesh.position.copy(TRIAL_CPS[i]);
      const nextIdx = (i + 1) % TRIAL_CPS.length;
      const pathDir = TRIAL_CPS[nextIdx].clone().sub(TRIAL_CPS[i]).normalize();
      mesh.quaternion.setFromUnitVectors(new THREE.Vector3(0, 0, 1), pathDir);
      scene.add(mesh);
      cpMeshes.push(mesh);
    }

    // Tracer dots: small glowing spheres that flow from the ship toward the
    // next checkpoint so the player always knows where to go.
    const dotGeo = new THREE.SphereGeometry(2.5, 5, 5);
    for (let i = 0; i < 10; i++) {
      const dotMat = new THREE.MeshBasicMaterial({ color: 0x66ffcc, transparent: true, opacity: 0.7 });
      const dot = new THREE.Mesh(dotGeo, dotMat);
      dot.visible = false;
      scene.add(dot);
      tracerDots.push(dot);
    }

    trialsCountdown = 3.0;
    trialsCountdownActive = true;
    const _cdWrap = document.getElementById('trials-countdown');
    const _cdNum  = document.getElementById('trials-countdown-num');
    if (_cdWrap) _cdWrap.style.display = 'flex';
    if (_cdNum)  { _cdNum.textContent = '3'; _cdNum.style.color = '#ff5566'; }
  }

  // Bullet hit-sphere is generous in both modes — mouse 6.0 (slight
  // forgiveness, lead correction still has to be roughly right), keys /
  // mobile 7.0 (more forgiveness, since digital + thumb input can't
  // pixel-aim). Decided up-front so we can size the sphere once.
  const coarseAim = !!opts.noMouse || opts.controlScheme === 'keyboard' || opts.controlScheme === 'mobile';
  const bullets = createBullets({ shipHitRadius: coarseAim ? 7.0 : 6.0 });
  scene.add(bullets.group);

  const beams = createBeams();
  scene.add(beams.group);

  const missileSystem = createMissiles();
  scene.add(missileSystem.group);

  const trails = createTrails();
  scene.add(trails.group);

  const tpCam = new ThirdPersonCamera(camera, ship);
  tpCam.snap();
  const input = new Input(renderer.domElement);
  // Control scheme (set in the lobby): 'mouse_keys' (default), 'keyboard'
  // (arrow steering, no mouse), or 'mobile' (touch joystick + on-screen
  // buttons).
  const controlScheme = opts.controlScheme
    || (opts.noMouse ? 'keyboard' : 'mouse_keys');
  const noMouseMode = controlScheme === 'keyboard';
  const isMobileScheme = controlScheme === 'mobile';
  // Mobile also disables real mouse handling — browsers synthesize
  // mousedown/mousemove from touches, and we don't want those bleeding
  // into the steering layer when the joystick is already driving it.
  input.mouseDisabled = noMouseMode || isMobileScheme;
  input.touchEnabled = isMobileScheme;
  // Arrow-keys mode: hide the cursor on game start, let Escape toggle it
  // back on (so the player can reach the settings gear without leaving
  // the match). Setting hides at the document level via a body class.
  if (noMouseMode) {
    document.body.classList.add('mouse-hidden');
    window.addEventListener('keydown', (e) => {
      if (e.code === 'Escape') document.body.classList.toggle('mouse-hidden');
    });
  }
  // On-screen control overlay (joystick + buttons) for mobile. Returns
  // no-op stubs for the other schemes so callers don't need to branch.
  const touchHud = createTouchHud({ input, scheme: controlScheme });
  const audio = createAudio();
  // Live volume from the settings panel: read the saved values (defaults
  // music=0.6, sfx=1.0) and apply, then expose `audio` globally so the
  // lobby's slider change handlers can adjust mid-game.
  const savedMusic = parseFloat(localStorage.getItem('spaceships:musicVolume'));
  const savedSfx = parseFloat(localStorage.getItem('spaceships:sfxVolume'));
  audio.setMusicVolume(Number.isFinite(savedMusic) ? savedMusic : 0.6);
  audio.setSfxVolume(Number.isFinite(savedSfx) ? savedSfx : 1.0);
  window.__shipAudio = audio;

  const ZERO_VEC = new THREE.Vector3();

  // Quadratic distance falloff for environmental SFX (rockbreak, shipdeath).
  // Within NEAR_DIST: full volume. Beyond FAR_DIST: silent. Smooth in between.
  const SFX_NEAR_DIST = 80;
  const SFX_FAR_DIST = 900;
  function distanceVol(pos) {
    const d = ship.position.distanceTo(pos);
    if (d <= SFX_NEAR_DIST) return 1.0;
    if (d >= SFX_FAR_DIST) return 0;
    const u = 1 - (d - SFX_NEAR_DIST) / (SFX_FAR_DIST - SFX_NEAR_DIST);
    return u * u;
  }

  // Engine audio mixer state. Move and boost loops crossfade based on speed
  // and boost/brake state. Volumes are smoothed toward a target each frame.
  const MOVE_MAX_VOL = 0.25;
  const BOOST_MAX_VOL = 0.4;
  const SPEED_FOR_FULL_VOL = 80; // u/s — at full throttle move loop hits peak
  const MOVE_DUCK_BOOST = 0.25;  // multiplier on move volume while boosting
  const MOVE_DUCK_BRAKE = 0.4;   // multiplier on move volume while airbraking
  let moveVol = 0;
  let boostVol = 0;

  // --- Multiplayer ---
  const ws = opts.ws;
  const myId = opts.you;
  const isSolo = !!opts.solo;
  const remotePlayers = new Map();
  const remoteColors = new Map(); // id -> { hullColor, accentColor } as hex integers
  const remoteModels = new Map(); // id -> modelUrl (set by 'ship-model' broadcast)
  const PALETTE = [0xff5577, 0x55ff88, 0xffcc55, 0xaa66ff, 0x55ddff, 0xff99cc, 0xff8833, 0x99ff55];

  // Marker diamond textures: red for enemies, green for teammates. Shared
  // across all remote ships; a per-record material is picked from r.team
  // vs myTeam. Diamond shape reads as more "HUD marker" than a plain dot.
  function makeDotTexture(fill) {
    const c = document.createElement('canvas');
    c.width = c.height = 32;
    const ctx = c.getContext('2d');
    ctx.fillStyle = fill;
    ctx.beginPath();
    ctx.moveTo(16, 2);   // top
    ctx.lineTo(30, 16);  // right
    ctx.lineTo(16, 30);  // bottom
    ctx.lineTo(2, 16);   // left
    ctx.closePath();
    ctx.fill();
    const t = new THREE.CanvasTexture(c);
    t.needsUpdate = true;
    return t;
  }
  const enemyMarkerMat = new THREE.SpriteMaterial({
    map: makeDotTexture('#ff2030'),
    sizeAttenuation: false, depthTest: false, transparent: true,
  });
  const allyMarkerMat = new THREE.SpriteMaterial({
    map: makeDotTexture('#30ff70'),
    sizeAttenuation: false, depthTest: false, transparent: true,
  });
  function pickMarkerMat(team) {
    return (team !== null && team !== undefined && team === myTeam)
      ? allyMarkerMat : enemyMarkerMat;
  }
  function refreshMarker(r) {
    if (r && r.marker) r.marker.material = pickMarkerMat(r.team);
  }

  const SHIP_MAX_HP = 100;
  const RESPAWN_DELAY = 2.5;
  const SPAWN_INVULN_DURATION = 2.0;
  let myHp = SHIP_MAX_HP;
  let myAlive = true;
  let myRespawnTimer = 0;
  let myInvulnTimer = SPAWN_INVULN_DURATION; // protected at game start too

  // Scoreboard: id → { name, kills, deaths }. Multiplayer fills from server
  // 'players' messages; solo seeds with the local pilot + a stub Bot entry
  // and updates locally on hits.
  const scores = new Map();
  if (Array.isArray(opts.players)) {
    for (const p of opts.players) {
      scores.set(p.id, {
        name: p.name,
        team: p.team ?? null,
        kills: p.kills || 0,
        deaths: p.deaths || 0,
      });
    }
  }
  // Always ensure the local player has the correct display name in scores.
  // In solo mode this is the only population; in multiplayer it may already
  // exist from opts.players but we want to prefer opts.pilotName (the
  // client-side callsign) and preserve any team/kill/death data already set.
  {
    const existing = scores.get(opts.you);
    scores.set(opts.you, {
      name: opts.pilotName || existing?.name || 'Pilot',
      team: existing?.team ?? null,
      kills: existing?.kills || 0,
      deaths: existing?.deaths || 0,
    });
  }
  if (isSolo) {
    // Bot entries are seeded by spawnBot() when the solo mode is wired below.
  }
  const scoreboardEl = document.getElementById('scoreboard');
  const scoreboardBody = document.getElementById('scoreboard-body');
  function renderScoreboard() {
    if (!scoreboardBody) return;
    const rows = [...scores.entries()]
      .map(([id, s]) => ({ id, ...s }))
      .sort((a, b) => {
        // Group by team first (0 before 1 before null), then kills desc, deaths asc
        const ta = a.team ?? 99, tb = b.team ?? 99;
        if (ta !== tb) return ta - tb;
        return b.kills - a.kills || a.deaths - b.deaths;
      });

    const hasTeams = rows.some(r => r.team !== null && r.team !== undefined);
    scoreboardBody.innerHTML = '';
    let lastTeam = undefined;

    for (const r of rows) {
      // Insert a team-header divider row when the team changes
      if (hasTeams && r.team !== lastTeam) {
        const header = document.createElement('tr');
        header.className = `sb-team-header t${r.team ?? 'x'}`;
        const label = r.team === 0 ? 'TEAM BLUE' : r.team === 1 ? 'TEAM RED' : 'NO TEAM';
        header.innerHTML = `<td colspan="3">${label}</td>`;
        scoreboardBody.appendChild(header);
        lastTeam = r.team;
      }

      const tr = document.createElement('tr');
      const classes = [];
      if (r.id === myId) classes.push('you');
      if (r.team === 0) classes.push('team0');
      if (r.team === 1) classes.push('team1');
      if (classes.length) tr.className = classes.join(' ');

      tr.innerHTML = `<td></td><td class="num">${r.kills}</td><td class="num">${r.deaths}</td>`;
      tr.children[0].textContent = r.name + (r.id === myId ? ' (you)' : '');
      scoreboardBody.appendChild(tr);
    }
  }
  renderScoreboard();

  // Returns smallest positive intercept time, or null if no real solution.
  // Working in the shooter's rest frame: bullet flies at speed `s` along
  // some forward direction; target sits at relative position R, drifting at
  // relative velocity U. We need t such that |R + U·t| = s·t.
  function solveIntercept(enemyPos, enemyVel, selfPos, selfVel, s) {
    const Rx = enemyPos.x - selfPos.x, Ry = enemyPos.y - selfPos.y, Rz = enemyPos.z - selfPos.z;
    const Ux = enemyVel.x - selfVel.x, Uy = enemyVel.y - selfVel.y, Uz = enemyVel.z - selfVel.z;
    const RR = Rx * Rx + Ry * Ry + Rz * Rz;
    const RU = Rx * Ux + Ry * Uy + Rz * Uz;
    const UU = Ux * Ux + Uy * Uy + Uz * Uz;
    const a = UU - s * s;
    const b = 2 * RU;
    const c = RR;
    if (Math.abs(a) < 1e-6) {
      if (Math.abs(b) < 1e-6) return null;
      const t = -c / b;
      return t > 0 ? t : null;
    }
    const disc = b * b - 4 * a * c;
    if (disc < 0) return null;
    const sd = Math.sqrt(disc);
    const t1 = (-b - sd) / (2 * a);
    const t2 = (-b + sd) / (2 * a);
    let t = Infinity;
    if (t1 > 0) t = Math.min(t, t1);
    if (t2 > 0) t = Math.min(t, t2);
    return Number.isFinite(t) ? t : null;
  }

  function getOrCreateRemote(id) {
    let r = remotePlayers.get(id);
    if (r) return r;
    const colors = remoteColors.get(id);
    const remoteName = (scores.get(id)?.name || '').trim();
    const isRemoteAdmin = remoteModels.get(id) === ADMIN_MODEL_URL || ADMIN_SHIP_NAMES.has(remoteName);
    const remoteModelUrl = isRemoteAdmin ? ADMIN_MODEL_URL : 'public/spaceship.glb';
    const remoteShip = colors
      ? createShip({ hullColor: colors.hullColor, accentColor: colors.accentColor, modelUrl: remoteModelUrl, doubleSided: isRemoteAdmin })
      : createShip({ tint: PALETTE[id % PALETTE.length], modelUrl: remoteModelUrl, doubleSided: isRemoteAdmin });
    remoteShip.scale.setScalar(SHIP_SCALE);

    // Constant-size dot above the ship. Red for enemies, green for allies.
    // sizeAttenuation: false keeps it a fixed fraction of the viewport.
    const teamHint = scores.get(id)?.team ?? null;
    const marker = new THREE.Sprite(pickMarkerMat(teamHint));
    marker.scale.set(0.011, 0.011, 1);
    marker.position.y = 1.6;
    marker.renderOrder = 999;
    remoteShip.add(marker);

    // Targeting overlays: bracket box + lead indicator + label.
    const box = document.createElement('div');
    box.className = 'target-box';
    box.style.display = 'none';
    const label = document.createElement('div');
    label.className = 'target-label';
    box.appendChild(label);
    document.body.appendChild(box);

    const lead = document.createElement('div');
    lead.className = 'lead-marker';
    lead.style.display = 'none';
    document.body.appendChild(lead);

    scene.add(remoteShip);
    if (isTerrainMap) remoteShip.traverse((o) => { if (o.isMesh) o.castShadow = true; });
    r = {
      id,
      ship: remoteShip,
      trailOffsets: isRemoteAdmin ? ADMIN_TRAIL_OFFSETS : TRAIL_OFFSETS,
      targetPos: new THREE.Vector3(),
      targetQuat: new THREE.Quaternion(),
      hasTarget: false,
      alive: true,
      hp: SHIP_MAX_HP,
      // Hit flash intensity. Set to 1 whenever HP drops, decays each
      // frame to 0 — drives emissive on every mesh under r.ship.
      hitFlash: 0,
      marker,
      box, label, lead,
      vel: new THREE.Vector3(),
      lastStateTime: 0,
      lastStatePos: new THREE.Vector3(),
      team: scores.get(id)?.team ?? null,
    };
    remotePlayers.set(id, r);

    // If this is an admin player but the admin model wasn't cached yet,
    // swap the ship's 3D model in-place once the download finishes.
    if (isRemoteAdmin && !isModelCached(ADMIN_MODEL_URL)) {
      adminModelReady.then((adminScene) => {
        const rec = remotePlayers.get(id);
        if (!rec || !adminScene) return;
        // Remove non-marker children (the old placeholder model).
        rec.ship.children.slice().forEach((c) => {
          if (c !== rec.marker) rec.ship.remove(c);
        });
        const newModel = adminScene.clone(true);
        newModel.rotation.y = -Math.PI / 2;
        newModel.traverse((o) => {
          if (o.isMesh && o.material) {
            o.material = o.material.clone();
            o.material.side = THREE.DoubleSide;
          }
        });
        rec.ship.add(newModel);
        rec.trailOffsets = ADMIN_TRAIL_OFFSETS;
        const col = remoteColors.get(id);
        if (col) applyColorsToShip(rec.ship, col.hullColor, col.accentColor);
      });
    }

    return r;
  }

  function explodeAt(pos, scale) {
    bullets.spawnExplosion(pos, scale);
  }

  function killRemote(id) {
    const r = remotePlayers.get(id);
    if (!r) return;
    r.alive = false;
    explodeAt(r.ship.position, 6);
    r.ship.visible = false;
  }

  function reviveRemote(id, pos, quat) {
    const r = getOrCreateRemote(id);
    r.alive = true;
    r.hp = SHIP_MAX_HP;
    r.targetPos.fromArray(pos);
    r.targetQuat.fromArray(quat);
    r.ship.position.copy(r.targetPos);
    r.ship.quaternion.copy(r.targetQuat);
    r.ship.visible = true;
  }

  function killSelf() {
    if (!myAlive) return;
    myAlive = false;
    explodeAt(ship.position, 6);
    ship.visible = false;
    shipVelocity.set(0, 0, 0);
  }

  function reviveSelf(pos, quat) {
    myAlive = true;
    myHp = SHIP_MAX_HP;
    missilesLeft = MISSILE_MAX;
    ship.position.fromArray(pos);
    ship.quaternion.fromArray(quat);
    shipVelocity.set(0, 0, 0);
    targetThrottle = 0;
    throttle = 0;
    ship.visible = true;
    tpCam.snap();
  }

  function removeRemote(id) {
    const r = remotePlayers.get(id);
    if (!r) return;
    scene.remove(r.ship);
    r.ship.traverse((o) => { if (o.material) o.material.dispose?.(); });
    if (r.box) r.box.remove();
    if (r.lead) r.lead.remove();
    remotePlayers.delete(id);
  }

  if (ws && ws.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify({
      type: 'colors',
      hullColor:   parseInt(getSavedShipColor().replace('#', ''), 16),
      accentColor: parseInt(getSavedAccentColor().replace('#', ''), 16),
    }));
    if (isLocalAdmin) {
      ws.send(JSON.stringify({ type: 'ship-model', modelUrl: ADMIN_MODEL_URL }));
    }
  }

  if (ws) {
    ws.addEventListener('message', (e) => {
      let msg;
      try { msg = JSON.parse(e.data); } catch { return; }
      if (msg.type === 'colors' && msg.id !== myId) {
        const hull   = typeof msg.hullColor   === 'number' ? msg.hullColor   : parseInt(String(msg.hullColor).replace('#', ''), 16);
        const accent = typeof msg.accentColor === 'number' ? msg.accentColor : parseInt(String(msg.accentColor).replace('#', ''), 16);
        remoteColors.set(msg.id, { hullColor: hull, accentColor: accent });
        const r = remotePlayers.get(msg.id);
        if (r) applyColorsToShip(r.ship, hull, accent);
        return;
      }
      if (msg.type === 'ship-model' && msg.id !== myId) {
        remoteModels.set(msg.id, msg.modelUrl);
        const isAdminModel = msg.modelUrl === ADMIN_MODEL_URL;
        const r = remotePlayers.get(msg.id);
        if (r && isAdminModel) {
          // Ship already exists with wrong model — swap to admin model now.
          adminModelReady.then((adminScene) => {
            const rec = remotePlayers.get(msg.id);
            if (!rec || !adminScene) return;
            rec.ship.children.slice().forEach((c) => {
              if (c !== rec.marker) rec.ship.remove(c);
            });
            const newModel = adminScene.clone(true);
            newModel.rotation.y = -Math.PI / 2;
            newModel.traverse((o) => {
              if (o.isMesh && o.material) {
                o.material = o.material.clone();
                o.material.side = THREE.DoubleSide;
              }
            });
            rec.ship.add(newModel);
            rec.trailOffsets = ADMIN_TRAIL_OFFSETS;
            const col = remoteColors.get(msg.id);
            if (col) applyColorsToShip(rec.ship, col.hullColor, col.accentColor);
          });
        }
        return;
      }
      if (msg.type === 'state' && msg.id !== myId) {
        const r = getOrCreateRemote(msg.id);
        if (!r.alive) return;
        const newPos = new THREE.Vector3().fromArray(msg.pos);
        const now = performance.now() / 1000;
        // Differentiate successive state messages to estimate velocity, with
        // exponential smoothing (alpha=0.45) so the lead reticle doesn't
        // twitch on each network update.
        if (r.lastStateTime > 0) {
          const dtState = now - r.lastStateTime;
          if (dtState > 0.005 && dtState < 0.5) {
            const measured = newPos.clone().sub(r.lastStatePos).divideScalar(dtState);
            if (r.velSeeded) {
              r.vel.lerp(measured, 0.45);
            } else {
              r.vel.copy(measured);
              r.velSeeded = true;
            }
          }
        }
        r.lastStateTime = now;
        r.lastStatePos.copy(newPos);
        r.targetPos.copy(newPos);
        r.targetQuat.fromArray(msg.quat);
        r.boost = !!msg.boost;
        if (!r.hasTarget) {
          r.ship.position.copy(r.targetPos);
          r.ship.quaternion.copy(r.targetQuat);
          r.hasTarget = true;
        }
      } else if (msg.type === 'disconnect') {
        removeRemote(msg.id);
        scores.delete(msg.id);
        renderScoreboard();
      } else if (msg.type === 'players') {
        for (const p of msg.players) {
          scores.set(p.id, {
            name: p.name,
            team: p.team ?? null,
            kills: p.kills || 0,
            deaths: p.deaths || 0,
          });
          if (p.team !== null && p.team !== undefined) {
            const r = remotePlayers.get(p.id);
            if (r) {
              r.team = p.team;
              refreshMarker(r);
            }
          }
        }
        renderScoreboard();
      } else if (msg.type === 'match-state') {
        matchTimer = msg.timer;
        if (Array.isArray(msg.teamKills)) {
          teamKills[0] = msg.teamKills[0] || 0;
          teamKills[1] = msg.teamKills[1] || 0;
        }
        renderMatchHud();
      } else if (msg.type === 'match-end') {
        if (Array.isArray(msg.teamKills)) {
          teamKills[0] = msg.teamKills[0] || 0;
          teamKills[1] = msg.teamKills[1] || 0;
        }
        endMatch();
      } else if (msg.type === 'hp') {
        if (msg.id === myId) { if (msg.hp < myHp) healthIdleDamage = 0; myHp = msg.hp; }
        else {
          const r = remotePlayers.get(msg.id);
          if (r) {
            // HP drop = trigger hit flash before overwriting.
            if (msg.hp < r.hp) r.hitFlash = 1;
            r.hp = msg.hp;
          }
        }
      } else if (msg.type === 'death') {
        let deathPos = ship.position;
        if (msg.id !== myId) {
          const r = remotePlayers.get(msg.id);
          if (r) deathPos = r.ship.position;
        }
        audio.play('shipdeath', distanceVol(deathPos));
        if (msg.id === myId) killSelf();
        else killRemote(msg.id);
        if (msg.killerId != null) {
          const kn = scores.get(msg.killerId)?.name
                  || (msg.killerId === myId ? (opts.pilotName || 'Pilot') : 'Pilot');
          const vn = scores.get(msg.id)?.name
                  || (msg.id === myId ? (opts.pilotName || 'Pilot') : 'Pilot');
          pushKillFeed(kn, vn, msg.killerId === myId, msg.id === myId);
        }
      } else if (msg.type === 'respawn') {
        if (msg.id === myId) {
          myInvulnTimer = SPAWN_INVULN_DURATION;
          reviveSelf(msg.pos, msg.quat);
        } else {
          reviveRemote(msg.id, msg.pos, msg.quat);
        }
      } else if (msg.type === 'fire' && msg.id !== myId) {
        const shooterTeam = scores.get(msg.id)?.team ?? null;
        const faction = (shooterTeam !== null && shooterTeam === myTeam) ? 'ally' : 'enemy';
        if (msg.kind === 'beam') {
          for (const shot of msg.shots) {
            const origin = new THREE.Vector3().fromArray(shot.pos);
            const end = new THREE.Vector3().fromArray(shot.end);
            beams.fire(origin, end, faction);
          }
        } else {
          for (const shot of msg.shots) {
            const origin = new THREE.Vector3().fromArray(shot.pos);
            const dir = new THREE.Vector3().fromArray(shot.dir);
            bullets.fire(origin, dir, faction);
          }
        }
        // Play one shoot sound per volley, attenuated by shooter distance.
        if (msg.shots.length > 0) {
          const o = msg.shots[0].pos;
          audio.play('shoot', distanceVol(new THREE.Vector3(o[0], o[1], o[2])));
        }
      } else if (msg.type === 'match-credits') {
        updateCachedCredits(msg.totalCredits);
        if (Array.isArray(msg.earned) && msg.earned.length) {
          queueAchievementToasts(msg.earned);
          stashAchievementsForHangar(msg.earned);
        }
      } else if (msg.type === 'asteroid-hp') {
        if (asteroids.setHp) asteroids.setHp(msg.id, msg.hp);
      } else if (msg.type === 'asteroid-destroyed') {
        if (asteroids.destroy) {
          const a = asteroids.destroy(msg.id);
          if (a) {
            bullets.spawnExplosion(a.mesh.position, a.radius);
            audio.play('rockbreak', distanceVol(a.mesh.position));
          }
        }
      }
    });
  }

  const STATE_INTERVAL = 1 / 20;
  let stateTimer = 0;

  const shipRadius = 2.2 * SHIP_SCALE;
  const shipVelocity = new THREE.Vector3();

  const MAX_THROTTLE = 80;
  const BOOST_FACTOR = 1.7;
  const THROTTLE_STEP = 6;
  const KEY_THROTTLE_RATE = 30;
  const PITCH_RATE = 1.75;
  const PITCH_UP_BOOST = 1.25;
  const YAW_RATE = 1.3;
  const ROLL_RATE = 1.4;
  const VELOCITY_BLEND = 4;
  const STEER_DEADZONE = 0.05;
  // Arrow-key ramp: asymmetric so quick taps give micro-corrections.
  //   Press → slow ramp up (~0.5s to full deflection): a brief tap only
  //           pushes a small fraction of input.
  //   Press + hold Q → fine-aim mode, much slower ramp so 50ms taps
  //           barely deflect at all (good for crosshair micro-adjusts).
  //   Release → fast decay (~0.1s back to neutral): no input lingers
  //           after key-up, so repeated taps stay tappy.
  let arrowKx = 0, arrowKy = 0;
  const ARROW_RAMP_UP_RATE = 3;
  const ARROW_RAMP_UP_RATE_FINE = 1.5;
  const ARROW_RAMP_DOWN_RATE = 12;

  // Aim assist: when an enemy is in the forward cone, rotate the ship
  // gently toward them. Press C to toggle in-game (persisted to
  // localStorage). In no-mouse mode it's forced on at the stronger
  // profile since arrow-key aiming is coarser.
  // Coarse-aim schemes (keyboard arrows + mobile thumbstick) get the
  // assist forced on and run with the wider/stronger profile below.
  let aimAssistEnabled = coarseAim
    ? true
    : localStorage.getItem('spaceships:aimAssist') === '1';
  let prevKeyC = false;
  const ASSIST_CONE_DOT = coarseAim ? 0.5 : 0.60;          // 60° vs 53° cone
  // Engagement caps. Autoaim turns off past this distance, matching
  // the targeting computer's pickup range so you can only get help on
  // enemies you can actually see the box on.
  const ASSIST_MIN_RANGE = 0;
  const ASSIST_RANGE = 1000;
  // The overhead team-color diamond shows from further out — it's the
  // low-detail "there's someone over there" indicator before the full
  // target box appears at closer range.
  const MARKER_VISIBLE_DIST = 1500;
  // Pull profile: strong while swinging onto target (helps newcomers and
  // arrow-key pilots track), zero once the crosshair is on. Strong far,
  // weak near = "guide, don't lock."
  const ASSIST_STRENGTH = coarseAim ? 2.2 : 2.6;           // max rad/sec
  const ASSIST_FALLOFF_START = coarseAim ? 0.30 : 0.28;    // ~17° vs ~16°
  // Both modes pull all the way to the lead point now — mouse used to
  // keep a small dead-zone to leave cursor freedom, but the intent
  // damper (ASSIST_INTENT_BREAK) handles "I want to aim manually" via
  // cursor velocity, so the dead-zone just hurt the lock accuracy.
  const ASSIST_DEAD_ANGLE = coarseAim ? 0.0 : 0.005;
  // Once a target is acquired, give it a small dot-bonus on subsequent
  // frames so the assist doesn't flicker between two equidistant enemies
  // and stays committed to the one you're already swinging onto.
  const ASSIST_STICKY_DOT_BONUS = 0.05;
  // Stick-intent break: pull strength scales down with steering magnitude
  // so deliberate input slips the lock. Tuned per input device:
  //   - Keys: low threshold (0.25) — any sustained press releases easily,
  //     so you aren't auto-locked when you're trying to evade or rotate.
  //   - Mouse: high threshold (1.8) — fine cursor jitter from aiming
  //     doesn't kill the assist; only a deliberate full-deflection swing
  //     does. Lets the mouse magnetism actually help during tracking.
  const ASSIST_INTENT_BREAK = coarseAim ? 0.25 : 1.8;

  // Hold-Space drift: decouples orientation from velocity. The ship keeps
  // its current momentum vector unchanged while you rotate freely, then
  // on release re-engages thrust along the new facing — a kart-style
  // power-slide that lets you spin to fire on a chaser without losing
  // speed. Sharpened turning rates apply during the drift, and a release
  // boost rewards longer holds.
  const BRAKE_PITCH_MULT = 1.3;
  const BRAKE_YAW_MULT = 1.7;
  const BRAKE_FULL_TIME = 1.4;            // seconds of holding to fully charge
  const BRAKE_BOOST_MIN = 0.18;           // minimum charge to launch any boost
  const BRAKE_BOOST_DURATION_MAX = 1.0;   // seconds of post-release boost at full charge
  const BRAKE_BOOST_BONUS_MAX = 50;       // flat extra u/s added to forward speed at full charge
  // Drift drag: per-second velocity multiplier while Space is held. 0.9
  // ≈ −10%/s, so speed has to drop ~7s to halve. Light enough that drift
  // still preserves momentum, heavy enough that you can't spin forever.
  const DRIFT_DRAG = 0.9;
  // Drift grip: how strongly the velocity vector is rotated toward the
  // current facing while drifting (magnitude preserved). Mimics a real
  // drift where the wheels still pull you in the new direction over
  // time. Set to 0 to fully decouple orientation from velocity.
  // [REVERT: drop this constant + the grip block in the drift branch.]
  const DRIFT_GRIP = 0.3;
  // Hold S during a drift to brake hard. Replaces the gentle DRIFT_DRAG
  // with a much stronger decay (~90%/s) for the frames S is held — gives
  // the player an explicit "I want to slow down now" tool without
  // breaking the drift's orientation freedom.
  const DRIFT_BRAKE = 0.1;
  // Velocity blend used while the brake-release boost is firing. Lower
  // than the normal VELOCITY_BLEND so the slingshot redirects floatily
  // — old momentum lingers a beat before the new heading takes over.
  const VELOCITY_BLEND_RELEASE = 1.5;
  // Drift overload: a two-stage warning before damage. Once charge is
  // full the bar stays yellow for the WARN delay (still safe — release
  // here for a clean boost). After WARN it flips red as a "let go now"
  // signal and waits another DAMAGE delay before HP starts ticking.
  const BRAKE_OVERCHARGE_WARN = 1.0;     // yellow → red after this long at full
  const BRAKE_OVERCHARGE_DAMAGE = 2.0;   // total seconds at full before damage
  const BRAKE_OVERCHARGE_DPS = 10;
  let brakeOverchargeTime = 0;
  let selfDamageAccum = 0;
  let brakeCharge = 0;
  let prevBraking = false;
  let brakeBoostTimer = 0;
  let brakeBoostCharge = 0;
  const chargeBar = document.getElementById('chargebar');
  const chargeFill = document.getElementById('chargebar-fill');

  // Bullets fire faster than beams — projectiles need lead, so giving them
  // higher DPS keeps them competitive with the always-on-target beam.
  const BULLET_COOLDOWN = 0.05;       // 20 shots/sec
  const BEAM_COOLDOWN = 0.25;         // 4 shots/sec
  // Single nose-mounted gun — fires straight along ship-forward from the
  // tip of the cone (cone half-length is 1.6).
  const MUZZLE_OFFSETS = [new THREE.Vector3(0, 0, 0.6)];
  let fireTimer = 0;

  // Gun mode toggle: 'bullet' (projectile) or 'beam' (instant hitscan).
  // Beam costs 2 ammo per shot, range BEAM_RANGE units.
  let gunMode = 'bullet';
  let prevKeyP = false;
  let prevKeyO = false;
  let prevKeyL = false;
  const BEAM_RANGE = 1000;
  // Generous targeting sphere — covers wing silhouette comfortably for both
  // reticle anchoring and beam hit detection. Slightly bigger than the
  // bullet hit radius so beams feel reliably "locked" when reticle is on.
  const BEAM_SHIP_RADIUS = 5.5;
  const BEAM_FORWARD_OFFSET = 4;

  // Casts a ray from origin along unit dir up to maxDist against the world
  // (remote ships + asteroids). Returns the closest hit metadata. Used by:
  //   - Targeting reticle anchoring
  //   - Beam fire hit detection
  // opts.skipShipId ignores a specific ship; opts.skipTeam ignores all
  // ships on a given team (no friendly fire / friendly target lock).
  function castWorldRay(origin, dir, maxDist, opts = {}) {
    const skipShipId = opts.skipShipId ?? null;
    const skipTeam = opts.skipTeam ?? null;
    let bestT = maxDist;
    let hitShipId = null;
    let hitAsteroidId = null;
    for (const r of remotePlayers.values()) {
      if (!r.alive || !r.hasTarget) continue;
      if (skipShipId !== null && r.id === skipShipId) continue;
      if (skipTeam !== null && r.team === skipTeam) continue;
      const t = raySphereDist(
        origin.x, origin.y, origin.z, dir.x, dir.y, dir.z,
        r.ship.position.x, r.ship.position.y, r.ship.position.z, BEAM_SHIP_RADIUS,
      );
      if (t !== null && t < bestT) { bestT = t; hitShipId = r.id; hitAsteroidId = null; }
    }
    for (const a of asteroids.list) {
      const t = raySphereDist(
        origin.x, origin.y, origin.z, dir.x, dir.y, dir.z,
        a.mesh.position.x, a.mesh.position.y, a.mesh.position.z, a.radius,
      );
      if (t !== null && t < bestT) { bestT = t; hitAsteroidId = a.id; hitShipId = null; }
    }
    // Static obstacles (moon). No hitId — they can't be damaged, they
    // just truncate the ray so beams stop at the surface and reticle
    // anchors don't reach through.
    for (const o of obstacles) {
      const t = raySphereDist(
        origin.x, origin.y, origin.z, dir.x, dir.y, dir.z,
        o.pos.x, o.pos.y, o.pos.z, o.radius,
      );
      if (t !== null && t < bestT) { bestT = t; hitShipId = null; hitAsteroidId = null; }
    }
    return { dist: bestT, hitShipId, hitAsteroidId };
  }

  // Ray-vs-sphere. Returns hit distance along `dir` (unit), or null.
  function raySphereDist(ox, oy, oz, dx, dy, dz, cx, cy, cz, r) {
    const mx = ox - cx, my = oy - cy, mz = oz - cz;
    const b = mx * dx + my * dy + mz * dz;
    const c = mx * mx + my * my + mz * mz - r * r;
    if (c > 0 && b > 0) return null;
    const disc = b * b - c;
    if (disc < 0) return null;
    const sd = Math.sqrt(disc);
    let t = -b - sd;
    if (t < 0) t = -b + sd;
    return t > 0 ? t : null;
  }

  // Gun: 20 trigger pulls before lockout. Both gun and boost share the
  // same lazy regen rule — passive recharge only kicks in REGEN_DELAY
  // seconds after the last use, so trigger-spamming or shift-tapping
  // doesn't get free top-ups.
  const REGEN_DELAY = 1.0;          // matches BOOST_REGEN_DELAY
  const MAX_AMMO = 90;
  const AMMO_REGEN = 36;            // 90 / 2.5s, same refill time as boost
  let ammo = MAX_AMMO;
  let ammoIdle = REGEN_DELAY; // start full, eligible to regen immediately

  const MISSILE_MAX = 4;
  let missilesLeft = MISSILE_MAX;
  let prevKeyE = false;
  const mslPips = [
    document.getElementById('msl-pip-1'),
    document.getElementById('msl-pip-2'),
    document.getElementById('msl-pip-3'),
    document.getElementById('msl-pip-4'),
  ];

  // Shift-boost fuel: 10 seconds at full, drains while held.
  const MAX_BOOST = 10;
  const BOOST_DRAIN = 2;
  const BOOST_RECHARGE = 4;
  const BOOST_REGEN_DELAY = 1.0;
  let boostMeter = MAX_BOOST;
  let boostIdle = REGEN_DELAY;

  // Health regeneration: after 2s out of combat (no damage taken, no shots
  // fired) regen ticks +1 HP every 0.1s until full.
  const HEALTH_REGEN_DELAY    = 2.0;
  const HEALTH_REGEN_INTERVAL = 0.1;
  let healthIdleDamage = HEALTH_REGEN_DELAY; // time since last damage received
  let healthIdleShot   = HEALTH_REGEN_DELAY; // time since last shot fired
  let healthRegenTick  = 0;                  // accumulator for 0.1s ticks

  const boostBar = document.getElementById('boostbar');
  const boostFill = document.getElementById('boostbar-fill');
  const heatBar = document.getElementById('heatbar');
  const heatFill = document.getElementById('heatbar-fill');
  const hitVignette = document.getElementById('hit-vignette');
  // Vignette intensity is driven each frame: spike to 1 on damage, decay
  // toward 0. prevHpForFlash tracks the last HP so we only flash on a
  // drop (not on respawn-back-to-full or during clamps).
  let prevHpForFlash = SHIP_MAX_HP;
  let vignetteAlpha = 0;
  const VIGNETTE_DECAY = 2.4; // 1 → 0 in ~0.4s

  const TRAIL_OFFSETS = [
    new THREE.Vector3(-2.2, -0.05, -1.8),
    new THREE.Vector3( 2.2, -0.05, -1.8),
  ];
  // Admin model has jets closer together and further back.
  const ADMIN_TRAIL_OFFSETS = [
    new THREE.Vector3(-0.9, -0.05, -2.4),
    new THREE.Vector3( 0.9, -0.05, -2.4),
  ];
  const localTrailOffsets = isLocalAdmin ? ADMIN_TRAIL_OFFSETS : TRAIL_OFFSETS;
  // Enemy/remote-ship trails. Default on; users on low-end devices can
  // turn this off in the settings panel. Captured at startGame time —
  // mid-match changes apply on next game.
  const enemyTrailsEnabled = localStorage.getItem('spaceships:enemyTrails') !== '0';
  // Per-state emission profile: rate (puffs/sec/engine), particle scale
  // range, color palette, position jitter, and lifetime range.
  const EMIT_CONFIG = {
    move:  { rate: 18, scale: [0.16, 0.28], colors: [0xffffff],         jitter: 0.05, life: [0.18, 0.30] },
    boost: { rate: 45, scale: [0.50, 0.85], colors: [0x66ddff, 0xffffff], jitter: 0.13, life: [0.45, 0.65] },
    brake: { rate: 35, scale: [0.36, 0.60], colors: [0xffd933, 0xffaa33], jitter: 0.10, life: [0.28, 0.45] },
  };
  let trailTimer = 0;
  // Read trail customization once at game start; captured so mid-match changes
  // apply on the next match (consistent with how pixel filter / enemy trails work).
  const savedTrailColorHex = parseInt(getSavedTrailColor().replace('#', ''), 16);
  const savedTrailShape    = getSavedTrailShape();

  let targetThrottle = 0;
  let throttle = 0;

  const hud = document.getElementById('hud-stats');
  const hpFill = document.getElementById('healthbar-fill');
  const hpText = document.getElementById('healthbar-text');
  const deathBanner = document.getElementById('deathbanner');

  const tmpQ = new THREE.Quaternion();
  const xAxis = new THREE.Vector3(1, 0, 0);
  const yAxis = new THREE.Vector3(0, 1, 0);
  const zAxis = new THREE.Vector3(0, 0, 1);

  const clock = new THREE.Clock();

  function update(dt) {
    // Trials 3-2-1-GO countdown: freeze the ship, update the overlay, then release.
    if (isTrialsMode && trialsCountdownActive) {
      trialsCountdown -= dt;
      const cdWrap = document.getElementById('trials-countdown');
      const cdNum  = document.getElementById('trials-countdown-num');
      const n = Math.ceil(Math.max(0, trialsCountdown));
      if (cdNum) {
        if (n > 0) {
          cdNum.textContent = n;
          cdNum.style.color = n === 3 ? '#ff5566' : n === 2 ? '#ffcc44' : '#44ffcc';
        } else {
          cdNum.textContent = 'GO!';
          cdNum.style.color = '#66ff44';
        }
      }
      if (trialsCountdown <= -0.6) {
        trialsCountdownActive = false;
        if (cdWrap) cdWrap.style.display = 'none';
      }
      tpCam.update(dt, input);
      return;
    }

    const braking = myAlive && input.keys.has('Space');

    if (myAlive) {
      // Mobile slider sets throttle absolutely; W/S/wheel are ignored
      // while it has a value (touch never sets the others anyway, but
      // be explicit so a stale wheel delta can't bump the slider).
      if (input.throttleOverride !== null) {
        targetThrottle = input.throttleOverride * MAX_THROTTLE;
        input.consumeWheel();
      } else {
        const wheel = input.consumeWheel();
        if (wheel !== 0) targetThrottle += wheel * THROTTLE_STEP;
        if (input.keys.has('KeyW')) targetThrottle += KEY_THROTTLE_RATE * dt;
        if (input.keys.has('KeyS')) targetThrottle -= KEY_THROTTLE_RATE * dt;
      }
      // Drifting preserves throttle so the ship resumes thrusting at the
      // same setting (along the new facing) the moment Space is released.
      targetThrottle = Math.max(0, Math.min(MAX_THROTTLE, targetThrottle));
      throttle = THREE.MathUtils.damp(throttle, targetThrottle, 3, dt);

      let sx = input.rmb ? 0 : input.steerX;
      let sy = input.rmb ? 0 : input.steerY;
      if (Math.abs(sx) < STEER_DEADZONE) sx = 0;
      if (Math.abs(sy) < STEER_DEADZONE) sy = 0;
      sx = Math.sign(sx) * Math.pow(Math.abs(sx), 1.6);
      sy = Math.sign(sy) * Math.pow(Math.abs(sy), 1.6);
      // Arrow-key steering for trackpad-averse pilots. Targets ramp from 0
      // toward ±1 via damp so deflection is smooth, not a snap. Held key
      // = analog-feeling input; release returns toward 0.
      let kxTarget = 0, kyTarget = 0;
      if (input.keys.has('ArrowLeft'))  kxTarget -= 1;
      if (input.keys.has('ArrowRight')) kxTarget += 1;
      if (input.keys.has('ArrowUp'))    kyTarget -= 1;
      if (input.keys.has('ArrowDown'))  kyTarget += 1;
      // Slow ramp on press, fast decay on release — taps stay micro.
      // Hold Q to halve the ramp rate for fine-aim micro-corrections.
      const upRate = input.keys.has('KeyQ') ? ARROW_RAMP_UP_RATE_FINE : ARROW_RAMP_UP_RATE;
      const rateX = kxTarget !== 0 ? upRate : ARROW_RAMP_DOWN_RATE;
      const rateY = kyTarget !== 0 ? upRate : ARROW_RAMP_DOWN_RATE;
      arrowKx = THREE.MathUtils.damp(arrowKx, kxTarget, rateX, dt);
      arrowKy = THREE.MathUtils.damp(arrowKy, kyTarget, rateY, dt);
      if (kxTarget !== 0 || Math.abs(arrowKx) > 0.01) sx = arrowKx;
      if (kyTarget !== 0 || Math.abs(arrowKy) > 0.01) sy = arrowKy;

      const pitchMult = braking ? BRAKE_PITCH_MULT : 1;
      const yawMult = braking ? BRAKE_YAW_MULT : 1;
      const pitchRate = (sy < 0 ? PITCH_RATE * PITCH_UP_BOOST : PITCH_RATE) * pitchMult;
      const pitch = sy * pitchRate * dt;
      const yaw = -sx * YAW_RATE * yawMult * dt;

      let roll = 0;
      if (input.keys.has('KeyD')) roll += ROLL_RATE * pitchMult * dt;
      if (input.keys.has('KeyA')) roll -= ROLL_RATE * pitchMult * dt;

      if (pitch) ship.quaternion.multiply(tmpQ.setFromAxisAngle(xAxis, pitch));
      if (yaw)   ship.quaternion.multiply(tmpQ.setFromAxisAngle(yAxis, yaw));
      if (roll)  ship.quaternion.multiply(tmpQ.setFromAxisAngle(zAxis, roll));
      ship.quaternion.normalize();

      if (aimAssistEnabled) {
        const steerMag = Math.max(Math.abs(sx), Math.abs(sy));
        applyAimAssist(dt, steerMag);
      }
    }

    if (brakeBoostTimer > 0) brakeBoostTimer = Math.max(0, brakeBoostTimer - dt);
    const wantShift = input.keys.has('ShiftLeft') || input.keys.has('ShiftRight');
    const shiftBoost = myAlive && !braking && wantShift && boostMeter > 0;
    const brakeReleaseBoost = myAlive && brakeBoostTimer > 0;
    const boosting = myAlive && (shiftBoost || brakeReleaseBoost);
    boostIdle += dt;
    if (shiftBoost) {
      boostMeter = Math.max(0, boostMeter - BOOST_DRAIN * dt);
      boostIdle = 0;
    } else if (wantShift) {
      // Holding shift with empty meter still counts as "in use" — hold the
      // regen back until they release the key.
      boostIdle = 0;
    }
    if (boostMeter < MAX_BOOST && boostIdle >= BOOST_REGEN_DELAY) {
      boostMeter = Math.min(MAX_BOOST, boostMeter + BOOST_RECHARGE * dt);
    }
    if (myAlive) {
      // Drift mode: orientation is decoupled from velocity. We freeze the
      // thrust integration while Space is held, so the player can swing
      // the nose around without losing trajectory. A gentle drag still
      // bleeds speed so a held drift isn't free perpetual motion.
      if (braking) {
        // [REVERT-DRIFT-GRIP start] Rotate velocity toward facing while
        // preserving magnitude — gives the drift a "wheel grip" feel.
        const speed = shipVelocity.length();
        if (speed > 0.001 && DRIFT_GRIP > 0) {
          const fwd = new THREE.Vector3(0, 0, 1).applyQuaternion(ship.quaternion);
          const desired = fwd.multiplyScalar(speed);
          shipVelocity.lerp(desired, 1 - Math.pow(0.001, dt * DRIFT_GRIP / 6));
        }
        // [REVERT-DRIFT-GRIP end]
        // S during a drift overrides the gentle drag with a hard brake.
        const drag = input.keys.has('KeyS') ? DRIFT_BRAKE : DRIFT_DRAG;
        shipVelocity.multiplyScalar(Math.pow(drag, dt));
      } else {
        const speedMult = shiftBoost ? BOOST_FACTOR : 1;
        const forward = new THREE.Vector3(0, 0, 1).applyQuaternion(ship.quaternion);
        const target = forward.clone().multiplyScalar(throttle * speedMult);
        // Brake-release adds a flat forward bonus instead of a multiplier so
        // the effect is the same regardless of current throttle.
        if (brakeReleaseBoost) {
          target.addScaledVector(forward, BRAKE_BOOST_BONUS_MAX * brakeBoostCharge);
        }
        // While the release-boost is firing, fall to a slower blend so
        // the redirect floats — old momentum eases out instead of snapping.
        const blend = brakeReleaseBoost ? VELOCITY_BLEND_RELEASE : VELOCITY_BLEND;
        shipVelocity.lerp(target, 1 - Math.pow(0.001, dt * blend / 6));
      }
      ship.position.addScaledVector(shipVelocity, dt);
    }

    // Charge while braking; on release convert charge into a timed boost
    // (same speed multiplier as Shift, but it auto-runs for a duration
    // proportional to how long you held).
    if (braking) {
      brakeCharge = Math.min(1, brakeCharge + dt / BRAKE_FULL_TIME);
    } else if (prevBraking && myAlive) {
      if (brakeCharge >= BRAKE_BOOST_MIN) {
        brakeBoostTimer = brakeCharge * BRAKE_BOOST_DURATION_MAX;
        brakeBoostCharge = brakeCharge;
      }
      brakeCharge = 0;
    } else if (!myAlive) {
      brakeCharge = 0;
      brakeBoostTimer = 0;
      brakeBoostCharge = 0;
    }
    prevBraking = braking;

    // Drift overload: once charge is full, tick a grace timer. The bar
    // stays yellow up to BRAKE_OVERCHARGE_WARN, flips red between WARN
    // and BRAKE_OVERCHARGE_DAMAGE, and only starts costing HP past that.
    if (braking && brakeCharge >= 1 && myAlive) {
      brakeOverchargeTime += dt;
      // Tutorial: damage suppressed so a new pilot exploring drift can't
      // accidentally die while reading the prompt.
      if (brakeOverchargeTime > BRAKE_OVERCHARGE_DAMAGE && SOLO_MODE !== 'tutorial') {
        selfDamageAccum += BRAKE_OVERCHARGE_DPS * dt;
        while (selfDamageAccum >= 1) {
          selfDamageAccum -= 1;
          if (ws && ws.readyState === WebSocket.OPEN) {
            ws.send(JSON.stringify({ type: 'self-damage', dmg: 1 }));
          } else if (isSolo) {
            applyPlayerDamageLocal(1);
          }
        }
      }
    } else {
      brakeOverchargeTime = 0;
      selfDamageAccum = 0;
    }

    if (chargeBar && chargeFill) {
      chargeBar.classList.toggle('active', braking || brakeCharge > 0);
      chargeBar.classList.toggle('full', brakeCharge >= 1);
      chargeBar.classList.toggle(
        'overload',
        brakeCharge >= 1 && brakeOverchargeTime >= BRAKE_OVERCHARGE_WARN,
      );
      chargeFill.style.width = (brakeCharge * 100).toFixed(1) + '%';
    }
    if (boostFill) {
      boostFill.style.width = (boostMeter / MAX_BOOST * 100).toFixed(1) + '%';
    }
    if (heatFill && heatBar) {
      heatFill.style.width = (ammo / MAX_AMMO * 100).toFixed(1) + '%';
      // Red bar when there isn't enough for the current weapon's next
      // shot — visual cue that you have to wait for regen.
      heatBar.classList.toggle('overheated', ammo < (gunMode === 'beam' ? 3 : 1));
    }
    for (let _pi = 0; _pi < mslPips.length; _pi++) {
      if (mslPips[_pi]) mslPips[_pi].classList.toggle('empty', _pi >= missilesLeft);
    }

    // P: toggle gun mode (bullet ↔ beam) on key-down edge.
    const nowKeyP = input.keys.has('KeyP');
    if (nowKeyP && !prevKeyP) {
      gunMode = gunMode === 'beam' ? 'bullet' : 'beam';
    }
    prevKeyP = nowKeyP;

    // C: toggle aim assist. Persisted to localStorage so it sticks.
    const nowKeyC = input.keys.has('KeyC');
    if (nowKeyC && !prevKeyC) {
      aimAssistEnabled = !aimAssistEnabled;
      try { localStorage.setItem('spaceships:aimAssist', aimAssistEnabled ? '1' : '0'); } catch {}
      showAimAssistToast(aimAssistEnabled);
    }
    prevKeyC = nowKeyC;

    // O: grab/release mouse via pointer lock. No-op in no-mouse mode since
    // the mouse is intentionally inert.
    const nowKeyO = input.keys.has('KeyO');
    if (nowKeyO && !prevKeyO && !noMouseMode) {
      if (document.pointerLockElement) {
        document.exitPointerLock();
      } else {
        renderer.domElement.requestPointerLock?.();
      }
    }
    prevKeyO = nowKeyO;

    // L: toggle fullscreen on the page root.
    const nowKeyL = input.keys.has('KeyL');
    if (nowKeyL && !prevKeyL) {
      if (document.fullscreenElement) {
        document.exitFullscreen?.();
      } else {
        document.documentElement.requestFullscreen?.();
      }
    }
    prevKeyL = nowKeyL;

    // E: fire a homing missile at the closest enemy.
    const nowKeyE = input.keys.has('KeyE');
    if (nowKeyE && !prevKeyE && myAlive && missilesLeft > 0) {
      let closestRecord = null;
      let closestDist = Infinity;
      for (const r of remotePlayers.values()) {
        if (!r.alive || !r.hasTarget) continue;
        if (myTeam !== undefined && myTeam !== null && r.team === myTeam) continue;
        const d = ship.position.distanceTo(r.ship.position);
        if (d < closestDist) { closestDist = d; closestRecord = r; }
      }
      if (closestRecord !== null) {
        const fwd = new THREE.Vector3(0, 0, 1).applyQuaternion(ship.quaternion);
        const mslOrigin = ship.position.clone().addScaledVector(fwd, 6);
        missileSystem.fire(mslOrigin, fwd, closestRecord);
        missilesLeft--;
        audio.play('shoot');
      }
    }
    prevKeyE = nowKeyE;

    fireTimer -= dt;
    ammoIdle += dt;
    const ammoCost = gunMode === 'beam' ? 3 : 1;
    const canFire = ammo >= ammoCost;
    if ((input.lmb || input.keys.has('KeyF')) && fireTimer <= 0 && myAlive && canFire) {
      const dir = new THREE.Vector3(0, 0, 1).applyQuaternion(ship.quaternion);
      const shots = [];
      for (const off of MUZZLE_OFFSETS) {
        const origin = off.clone().applyQuaternion(ship.quaternion).add(ship.position);
        if (gunMode === 'beam') {
          const cast = castWorldRay(origin, dir, BEAM_RANGE, { skipTeam: myTeam });
          const bestT = cast.dist;
          const hitTargetId = cast.hitShipId;
          const hitAsteroidId = cast.hitAsteroidId;
          const end = origin.clone().addScaledVector(dir, bestT);
          // Visual: spawn the beam farther forward so it doesn't appear to
          // emerge from inside the ship. If the hit is closer than the
          // offset, fall back to the muzzle origin.
          const visualStart = bestT > BEAM_FORWARD_OFFSET
            ? origin.clone().addScaledVector(dir, BEAM_FORWARD_OFFSET)
            : origin;
          beams.fire(visualStart, end, 'self');
          if (hitTargetId !== null) {
            bullets.spawnExplosion(end, 1.0);
            audio.play('hitmarker_2');
            if (ws && ws.readyState === WebSocket.OPEN) {
              ws.send(JSON.stringify({ type: 'hit', targetId: hitTargetId, kind: 'beam' }));
            } else if (isSolo) {
              applyHitToBot(hitTargetId, 10, opts.you, myTeam);
            }
          } else if (hitAsteroidId !== null && ws && ws.readyState === WebSocket.OPEN) {
            ws.send(JSON.stringify({ type: 'asteroid-hit', id: hitAsteroidId }));
            bullets.spawnExplosion(end, 0.6);
            audio.play('impact');
          }
          shots.push({
            pos: [visualStart.x, visualStart.y, visualStart.z],
            end: [end.x, end.y, end.z],
          });
        } else {
          bullets.fire(origin, dir, 'self');
          shots.push({
            pos: [origin.x, origin.y, origin.z],
            dir: [dir.x, dir.y, dir.z],
          });
        }
      }
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: 'fire', kind: gunMode, shots }));
      }
      audio.play('shoot');
      ammo = Math.max(0, ammo - ammoCost);
      ammoIdle = 0;
      healthIdleShot = 0;
      fireTimer = gunMode === 'beam' ? BEAM_COOLDOWN : BULLET_COOLDOWN;
    }
    if (ammo < MAX_AMMO && ammoIdle >= REGEN_DELAY) {
      ammo = Math.min(MAX_AMMO, ammo + AMMO_REGEN * dt);
    }

    // Health regen: tick idle timers; regenerate 1 HP per 0.1s after 2s out of combat.
    if (myAlive) {
      healthIdleDamage += dt;
      healthIdleShot   += dt;
      if (healthIdleDamage >= HEALTH_REGEN_DELAY && healthIdleShot >= HEALTH_REGEN_DELAY && myHp < SHIP_MAX_HP) {
        healthRegenTick += dt;
        if (healthRegenTick >= HEALTH_REGEN_INTERVAL) {
          healthRegenTick -= HEALTH_REGEN_INTERVAL;
          myHp = Math.min(SHIP_MAX_HP, myHp + 1);
        }
      } else {
        healthRegenTick = 0;
      }
    }

    // Pick which engine emission profile applies this frame. Brake takes
    // priority over boost; boost over plain movement.
    let emitMode = null;
    if (myAlive) {
      if (braking) emitMode = 'brake';
      else if (boosting) emitMode = 'boost';
      else if (shipVelocity.length() > 5) emitMode = 'move';
    }
    // Engine mixer: move volume scales with speed, boost ramps in via damp.
    // Boosting and braking each duck the move loop so the layered sound
    // doesn't muddy. Damp smooths transitions for ease-in/ease-out.
    const speed = shipVelocity.length();
    const speedFrac = Math.max(0, Math.min(1, speed / SPEED_FOR_FULL_VOL));
    let moveTarget = myAlive ? MOVE_MAX_VOL * speedFrac : 0;
    let boostTarget = 0;
    if (myAlive) {
      if (boosting) {
        moveTarget *= MOVE_DUCK_BOOST;
        boostTarget = BOOST_MAX_VOL;
      } else if (braking) {
        moveTarget *= MOVE_DUCK_BRAKE;
      }
    }
    moveVol = THREE.MathUtils.damp(moveVol, moveTarget, 4, dt);
    boostVol = THREE.MathUtils.damp(boostVol, boostTarget, 5, dt);
    audio.setLoopVolume('move', moveVol);
    audio.setLoopVolume('boost', boostVol);
    if (emitMode) {
      const cfg = EMIT_CONFIG[emitMode];
      trailTimer += dt;
      const interval = 1 / cfg.rate;
      while (trailTimer >= interval) {
        trailTimer -= interval;
        for (const off of localTrailOffsets) {
          const p = off.clone().applyQuaternion(ship.quaternion).add(ship.position);
          p.x += (Math.random() - 0.5) * cfg.jitter;
          p.y += (Math.random() - 0.5) * cfg.jitter;
          p.z += (Math.random() - 0.5) * cfg.jitter;
          const scale = cfg.scale[0] + Math.random() * (cfg.scale[1] - cfg.scale[0]);
          // Use player's chosen color for move/boost; keep brake orange as a
          // distinct deceleration signal.
          const baseColor = cfg.colors[Math.floor(Math.random() * cfg.colors.length)];
          const color = (emitMode !== 'brake') ? savedTrailColorHex : baseColor;
          const life = cfg.life[0] + Math.random() * (cfg.life[1] - cfg.life[0]);
          trails.emit(p, scale, color, life, savedTrailShape);
        }
      }
    } else {
      trailTimer = 0;
    }

    // Remote ship trails. Same emission machinery as the local player,
    // but driven from each remote's tracked velocity (and r.boost flag
    // from their 'state' broadcast). Skipped entirely when the user has
    // disabled enemy trails for perf.
    if (enemyTrailsEnabled) {
      for (const r of remotePlayers.values()) {
        if (!r.alive || !r.hasTarget) { r.trailTimer = 0; continue; }
        // Distance-gate so distant pilots don't reveal themselves via
        // bright additive plumes. Tied to the marker-visibility cap so
        // the diamond and the engine trail appear/disappear together.
        if (ship.position.distanceTo(r.ship.position) > MARKER_VISIBLE_DIST) {
          r.trailTimer = 0;
          continue;
        }
        const rSpeed = r.vel.length();
        let rMode = null;
        if (r.boost) rMode = 'boost';
        else if (rSpeed > 5) rMode = 'move';
        if (!rMode) { r.trailTimer = 0; continue; }
        const cfg = EMIT_CONFIG[rMode];
        r.trailTimer = (r.trailTimer || 0) + dt;
        const interval = 1 / cfg.rate;
        while (r.trailTimer >= interval) {
          r.trailTimer -= interval;
          for (const off of (r.trailOffsets || TRAIL_OFFSETS)) {
            const p = off.clone().applyQuaternion(r.ship.quaternion).add(r.ship.position);
            p.x += (Math.random() - 0.5) * cfg.jitter;
            p.y += (Math.random() - 0.5) * cfg.jitter;
            p.z += (Math.random() - 0.5) * cfg.jitter;
            const scale = cfg.scale[0] + Math.random() * (cfg.scale[1] - cfg.scale[0]);
            const color = cfg.colors[Math.floor(Math.random() * cfg.colors.length)];
            const life = cfg.life[0] + Math.random() * (cfg.life[1] - cfg.life[0]);
            trails.emit(p, scale, color, life);
          }
        }
      }
    }

    asteroids.update(dt);
    if (moon) moon.update(dt);
    beams.update(dt);
    bullets.update(
      dt,
      asteroids,
      remotePlayers,
      (targetId) => {
        audio.play('hitmarker_2');
        if (ws && ws.readyState === WebSocket.OPEN) {
          ws.send(JSON.stringify({ type: 'hit', targetId, kind: 'bullet' }));
        } else if (isSolo) {
          applyHitToBot(targetId, 10, opts.you, myTeam);
        }
      },
      (asteroidId) => {
        if (ws && ws.readyState === WebSocket.OPEN) {
          ws.send(JSON.stringify({ type: 'asteroid-hit', id: asteroidId }));
        }
        audio.play('impact');
      },
      myTeam,
      obstacles,
    );
    missileSystem.update(
      dt,
      remotePlayers,
      (targetId) => {
        audio.play('hitmarker_2');
        if (ws && ws.readyState === WebSocket.OPEN) {
          ws.send(JSON.stringify({ type: 'hit', targetId, kind: 'missile' }));
        } else if (isSolo) {
          applyHitToBot(targetId, 50, opts.you, myTeam);
        }
      },
      myTeam,
    );
    trails.update(dt, camera);
    if (clouds) clouds.update(dt);

    // Keep shadow light directly above the player so the frustum stays tight
    // and the shadow falls straight down regardless of world position.
    if (terrainSun) {
      terrainSun.target.position.copy(ship.position);
      terrainSun.target.updateMatrixWorld();
      terrainSun.position.set(ship.position.x, ship.position.y + 500, ship.position.z);
    }
    if (myAlive) {
      resolveCollisions();
      resolveMothershipCollisions();
    }
    tpCam.update(dt, input);

    // --- Network sync ---
    if (ws && ws.readyState === WebSocket.OPEN && myAlive) {
      stateTimer += dt;
      if (stateTimer >= STATE_INTERVAL) {
        stateTimer = 0;
        ws.send(JSON.stringify({
          type: 'state',
          pos: [ship.position.x, ship.position.y, ship.position.z],
          quat: [ship.quaternion.x, ship.quaternion.y, ship.quaternion.z, ship.quaternion.w],
          boost: boosting,
        }));
      }
    }
    // Smooth remote players toward their last reported pose.
    const remoteLerp = 1 - Math.pow(0.001, dt * 8);
    for (const r of remotePlayers.values()) {
      // Solo bots write ship.position directly each frame; lerp is for remote players only.
      if (r.isBot && isSolo) continue;
      r.ship.position.lerp(r.targetPos, remoteLerp);
      r.ship.quaternion.slerp(r.targetQuat, remoteLerp);
    }

    // Solo: tick all bots' AI, run respawn timers for bots + player, and
    // count down the match timer.
    if (isSolo) {
      for (const b of bots) {
        if (b.record.alive) {
          if (!matchOver) b.ai.update(dt);
        } else if (b.record.respawnTimer > 0) {
          b.record.respawnTimer -= dt;
          if (b.record.respawnTimer <= 0) reviveBotLocal(b.id);
        }
      }
      if (!myAlive && myRespawnTimer > 0) {
        myRespawnTimer -= dt;
        if (myRespawnTimer <= 0) revivePlayerLocal();
      }
      if (matchActive && !matchOver) {
        matchTimer -= dt;
        renderMatchHud();
        if (matchTimer <= 0) endMatch();
      }

      if (isTrialsMode && cpMeshes.length > 0) {
        if (cpCooldown > 0) {
          cpCooldown -= dt;
        } else if (myAlive && ship.position.distanceTo(TRIAL_CPS[trialsNextCp]) < CP_TRIGGER_DIST) {
          cpMeshes[trialsNextCp].material.color.setHex(0x44aa66);
          cpMeshes[trialsNextCp].material.opacity = 0.15;
          boostMeter = Math.min(MAX_BOOST, boostMeter + 3.5);
          boostIdle = 0;
          const wasAtStart = trialsNextCp === 0;
          trialsNextCp = (trialsNextCp + 1) % TRIAL_CPS.length;
          cpCooldown = 1.5;

          if (wasAtStart) {
            if (!trialsRunning) {
              trialsRunning = true;
              trialsTimer = 0;
              trialsLap = 1;
            } else {
              trialsLastLap = trialsTimer;
              if (trialsBestLap === null || trialsTimer < trialsBestLap) {
                trialsBestLap = trialsTimer;
                localStorage.setItem(TRIAL_BEST_KEY, trialsBestLap.toFixed(3));
                reportTrialTime(TRIAL_NUM, trialsBestLap);
              }
              trialsTimer = 0;
              trialsLap++;
            }
          }

          cpMeshes[trialsNextCp].material.color.setHex(0x66ffcc);
          cpMeshes[trialsNextCp].material.opacity = 0.9;
          updateTrialsHud();
        }

        if (trialsRunning && myAlive) {
          trialsTimer += dt;
          updateTrialsHud();
        }

        // Animate tracer dots flowing from ship toward the next checkpoint.
        if (tracerDots.length > 0) {
          if (myAlive) {
            const target = TRIAL_CPS[trialsNextCp];
            const n = tracerDots.length;
            const anim = (performance.now() * 0.0008) % 1;
            for (let i = 0; i < n; i++) {
              const t = ((i / n + anim) % 1) * 0.88 + 0.06;
              tracerDots[i].position.lerpVectors(ship.position, target, t);
              tracerDots[i].material.opacity = 0.12 + (1 - t) * 0.72;
              tracerDots[i].visible = true;
            }
          } else {
            for (const d of tracerDots) d.visible = false;
          }
        }
      }
    }


    if (tutorial) tutorial.update(dt);

    // --- Targeting computer ---
    // Player crosshair: raycast from muzzle along ship-forward, anchor the
    // reticle to whatever it hits (or BEAM_RANGE if empty space). This
    // sidesteps camera parallax — the reticle sits on the actual hit point
    // of an instant shot, so it always lines up with the beam visually.
    const W = window.innerWidth, H = window.innerHeight;
    const projTmp = new THREE.Vector3();
    const aimFwd = new THREE.Vector3(0, 0, 1).applyQuaternion(ship.quaternion);
    const muzzleWorld = ship.position.clone().addScaledVector(aimFwd, 1.6);
    const reticleCast = castWorldRay(muzzleWorld, aimFwd, BEAM_RANGE, { skipTeam: myTeam });
    const reticleAimWorld = muzzleWorld.clone().addScaledVector(aimFwd, reticleCast.dist);
    projTmp.copy(reticleAimWorld).project(camera);
    const reticleX = (projTmp.x * 0.5 + 0.5) * W;
    const reticleY = (-projTmp.y * 0.5 + 0.5) * H;

    const reticleEl = document.getElementById('reticle');
    if (reticleEl) {
      reticleEl.style.left = reticleX + 'px';
      reticleEl.style.top = reticleY + 'px';
    }

    let bestAlignment = Infinity;
    let anyVisible = false;
    // Show target box whenever the diamond marker is visible so there is no
    // confusing gap where you see the blip but not the stats. Aim-assist
    // still only engages within ASSIST_RANGE (1000u).
    const TARGETING_MAX_DIST = MARKER_VISIBLE_DIST;
    for (const r of remotePlayers.values()) {
      if (!r.alive || !r.hasTarget) {
        r.box.style.display = 'none';
        r.lead.style.display = 'none';
        if (r.marker) r.marker.visible = false;
        continue;
      }
      const dist = ship.position.distanceTo(r.ship.position);
      // Fog-of-war: hide the ship mesh, marker, and (via separate caps
      // above) trails / box past MARKER_VISIBLE_DIST. Three.js was
      // rendering the silhouette out to the camera's 5000u far plane,
      // which leaked enemy positions long before the diamond appeared.
      r.ship.visible = dist <= MARKER_VISIBLE_DIST;
      // Hit flash: spike on HP drop, ramp down over ~0.25s. Drives the
      // emissive of every Mesh in the ship Group toward white so the
      // whole hull pulses on damage.
      if (r.hitFlash > 0) {
        r.hitFlash = Math.max(0, r.hitFlash - dt * 4);
        const f = r.hitFlash;
        r.ship.traverse((o) => {
          if (o.isMesh && o.material && o.material.emissive) {
            o.material.emissive.setRGB(f, f, f);
          }
        });
      }
      // Overhead diamond marker: shows from MARKER_VISIBLE_DIST so distant
      // pilots register as a colored blip before the full target box
      // appears at closer range. Doesn't care about team — friendlies and
      // enemies both blip, you just read their color.
      if (r.marker) r.marker.visible = dist <= MARKER_VISIBLE_DIST;
      const isTeammate = r.team !== null && r.team !== undefined && r.team === myTeam;
      if (isTeammate) {
        r.box.style.display = 'none';
        r.lead.style.display = 'none';
        continue;
      }
      if (dist > TARGETING_MAX_DIST) {
        r.box.style.display = 'none';
        r.lead.style.display = 'none';
        continue;
      }
      // Line-of-sight: if any asteroid intersects the ray from us to the
      // target closer than the target itself, the target is occluded.
      // Same check the aim assist uses, so visual and lock-on stay in
      // sync — hide behind a rock and the HUD goes blind too.
      const losDx = (r.ship.position.x - ship.position.x) / dist;
      const losDy = (r.ship.position.y - ship.position.y) / dist;
      const losDz = (r.ship.position.z - ship.position.z) / dist;
      let occluded = false;
      for (const a of asteroids.list) {
        const hit = raySphereDist(
          ship.position.x, ship.position.y, ship.position.z,
          losDx, losDy, losDz,
          a.mesh.position.x, a.mesh.position.y, a.mesh.position.z,
          a.radius,
        );
        if (hit !== null && hit < dist) { occluded = true; break; }
      }
      if (!occluded) {
        // Static obstacles (moon) hide the target box too — without this,
        // the lead marker shows through the moon and the reticle locks
        // on a target the bullets can't actually reach.
        for (const o of obstacles) {
          const hit = raySphereDist(
            ship.position.x, ship.position.y, ship.position.z,
            losDx, losDy, losDz,
            o.pos.x, o.pos.y, o.pos.z, o.radius,
          );
          if (hit !== null && hit < dist) { occluded = true; break; }
        }
      }
      if (occluded) {
        r.box.style.display = 'none';
        r.lead.style.display = 'none';
        continue;
      }
      projTmp.copy(r.ship.position).project(camera);
      const behind = projTmp.z > 1 || projTmp.z < -1;
      const sx = (projTmp.x * 0.5 + 0.5) * W;
      const sy = (-projTmp.y * 0.5 + 0.5) * H;
      const offscreen = sx < -32 || sx > W + 32 || sy < -32 || sy > H + 32;
      if (behind || offscreen) {
        r.box.style.display = 'none';
        r.lead.style.display = 'none';
        continue;
      }
      anyVisible = true;
      r.box.style.display = '';
      r.box.style.left = sx + 'px';
      r.box.style.top = sy + 'px';
      const targetName = scores.get(r.id)?.name || `P${r.id}`;
      r.label.textContent = `${targetName}  HP ${r.hp}`;

      // Lead marker stays on the enemy ship itself — no motion prediction.
      // sx/sy were already projected just above for the target box.
      r.lead.style.display = '';
      r.lead.style.left = sx + 'px';
      r.lead.style.top = sy + 'px';
      const lx = sx, ly = sy;

      const dx = lx - reticleX, dy = ly - reticleY;
      const screenDist = Math.sqrt(dx * dx + dy * dy);
      r.lead.classList.toggle('aligned', screenDist < 22);
      if (screenDist < bestAlignment) bestAlignment = screenDist;
    }
    if (reticleEl) {
      reticleEl.classList.toggle('locked', anyVisible && bestAlignment < 22);
    }

    if (hud) {
      const tPct = Math.round((throttle / MAX_THROTTLE) * 100);
      const gunLabel = gunMode === 'beam' ? 'BEAM (3 ammo)' : 'BULLET';
      const status = myAlive
        ? `Throttle: ${tPct}%${boosting ? '  [BOOST]' : ''}   Gun: ${gunLabel}`
        : `Respawning…`;
      const x = ship.position.x.toFixed(0);
      const y = ship.position.y.toFixed(0);
      const z = ship.position.z.toFixed(0);
      hud.textContent =
        `${status}   Speed: ${shipVelocity.length().toFixed(1)} u/s   X:${x} Y:${y} Z:${z}   Asteroids: ${asteroids.list.length}   Players: ${remotePlayers.size + 1}`;
    }

    if (hpFill && hpText) {
      const pct = Math.max(0, myHp / SHIP_MAX_HP);
      hpFill.style.width = (pct * 100).toFixed(1) + '%';
      // Hue shifts green → yellow → red as HP drops.
      const hue = Math.round(pct * 120);
      hpFill.style.background = `linear-gradient(180deg, hsl(${hue}, 80%, 60%) 0%, hsl(${hue}, 70%, 38%) 100%)`;
      hpText.textContent = `${myHp} / ${SHIP_MAX_HP}`;
    }
    // Hit vignette: spike on any HP drop while alive, decay otherwise.
    if (myAlive && myHp < prevHpForFlash) {
      vignetteAlpha = 1;
    }
    prevHpForFlash = myHp;
    vignetteAlpha = Math.max(0, vignetteAlpha - VIGNETTE_DECAY * dt);
    if (hitVignette) hitVignette.style.opacity = vignetteAlpha.toFixed(3);
    // Spawn protection: tick down + flicker the ship at 6Hz so the player
    // can see they're invulnerable. When timer hits 0 the ship goes solid.
    if (myInvulnTimer > 0) {
      myInvulnTimer = Math.max(0, myInvulnTimer - dt);
      if (myAlive) {
        ship.visible = (Math.floor(performance.now() * 0.012) % 2 === 0);
        if (myInvulnTimer === 0) ship.visible = true;
      }
    }
    if (deathBanner) {
      deathBanner.style.display = myAlive ? 'none' : 'block';
    }
  }

  // Brief on-screen toast when aim assist is toggled. Reuses (or creates)
  // a single floating div so repeated toggles just refresh the text.
  let aimAssistToastEl = null;
  let aimAssistToastTimer = null;
  function showAimAssistToast(on) {
    if (!aimAssistToastEl) {
      aimAssistToastEl = document.createElement('div');
      aimAssistToastEl.style.cssText =
        'position:fixed;top:60px;left:50%;transform:translateX(-50%);' +
        'padding:8px 18px;border-radius:10px;border:2px solid #b0e0ff;' +
        'background:rgba(8,14,28,0.85);color:#ffe07a;font-size:18px;' +
        'letter-spacing:2px;z-index:5;pointer-events:none;' +
        'box-shadow:0 0 12px rgba(80,160,255,0.4);';
      document.body.appendChild(aimAssistToastEl);
    }
    aimAssistToastEl.textContent = `AIM ASSIST: ${on ? 'ON' : 'OFF'}`;
    aimAssistToastEl.style.display = 'block';
    if (aimAssistToastTimer) clearTimeout(aimAssistToastTimer);
    aimAssistToastTimer = setTimeout(() => {
      if (aimAssistToastEl) aimAssistToastEl.style.display = 'none';
    }, 1500);
  }

  const killfeedEl = document.getElementById('killfeed');
  function pushKillFeed(killerName, victimName, isYouKiller, isYouVictim) {
    if (!killfeedEl) return;
    const entry = document.createElement('div');
    entry.className = 'kf-entry';

    const kEl = document.createElement('span');
    kEl.className = 'kf-killer' + (isYouKiller ? ' kf-you' : '');
    kEl.textContent = killerName;

    const iEl = document.createElement('span');
    iEl.className = 'kf-icon';
    iEl.textContent = '→';

    const vEl = document.createElement('span');
    vEl.className = 'kf-victim' + (isYouVictim ? ' kf-you' : '');
    vEl.textContent = victimName;

    entry.appendChild(kEl);
    entry.appendChild(iEl);
    entry.appendChild(vEl);
    killfeedEl.insertBefore(entry, killfeedEl.firstChild);
    while (killfeedEl.children.length > 5) killfeedEl.removeChild(killfeedEl.lastChild);

    setTimeout(() => {
      entry.classList.add('kf-fading');
      setTimeout(() => { if (entry.parentNode) entry.parentNode.removeChild(entry); }, 420);
    }, 3600);
  }

  // Aim assist: damped magnetic pull toward closest enemy in the forward
  // cone. Two layers of smoothing prevent jank:
  //   1. assistStrengthSmoothed ramps up/down as targets enter/exit cone.
  //   2. assistTargetDir lerps toward the current target's lead point, so
  //      switching targets doesn't snap — the pull arcs across smoothly.
  // Lead correction: the pull aims at where the bullet will arrive, not
  // where the enemy currently is. That's the difference between "the
  // crosshair tracks them" and "your shots actually hit."
  const _assistFwd = new THREE.Vector3();
  const _assistTo = new THREE.Vector3();
  const _assistLead = new THREE.Vector3();
  const _assistAxis = new THREE.Vector3();
  const _assistQ = new THREE.Quaternion();
  const assistTargetDir = new THREE.Vector3();
  let assistStrengthSmoothed = 0;
  let assistHasTarget = false;
  let lastAssistTargetId = null;
  function applyAimAssist(dt, steerMag = 0) {
    if (!myAlive) {
      assistStrengthSmoothed = 0;
      assistHasTarget = false;
      return;
    }
    // Player-intent damper: any deliberate steering scales pull down so
    // the lock yields when you try to switch targets or evade. Quadratic
    // so light corrections leave most of the help intact, but moderate
    // input slips it fast.
    const intentDamp = Math.max(0, 1 - steerMag / ASSIST_INTENT_BREAK);
    const intentFactor = intentDamp * intentDamp;
    if (intentFactor <= 0) {
      assistStrengthSmoothed = THREE.MathUtils.damp(assistStrengthSmoothed, 0, 6, dt);
      assistHasTarget = false;
      return;
    }
    _assistFwd.set(0, 0, 1).applyQuaternion(ship.quaternion);

    // Find the best target this frame. Aim/score against each enemy's
    // lead point (where a bullet would actually intercept them) so the
    // assist commits to the same point that bullets will hit. Sticky
    // bonus on whoever was the previous target keeps the lock from
    // hopping between equidistant enemies.
    let bestDot = ASSIST_CONE_DOT;
    let bestTarget = null;
    let bestLead = null;
    for (const r of remotePlayers.values()) {
      if (!r.alive || !r.hasTarget) continue;
      if (r.team !== null && r.team !== undefined && r.team === myTeam) continue;
      // Lead-corrected aim point. solveIntercept can return null for
      // unreachable targets (e.g. fleeing faster than bullets) — fall
      // back to the raw position in that case.
      const t = solveIntercept(r.ship.position, r.vel, ship.position, shipVelocity, BULLET_SPEED);
      if (t !== null && t > 0 && Number.isFinite(t)) {
        _assistLead.copy(r.vel).multiplyScalar(t).add(r.ship.position);
      } else {
        _assistLead.copy(r.ship.position);
      }
      _assistTo.subVectors(_assistLead, ship.position);
      const dist = _assistTo.length();
      if (dist > ASSIST_RANGE || dist < ASSIST_MIN_RANGE) continue;
      _assistTo.divideScalar(dist);
      // Line-of-sight: if any asteroid OR static obstacle intersects the
      // ray to the lead point inside that distance, the target is blocked
      // — skip. This mirrors what the player's actual bullets would do
      // (rocks + moon block shots) so the lock can't drag your nose
      // through cover.
      let blocked = false;
      for (const a of asteroids.list) {
        const hit = raySphereDist(
          ship.position.x, ship.position.y, ship.position.z,
          _assistTo.x, _assistTo.y, _assistTo.z,
          a.mesh.position.x, a.mesh.position.y, a.mesh.position.z,
          a.radius,
        );
        if (hit !== null && hit < dist) { blocked = true; break; }
      }
      if (!blocked) {
        for (const o of obstacles) {
          const hit = raySphereDist(
            ship.position.x, ship.position.y, ship.position.z,
            _assistTo.x, _assistTo.y, _assistTo.z,
            o.pos.x, o.pos.y, o.pos.z, o.radius,
          );
          if (hit !== null && hit < dist) { blocked = true; break; }
        }
      }
      if (blocked) continue;
      let d = _assistFwd.dot(_assistTo);
      if (r.id === lastAssistTargetId) d += ASSIST_STICKY_DOT_BONUS;
      if (d > bestDot) {
        bestDot = d;
        bestTarget = r;
        bestLead = _assistLead.clone();
      }
    }

    // Smooth on/off as a target enters/leaves the cone — the actual pull
    // strength comes from the angle profile below.
    const targetPresence = bestTarget ? 1 : 0;
    assistStrengthSmoothed = THREE.MathUtils.damp(assistStrengthSmoothed, targetPresence, 6, dt);

    if (bestTarget) {
      _assistTo.subVectors(bestLead, ship.position).normalize();
      if (!assistHasTarget || lastAssistTargetId !== bestTarget.id) {
        // First frame seeing this target — seed instead of lerping from a
        // stale direction so the pull starts in the right place.
        assistTargetDir.copy(_assistTo);
      } else {
        // Tighter lerp than before (12 vs 8) — with sticky targeting the
        // direction doesn't need slack for cross-target arcs, and faster
        // tracking means the lead point stays accurate as the enemy moves.
        assistTargetDir.lerp(_assistTo, 1 - Math.exp(-12 * dt));
        assistTargetDir.normalize();
      }
      assistHasTarget = true;
      lastAssistTargetId = bestTarget.id;
    } else {
      assistHasTarget = false;
      lastAssistTargetId = null;
    }

    if (assistStrengthSmoothed < 0.01 || !assistHasTarget) return;

    const angle = _assistFwd.angleTo(assistTargetDir);
    // Dead zone: don't pull at all when crosshair is on/near target so
    // arrow-key fine-aim stays free.
    if (angle <= ASSIST_DEAD_ANGLE) return;
    // Falloff zone: pull strength ramps from 0 (at the dead-angle edge)
    // up to full (at FALLOFF_START and beyond). So the pull is strongest
    // while you're swinging onto the target — exactly when help is most
    // useful — and fades smoothly to nothing as the crosshair approaches.
    let strengthMult;
    if (angle >= ASSIST_FALLOFF_START) {
      strengthMult = 1.0;
    } else {
      strengthMult = (angle - ASSIST_DEAD_ANGLE) /
        (ASSIST_FALLOFF_START - ASSIST_DEAD_ANGLE);
    }
    const fadedAngle = angle - ASSIST_DEAD_ANGLE;
    const step = Math.min(
      fadedAngle,
      ASSIST_STRENGTH * assistStrengthSmoothed * strengthMult * intentFactor * dt,
    );
    _assistAxis.crossVectors(_assistFwd, assistTargetDir);
    if (_assistAxis.lengthSq() < 1e-6) return;
    _assistAxis.normalize();
    _assistQ.setFromAxisAngle(_assistAxis, step);
    ship.quaternion.premultiply(_assistQ).normalize();
  }

  // Sphere-vs-AABB. Pushes the sphere out of the box along the shortest
  // separation axis and reflects velocity along the contact normal.
  function collideSphereWithBox(pos, vel, radius, boxCenter, halfSize) {
    const dx = pos.x - boxCenter.x;
    const dy = pos.y - boxCenter.y;
    const dz = pos.z - boxCenter.z;
    const inside =
      Math.abs(dx) < halfSize.x &&
      Math.abs(dy) < halfSize.y &&
      Math.abs(dz) < halfSize.z;
    let nx, ny, nz, push;
    if (inside) {
      const px = halfSize.x - Math.abs(dx);
      const py = halfSize.y - Math.abs(dy);
      const pz = halfSize.z - Math.abs(dz);
      if (px < py && px < pz) { nx = Math.sign(dx) || 1; ny = 0; nz = 0; push = px + radius; }
      else if (py < pz)        { nx = 0; ny = Math.sign(dy) || 1; nz = 0; push = py + radius; }
      else                     { nx = 0; ny = 0; nz = Math.sign(dz) || 1; push = pz + radius; }
    } else {
      const cx = Math.max(-halfSize.x, Math.min(halfSize.x, dx));
      const cy = Math.max(-halfSize.y, Math.min(halfSize.y, dy));
      const cz = Math.max(-halfSize.z, Math.min(halfSize.z, dz));
      const ox = dx - cx, oy = dy - cy, oz = dz - cz;
      const distSq = ox * ox + oy * oy + oz * oz;
      if (distSq >= radius * radius || distSq < 0.0001) return;
      const dist = Math.sqrt(distSq);
      nx = ox / dist; ny = oy / dist; nz = oz / dist;
      push = radius - dist;
    }
    pos.x += nx * push;
    pos.y += ny * push;
    pos.z += nz * push;
    const vDotN = vel.x * nx + vel.y * ny + vel.z * nz;
    if (vDotN < 0) {
      vel.x -= 1.4 * vDotN * nx;
      vel.y -= 1.4 * vDotN * ny;
      vel.z -= 1.4 * vDotN * nz;
    }
  }

  function resolveMothershipCollisions() {
    for (const m of motherships) {
      collideSphereWithBox(ship.position, shipVelocity, shipRadius, m.pos, m.halfSize);
    }
  }

  // Per-frame "currently touching" sets. Damage is applied on the rising
  // edge — the frame contact begins — so a ship wedged against a rock
  // doesn't take a hit every tick. Carried across frames; resets on
  // death (the ship is teleported out of contact anyway).
  const touchingAsteroids = new Set();
  let touchingMoon = false;
  let touchingWater = false;
  function dealSelfDamage(dmg) {
    // Respect spawn protection here as well as in applyPlayerDamageLocal
    // (the server doesn't know about invuln for self-damage messages, so
    // the guard has to live client-side to avoid being one-shot mid-
    // respawn flicker by a rock you spawned next to).
    if (myInvulnTimer > 0) return;
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: 'self-damage', dmg }));
    } else if (isSolo) {
      applyPlayerDamageLocal(dmg);
    }
  }

  function resolveCollisions() {
    const nextAsteroids = new Set();
    for (const a of asteroids.list) {
      const dx = ship.position.x - a.mesh.position.x;
      const dy = ship.position.y - a.mesh.position.y;
      const dz = ship.position.z - a.mesh.position.z;
      const distSq = dx * dx + dy * dy + dz * dz;
      const minDist = shipRadius + a.radius;
      if (distSq < minDist * minDist && distSq > 0.0001) {
        const dist = Math.sqrt(distSq);
        const nx = dx / dist, ny = dy / dist, nz = dz / dist;
        const push = minDist - dist;
        ship.position.x += nx * push;
        ship.position.y += ny * push;
        ship.position.z += nz * push;
        const vDotN = shipVelocity.x * nx + shipVelocity.y * ny + shipVelocity.z * nz;
        if (vDotN < 0) {
          shipVelocity.x -= 1.3 * vDotN * nx;
          shipVelocity.y -= 1.3 * vDotN * ny;
          shipVelocity.z -= 1.3 * vDotN * nz;
        }
        nextAsteroids.add(a);
        // Rising-edge damage: 15–29 HP per fresh contact. Tutorial is
        // exempt so a first-time pilot bumping a rock doesn't die mid-
        // lesson — same exemption the drift-overload damage uses.
        if (!touchingAsteroids.has(a) && SOLO_MODE !== 'tutorial') {
          const dmg = 15 + Math.floor(Math.random() * 15); // [15, 29]
          dealSelfDamage(dmg);
        }
      }
    }
    touchingAsteroids.clear();
    for (const a of nextAsteroids) touchingAsteroids.add(a);

    // Static obstacles (moon): same sphere bounce, but a fresh contact
    // is instantly fatal. Send a max-HP self-damage so the server kills
    // us authoritatively; solo applies locally.
    let moonContactThisFrame = false;
    for (const o of obstacles) {
      const dx = ship.position.x - o.pos.x;
      const dy = ship.position.y - o.pos.y;
      const dz = ship.position.z - o.pos.z;
      const distSq = dx * dx + dy * dy + dz * dz;
      const minDist = shipRadius + o.radius;
      if (distSq < minDist * minDist && distSq > 0.0001) {
        const dist = Math.sqrt(distSq);
        const nx = dx / dist, ny = dy / dist, nz = dz / dist;
        const push = minDist - dist;
        ship.position.x += nx * push;
        ship.position.y += ny * push;
        ship.position.z += nz * push;
        const vDotN = shipVelocity.x * nx + shipVelocity.y * ny + shipVelocity.z * nz;
        if (vDotN < 0) {
          shipVelocity.x -= 1.3 * vDotN * nx;
          shipVelocity.y -= 1.3 * vDotN * ny;
          shipVelocity.z -= 1.3 * vDotN * nz;
        }
        moonContactThisFrame = true;
        if (!touchingMoon && SOLO_MODE !== 'tutorial') {
          dealSelfDamage(SHIP_MAX_HP);
        }
      }
    }
    touchingMoon = moonContactThisFrame;

    // Terrain map: ground surface is an instant-kill floor.
    if (isTerrainMap) {
      const groundY = getTerrainHeight(ship.position.x, ship.position.z);
      const killY   = groundY + TERRAIN_KILL_CLEARANCE;
      if (ship.position.y < killY) {
        ship.position.y = killY;
        if (shipVelocity.y < 0) shipVelocity.y *= -0.5;
        if (!touchingWater && SOLO_MODE !== 'tutorial') {
          dealSelfDamage(SHIP_MAX_HP);
        }
        touchingWater = true; // reuse rising-edge flag
      } else {
        touchingWater = false;
      }
    }
  }

  // --- Solo mode wiring -------------------------------------------------
  // 'train' = 1v1 vs one bot, shorter timer so practice sessions wrap naturally.
  // Bots reuse the remote-player render pipeline; damage and respawns are
  // applied locally. Each bot is fed an `entity` for the player and other
  // bots so its targeting can pick the closest opponent.
  const SOLO_MODE = isSolo ? (opts.mode || 'train') : null;
  const myTeam = isSolo ? 0 : (opts.spawn?.team ?? 0);
  const MATCH_DURATION = SOLO_MODE === 'train' ? 180 : 300;
  const teamKills = [0, 0];
  let matchTimer = MATCH_DURATION;
  let matchOver = false;
  // Train and any networked match show the team HUD + win banner. Solo ticks
  // the timer locally; MP receives match-state from the server. Tutorial is
  // excluded — it's a guided lesson, not a match.
  const matchActive = SOLO_MODE === 'skirmish' || SOLO_MODE === 'train' || !isSolo;
  let soloBotsKilled = 0;

  // ── Achievement toast queue ───────────────────────────────────────────────
  const _achToastContainer = document.getElementById('achievement-toasts');
  let _achQueue = [];
  let _achTimer = null;

  function _flushAchQueue() {
    if (!_achQueue.length) { _achTimer = null; return; }
    const { icon, label, reward } = _achQueue.shift();
    if (_achToastContainer) {
      const toast = document.createElement('div');
      toast.className = 'ach-toast';
      const crLine = reward > 0
        ? `<span class="ach-toast-cr">+${reward.toLocaleString()} ⬡</span>`
        : '';
      toast.innerHTML =
        `<span class="ach-toast-icon">${icon}</span>` +
        `<div class="ach-toast-body">` +
          `<span class="ach-toast-title">ACHIEVEMENT UNLOCKED</span>` +
          `<span class="ach-toast-label">${label}</span>` +
          crLine +
        `</div>`;
      _achToastContainer.appendChild(toast);
      setTimeout(() => toast.remove(), 3700);
    }
    _achTimer = setTimeout(_flushAchQueue, 900);
  }

  function queueAchievementToasts(earned) {
    if (!Array.isArray(earned) || !earned.length) return;
    _achQueue.push(...earned);
    if (!_achTimer) _flushAchQueue();
  }

  function stashAchievementsForHangar(earned) {
    if (!Array.isArray(earned) || !earned.length) return;
    try {
      const existing = JSON.parse(localStorage.getItem('spaceships:pendingAchs') || '[]');
      localStorage.setItem('spaceships:pendingAchs', JSON.stringify([...existing, ...earned]));
    } catch {}
  }

  function updateCachedCredits(total) {
    if (Number.isFinite(total)) localStorage.setItem('spaceships:credits', String(total));
  }

  async function reportSoloResult(kills, deaths, won, botsKilled) {
    const token = localStorage.getItem('spaceships:token');
    if (!token) return;
    try {
      const res  = await fetch('/spaceships/api/solo-result', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'Authorization': 'Bearer ' + token },
        body: JSON.stringify({ kills, deaths, won, botsKilled }),
      });
      const data = await res.json();
      if (data.ok) {
        if (data.newAchievements?.length) {
          queueAchievementToasts(data.newAchievements);
          stashAchievementsForHangar(data.newAchievements);
        }
        updateCachedCredits(data.totalCredits);
      }
    } catch (e) {
      console.warn('[solo-result] could not report:', e);
    }
  }

  async function reportTrialTime(trialNum, time) {
    const token = localStorage.getItem('spaceships:token');
    if (!token) return;
    try {
      const res  = await fetch('/spaceships/api/trial-result', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'Authorization': 'Bearer ' + token },
        body: JSON.stringify({ trialNum, time }),
      });
      const data = await res.json();
      if (data.ok) {
        if (data.newAchievements?.length) {
          queueAchievementToasts(data.newAchievements);
          stashAchievementsForHangar(data.newAchievements);
        }
        updateCachedCredits(data.totalCredits);
      }
    } catch (e) {
      console.warn('[trial-result] could not report:', e);
    }
  }

  function makeBotEntity(r) {
    return {
      id: r.id,
      get team() { return r.team; },
      get position() { return r.ship.position; },
      get velocity() { return r.vel; },
      get alive() { return r.alive; },
      takeHit(dmg, killerId, killerTeam) {
        applyHitToBot(r.id, dmg, killerId, killerTeam);
      },
    };
  }
  const playerEntity = {
    id: opts.you,
    team: myTeam,
    get position() { return ship.position; },
    get velocity() { return shipVelocity; },
    get alive() { return myAlive; },
    takeHit(dmg, killerId, killerTeam) {
      applyPlayerDamageLocal(dmg, killerId, killerTeam);
    },
  };

  const bots = []; // { id, team, record, ai, entity }
  function spawnBot(id, team, position, name) {
    const r = getOrCreateRemote(id);
    r.isBot = true;
    r.team = team;
    refreshMarker(r);
    r.alive = true;
    r.hasTarget = true;
    r.hp = SHIP_MAX_HP;
    r.respawnTimer = 0;
    r.ship.position.copy(position);
    r.targetPos.copy(position);
    r.targetQuat.copy(r.ship.quaternion);
    const entity = makeBotEntity(r);
    const ai = createBotAI(r, {
      team,
      faction: team === myTeam ? 'ally' : 'enemy',
      beams,
      bullets,
      asteroids,
      obstacles,
      solveIntercept,
      raySphereDist,
      audio,
      distanceVol,
      hardMode: !!opts.hardMode,
      terrainHeightFn: isTerrainMap ? getTerrainHeight : null,
      getOpponents: () => {
        const out = [];
        if (playerEntity.team !== team) out.push(playerEntity);
        for (const b of bots) if (b.team !== team) out.push(b.entity);
        return out;
      },
    });
    const bot = { id, team, record: r, ai, entity };
    bots.push(bot);
    if (!scores.has(id)) {
      scores.set(id, { name: name || `Bot ${id}`, kills: 0, deaths: 0 });
    }
    return bot;
  }

  function spawnSoloEntities() {
    if (SOLO_MODE === 'train') {
      const fwd = new THREE.Vector3(0, 0, 1).applyQuaternion(ship.quaternion);
      const pos = ship.position.clone().addScaledVector(fwd, 250);
      spawnBot(1, 1, pos, 'Bot');
    } else if (SOLO_MODE === 'skirmish') {
      const FRIENDLY_ANCHOR = isTerrainMap ? new THREE.Vector3(0, 40, -1400) : new THREE.Vector3(0, 0, -540);
      const ENEMY_ANCHOR    = isTerrainMap ? new THREE.Vector3(0, 40,  1400) : new THREE.Vector3(0, 0,  540);
      const jitter = (range) => (Math.random() - 0.5) * range;
      for (let i = 0; i < 4; i++) {
        const pos = FRIENDLY_ANCHOR.clone().add(new THREE.Vector3(jitter(80), jitter(30), jitter(80)));
        spawnBot(1 + i, 0, pos, `Ally ${i + 1}`);
      }
      for (let i = 0; i < 5; i++) {
        const pos = ENEMY_ANCHOR.clone().add(new THREE.Vector3(jitter(80), jitter(30), jitter(80)));
        spawnBot(5 + i, 1, pos, `Enemy ${i + 1}`);
      }
    }
  }
  if (isSolo) spawnSoloEntities();

  // ---- Tutorial mode ---------------------------------------------------
  // Step machine that gates progression on real input/state changes, with
  // an on-screen prompt. After all steps complete the player gets 20s of
  // free flight, then the page reloads back to the lobby. Player is
  // damage-immune throughout (no enemies, drift-overload disabled).
  const isTutorial = SOLO_MODE === 'tutorial';
  const tutorial = isTutorial ? createTutorial() : null;
  function createTutorial() {
    const panel = document.getElementById('tutorial-panel');
    const stepEl = document.getElementById('tutorial-step');
    const promptEl = document.getElementById('tutorial-prompt');
    const hintEl = document.getElementById('tutorial-hint');
    const fillEl = document.getElementById('tutorial-progress-fill');
    if (panel) panel.style.display = 'block';

    const steerHintMouse = 'Move the mouse to aim — yaw the ship 30° to either side.';
    const steerHintKeys = 'Hold Left or Right arrow to yaw the ship 30°.';
    let initialFwd = null;
    let initialUp = null;
    const tmpVec = new THREE.Vector3();

    // Helpers each step reads via closure.
    const steps = [
      {
        prompt: 'Throttle Up',
        hint: 'Press W to accelerate.',
        check: () => throttle > 25,
      },
      {
        prompt: 'Throttle Down',
        hint: 'Press S to slow down.',
        check: () => targetThrottle < 5,
      },
      {
        prompt: 'Steer',
        hint: noMouseMode ? steerHintKeys : steerHintMouse,
        onEnter() {
          initialFwd = new THREE.Vector3(0, 0, 1).applyQuaternion(ship.quaternion);
        },
        check: () => {
          tmpVec.set(0, 0, 1).applyQuaternion(ship.quaternion);
          const dot = Math.max(-1, Math.min(1, tmpVec.dot(initialFwd)));
          return Math.acos(dot) > Math.PI * (30 / 180);
        },
      },
      {
        prompt: 'Roll',
        hint: 'Hold A or D to roll left/right (30°).',
        onEnter() {
          initialUp = new THREE.Vector3(0, 1, 0).applyQuaternion(ship.quaternion);
        },
        check: () => {
          tmpVec.set(0, 1, 0).applyQuaternion(ship.quaternion);
          const dot = Math.max(-1, Math.min(1, tmpVec.dot(initialUp)));
          return Math.acos(dot) > Math.PI * (30 / 180);
        },
      },
      {
        prompt: 'Boost',
        hint: 'Hold Shift to boost forward.',
        state: { t: 0 },
        onEnter() { this.state.t = 0; },
        check(dt) {
          if (input.keys.has('ShiftLeft') || input.keys.has('ShiftRight')) this.state.t += dt;
          return this.state.t > 0.6;
        },
      },
      {
        prompt: 'Drift',
        hint: 'Hold Space to drift — keep momentum while you rotate freely.',
        state: { t: 0 },
        onEnter() { this.state.t = 0; },
        check(dt) {
          if (input.keys.has('Space')) this.state.t += dt;
          return this.state.t > 0.8;
        },
      },
      {
        prompt: 'Fire',
        hint: noMouseMode ? 'Press F to fire.' : 'Left-click or press F to fire.',
        state: { ammoAtEnter: 0 },
        onEnter() { this.state.ammoAtEnter = ammo; },
        check() { return ammo < this.state.ammoAtEnter; },
      },
      {
        prompt: 'Switch Weapon',
        hint: 'Press P to toggle bullets / beam.',
        state: { startMode: '' },
        onEnter() { this.state.startMode = gunMode; },
        check() { return gunMode !== this.state.startMode; },
      },
      {
        prompt: 'Aim Assist',
        hint: 'Press C to toggle aim assist on / off.',
        state: { startVal: false },
        onEnter() { this.state.startVal = aimAssistEnabled; },
        check() { return aimAssistEnabled !== this.state.startVal; },
      },
      {
        prompt: 'Fullscreen',
        hint: 'Press L to enter fullscreen.',
        check: () => !!document.fullscreenElement,
      },
      {
        prompt: 'Free Flight',
        hint: 'Try everything together — returning to menu shortly.',
        state: { t: 0 },
        onEnter() { this.state.t = 0; },
        check(dt) { this.state.t += dt; return this.state.t > 20; },
        showCountdown: true,
      },
    ];

    let idx = -1;
    let advanced = false;

    function show(step) {
      stepEl.textContent = `STEP ${idx + 1} / ${steps.length}`;
      promptEl.textContent = step.prompt;
      hintEl.textContent = step.hint;
      fillEl.style.width = '0%';
    }

    function advance() {
      idx += 1;
      if (idx >= steps.length) {
        finish();
        return;
      }
      const step = steps[idx];
      if (typeof step.onEnter === 'function') step.onEnter();
      show(step);
    }

    function finish() {
      promptEl.textContent = 'Complete!';
      hintEl.textContent = 'Returning to menu…';
      fillEl.style.width = '100%';
      setTimeout(() => { window.location.reload(); }, 1200);
    }

    function update(dt) {
      if (idx < 0) { advance(); return; }
      if (idx >= steps.length) return;
      const step = steps[idx];
      // Pass dt to per-step check (held-key timers, free-flight countdown).
      const done = step.check(dt);
      if (step.showCountdown) {
        const left = Math.max(0, 20 - (step.state.t || 0));
        hintEl.textContent = `Try everything together — returning in ${left.toFixed(1)}s.`;
        fillEl.style.width = ((1 - left / 20) * 100).toFixed(1) + '%';
      }
      if (done) advance();
    }

    return { update, isActive: () => idx >= 0 && idx < steps.length };
  }

  function applyHitToBot(id, dmg, killerId, killerTeam) {
    const r = remotePlayers.get(id);
    if (!r || !r.alive) return;
    r.hp = Math.max(0, r.hp - dmg);
    r.hitFlash = 1;
    if (r.hp <= 0) {
      audio.play('shipdeath', distanceVol(r.ship.position));
      killRemote(id);
      r.respawnTimer = RESPAWN_DELAY;
      if (matchActive && killerTeam !== undefined && killerTeam !== null) {
        teamKills[killerTeam] = (teamKills[killerTeam] || 0) + 1;
      }
      if (killerId !== undefined && killerId !== null) {
        const ks = scores.get(killerId);
        if (ks) ks.kills += 1;
      }
      if (isSolo && killerId === opts.you && r.isBot) soloBotsKilled++;
      const ds = scores.get(id);
      if (ds) ds.deaths += 1;
      if (killerId !== undefined && killerId !== null) {
        const kn = scores.get(killerId)?.name || 'Pilot';
        const vn = scores.get(id)?.name || 'Bot';
        pushKillFeed(kn, vn, killerId === opts.you, false);
      }
      renderScoreboard();
      renderMatchHud();
    } else {
      const b = bots.find((b) => b.id === id);
      if (b && b.ai) b.ai.notifyHit();
    }
  }

  function applyPlayerDamageLocal(dmg, killerId, killerTeam) {
    if (!myAlive) return;
    if (myInvulnTimer > 0) return; // spawn protection
    healthIdleDamage = 0;
    myHp = Math.max(0, myHp - dmg);
    if (myHp <= 0) {
      audio.play('shipdeath');
      killSelf();
      myRespawnTimer = RESPAWN_DELAY;
      if (matchActive && killerTeam !== undefined && killerTeam !== null) {
        teamKills[killerTeam] = (teamKills[killerTeam] || 0) + 1;
      }
      if (killerId !== undefined && killerId !== null) {
        const ks = scores.get(killerId);
        if (ks) ks.kills += 1;
      }
      const me = scores.get(opts.you);
      if (me) me.deaths += 1;
      if (killerId !== undefined && killerId !== null) {
        const kn = scores.get(killerId)?.name || 'Bot';
        const vn = scores.get(opts.you)?.name || opts.pilotName || 'Pilot';
        pushKillFeed(kn, vn, false, true);
      }
      renderScoreboard();
      renderMatchHud();
    }
  }

  function reviveBotLocal(id) {
    const r = remotePlayers.get(id);
    if (!r) return;
    let anchor;
    if (SOLO_MODE === 'skirmish') {
      anchor = r.team === 0
        ? (isTerrainMap ? new THREE.Vector3(0, 40, -1400) : new THREE.Vector3(0, 0, -540))
        : (isTerrainMap ? new THREE.Vector3(0, 40,  1400) : new THREE.Vector3(0, 0,  540));
    } else {
      anchor = ship.position.clone().add(new THREE.Vector3(
        (Math.random() * 2 - 1), 0, (Math.random() * 2 - 1),
      ).normalize().multiplyScalar(280));
    }
    r.ship.position.copy(anchor).add(new THREE.Vector3(
      (Math.random() - 0.5) * 60,
      (Math.random() - 0.5) * 20,
      (Math.random() - 0.5) * 60,
    ));
    r.targetPos.copy(r.ship.position);
    r.ship.quaternion.identity();
    r.targetQuat.copy(r.ship.quaternion);
    r.vel.set(0, 0, 0);
    r.hp = SHIP_MAX_HP;
    r.alive = true;
    r.ship.visible = true;
    const b = bots.find((b) => b.id === id);
    if (b && b.ai) b.ai.notifyRespawn();
  }

  function revivePlayerLocal() {
    myInvulnTimer = SPAWN_INVULN_DURATION;
    let pos, quat;
    if (isSolo) {
      if (isTrialsMode) {
        pos = [0, 20, -510];
        quat = [0, 0, 0, 1];
        trialsRunning = false;
        trialsTimer = 0;
        trialsNextCp = 0;
        cpCooldown = 2.0;
        for (let i = 0; i < cpMeshes.length; i++) {
          cpMeshes[i].material.color.setHex(i === 0 ? 0x66ffcc : 0x224466);
          cpMeshes[i].material.opacity = i === 0 ? 0.85 : 0.35;
        }
        for (const d of tracerDots) d.visible = false;
        updateTrialsHud();
      } else {
        const spawnZ = isTerrainMap ? -1400 : -540;
        const spawnY = isTerrainMap ? 40     : 0;
        pos = [
          (Math.random() - 0.5) * 60,
          spawnY + (Math.random() - 0.5) * 20,
          spawnZ + (Math.random() - 0.5) * 60,
        ];
        quat = [0, 0, 0, 1];
      }
    } else {
      pos = opts.spawn?.pos ?? [0, 0, 0];
      quat = opts.spawn?.quat ?? [0, 0, 0, 1];
    }
    reviveSelf(pos, quat);
  }

  // --- Match HUD (timer + team score) ---
  const matchHudEl = document.getElementById('matchhud');
  const matchTimerEl = document.getElementById('matchtimer');
  const team0ScoreEl = document.getElementById('team0score');
  const team1ScoreEl = document.getElementById('team1score');
  const matchResultEl = document.getElementById('matchresult');
  function fmtTime(t) {
    const s = Math.max(0, Math.ceil(t));
    const m = Math.floor(s / 60);
    const r = s % 60;
    return `${m}:${r.toString().padStart(2, '0')}`;
  }
  function renderMatchHud() {
    if (!matchActive) return;
    if (team0ScoreEl) team0ScoreEl.textContent = teamKills[0];
    if (team1ScoreEl) team1ScoreEl.textContent = teamKills[1];
    if (matchTimerEl) matchTimerEl.textContent = fmtTime(matchTimer);
  }
  if (matchActive && matchHudEl) {
    matchHudEl.style.display = 'flex';
    renderMatchHud();
  }

  // --- Trials HUD ----------------------------------------------------------
  const trialsHudEl          = document.getElementById('trials-hud');
  const trialsTimerEl        = document.getElementById('trials-timer');
  const trialsCpEl           = document.getElementById('trials-checkpoint');
  const trialsBestEl         = document.getElementById('trials-best');
  const trialsLastEl         = document.getElementById('trials-last');
  const trialsLapEl          = document.getElementById('trials-lap');
  function fmtLapTime(t) {
    const total = Math.max(0, t);
    const m = Math.floor(total / 60);
    const s = (total % 60).toFixed(3).padStart(6, '0');
    return `${m}:${s}`;
  }
  function updateTrialsHud() {
    if (!trialsTimerEl) return;
    trialsTimerEl.textContent = trialsRunning ? fmtLapTime(trialsTimer) : '0:00.000';
    if (trialsLapEl) trialsLapEl.textContent = `LAP ${trialsLap || 1}`;
    if (trialsCpEl) trialsCpEl.textContent = `CHECKPOINT ${trialsNextCp + 1} / ${TRIAL_CPS.length}`;
    if (trialsBestEl) trialsBestEl.textContent = trialsBestLap !== null ? `Best: ${fmtLapTime(trialsBestLap)}` : '';
    if (trialsLastEl) trialsLastEl.textContent = trialsLastLap !== null ? `Last: ${fmtLapTime(trialsLastLap)}` : '';
  }
  if (isTrialsMode && trialsHudEl) {
    trialsHudEl.style.display = 'flex';
    updateTrialsHud();
  }

  function endMatch() {
    matchOver = true;

    if (isSolo && matchActive) {
      const myScore = scores.get(opts.you) || { kills: 0, deaths: 0 };
      const won = teamKills[0] > teamKills[1] ? true : teamKills[1] > teamKills[0] ? false : null;
      reportSoloResult(myScore.kills, myScore.deaths, won, soloBotsKilled);
    }

    let title;
    if (teamKills[0] > teamKills[1]) title = 'BLUE WINS';
    else if (teamKills[1] > teamKills[0]) title = 'RED WINS';
    else title = 'DRAW';
    if (matchResultEl) {
      matchResultEl.innerHTML =
        `${title}<span class="sub">${teamKills[0]} – ${teamKills[1]}</span>` +
        `<button class="lobby-btn" id="btnBackToLobby">Back to Lobby</button>`;
      matchResultEl.style.display = 'block';
      // Reload restores the pristine lobby — simpler than tearing down the
      // running scene + WebSocket + bots one piece at a time.
      const btn = matchResultEl.querySelector('#btnBackToLobby');
      if (btn) btn.addEventListener('click', () => {
        const overlay = document.getElementById('ad-overlay');
        const skipBtn = document.getElementById('ad-skip');
        if (overlay && skipBtn) {
          skipBtn.onclick = () => location.reload();
          overlay.style.display = 'flex';
          try { (window.adsbygoogle = window.adsbygoogle || []).push({}); } catch {}
        } else {
          location.reload();
        }
      });
    }
  }

  // TAB toggles the scoreboard. One press shows; press again hides. Key
  // repeat is ignored so holding doesn't ping-pong. Listener is in capture
  // phase so it pre-empts any focusable element that might steal Tab.
  window.addEventListener('keydown', (e) => {
    if (e.code !== 'Tab' || e.repeat) return;
    e.preventDefault();
    if (scoreboardEl) scoreboardEl.classList.toggle('visible');
  }, true);

  function loop() {
    try {
      const dt = Math.min(0.05, clock.getDelta());
      update(dt);
      touchHud.update();
      renderFrame();
    } catch (err) {
      console.error('Game loop error:', err);
    }
    requestAnimationFrame(loop);
  }

  window.addEventListener('resize', () => {
    camera.aspect = window.innerWidth / window.innerHeight;
    camera.updateProjectionMatrix();
    renderer.setSize(window.innerWidth, window.innerHeight);
    if (pixelRT) {
      pixelRT.setSize(
        Math.max(1, Math.floor(window.innerWidth / PIXEL_SCALE)),
        Math.max(1, Math.floor(window.innerHeight / PIXEL_SCALE)),
      );
    }
  });

  loop();
}
