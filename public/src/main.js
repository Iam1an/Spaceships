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
import { FirstPersonCamera } from './fpcamera.js';
import { getCockpitProfile, createCockpit } from './cockpit.js';
import { Input } from './input.js';
import { createAudio } from './audio.js';
import { createBotAI } from './bot.js';
import { createTouchHud } from './touchhud.js';
import { getSavedShipColor, getSavedAccentColor, getSavedTrailColor, getSavedTrailShape } from './customization.js';
import { createWarpEffect } from './warp.js';
import {
  ULTRA, rendererParams, configureRenderer, createComposer, createUltraSkybox,
  applyEnvironment, applySkyEnvironment, sweepScene, installSpaceLights,
  upgradeTerrainSun, installContextLossGuard, contextLossMessage,
} from './graphics.js';
let started = false;
export async function startGame(opts = {}) {
  if (started) return;
  started = true;
  const scene = new THREE.Scene();
  const renderer = new THREE.WebGLRenderer(rendererParams({ antialias: true }));
  renderer.setPixelRatio(Math.min(window.devicePixelRatio, 1.5));
  renderer.setSize(window.innerWidth, window.innerHeight);
  renderer.shadowMap.enabled = true;
  renderer.shadowMap.type = THREE.BasicShadowMap;
  configureRenderer(renderer);
  document.body.appendChild(renderer.domElement);
  let contextLost = false;
  installContextLossGuard(renderer, () => {
    contextLost = true;
    document.body.appendChild(contextLossMessage());
  });
  const camera = new THREE.PerspectiveCamera(75, window.innerWidth / window.innerHeight, 0.1, 2500);
  const BASE_FOV = camera.fov;
  // The retro filter and the ultra pipeline are opposing looks; ultra wins.
  const pixelEnabled = !ULTRA && localStorage.getItem('spaceships:pixelFilter') !== '0';
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
  const ultraFx = createComposer(renderer, scene, camera);
  function renderFrame(dt = 0.016) {
    // Drawing into a lost context throws on every frame; the guard has already
    // put an explanation on screen.
    if (contextLost) return;
    if (ultraFx) {
      ultraFx.render(dt);
    } else if (pixelEnabled) {
      renderer.setRenderTarget(pixelRT);
      renderer.render(scene, camera);
      renderer.setRenderTarget(null);
      renderer.render(postScene, postCamera);
    } else {
      renderer.render(scene, camera);
    }
  }
  const clock = new THREE.Clock();
  const warpEffect = createWarpEffect(scene, camera);
  let isLoading = true;
  function loadingLoop() {
    if (!isLoading) return;
    const dt = Math.min(0.05, clock.getDelta());
    warpEffect.update(dt);
    renderFrame(dt);
    requestAnimationFrame(loadingLoop);
  }
  loadingLoop();
  try { await loadShipModel(); } catch (e) { console.warn('[ship] GLB load failed, using primitives', e); }
  isLoading = false;
  const ADMIN_MODEL_URL = 'spaceshipADMIN.glb';
  const adminModelReady = loadShipModel(ADMIN_MODEL_URL).catch(() => null);
  const MAP_TYPE = opts.map || 'space';
  const isTerrainMap = MAP_TYPE === 'terrain';
  if (isTerrainMap) camera.far = 5000;
  camera.updateProjectionMatrix();
  let terrainSun = null;
  if (isTerrainMap) {
    scene.add(new THREE.AmbientLight(0xfff8e8, ULTRA ? 0.28 : 0.60));
    terrainSun = new THREE.DirectionalLight(0xfff5cc, 1.4);
    terrainSun.position.set(0, 500, 0);
    terrainSun.castShadow = true;
    terrainSun.shadow.mapSize.set(1024, 1024);
    terrainSun.shadow.camera.left = -150;
    terrainSun.shadow.camera.right = 150;
    terrainSun.shadow.camera.top = 150;
    terrainSun.shadow.camera.bottom = -150;
    terrainSun.shadow.camera.near = 1;
    terrainSun.shadow.camera.far = 700;
    scene.add(terrainSun.target);
    scene.add(terrainSun);
    scene.background = new THREE.Color(0x6fa8d4);
    scene.fog = new THREE.Fog(0xbbd5f0, 1400, 4800);
    upgradeTerrainSun(terrainSun);
    applySkyEnvironment(scene, renderer, 0x6fa8d4, 0x4a4335);
  } else if (ULTRA) {
    installSpaceLights(scene);
    const sky = createUltraSkybox();
    scene.background = sky;
    applyEnvironment(scene, renderer, sky);
  } else {
    scene.add(new THREE.AmbientLight(0xffffff, 0.35));
    const sun = new THREE.DirectionalLight(0xffffff, 1.1);
    sun.position.set(200, 300, 100);
    scene.add(sun);
    scene.background = createSkybox();
  }
  const isTrialsMode = !!(opts.solo && opts.mode && opts.mode.startsWith('trials'));
  const isCampaign = !!(opts.solo && opts.mode === 'campaign');
  const CAMPAIGN_MISSION = isCampaign ? (opts.missionId ?? 1) : 1;
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
  if (isCampaign) {
    platformB.visible = false;
  }
  const terrainMesh = isTerrainMap ? createTerrain() : null;
  if (terrainMesh) {
    terrainMesh.receiveShadow = true;
    scene.add(terrainMesh);
  }
  if (isTerrainMap) createTrees(scene);
  const clouds = isTerrainMap ? createClouds(scene) : null;
  const MOON_RADIUS = 80;
  const moon = isTerrainMap ? null : createMoon({ radius: MOON_RADIUS, position: [0, 0, 0] });
  if (moon) scene.add(moon.mesh);
  const obstacles = moon ? [{ pos: moon.pos, radius: MOON_RADIUS }] : [];
  const moonAvoid = moon
    ? { pos: moon.pos, halfSize: new THREE.Vector3(MOON_RADIUS, MOON_RADIUS, MOON_RADIUS) }
    : null;
  const SHIP_SCALE = 1.5;
  const savedHull = parseInt(getSavedShipColor().replace('#', ''), 16);
  const savedAccent = parseInt(getSavedAccentColor().replace('#', ''), 16);
  const ADMIN_SHIP_NAMES = new Set(['Admin', 'ariairspeed']);
  const localPlayerName = (opts.pilotName || '').trim();
  const isLocalAdmin = ADMIN_SHIP_NAMES.has(localPlayerName) || localStorage.getItem('spaceships:unlock_admin_ship') === '1';
  const ship = createShip({
    hullColor: savedHull,
    accentColor: savedAccent,
    modelUrl: isLocalAdmin ? ADMIN_MODEL_URL : 'spaceship.glb',
    doubleSided: isLocalAdmin,
  });
  if (isLocalAdmin && !isModelCached(ADMIN_MODEL_URL)) {
    adminModelReady.then((adminScene) => {
      if (!adminScene) return;
      // Only strip the exterior — the cockpit interior is a sibling child and must survive
      // this swap, which lands after the 4.7 MB admin GLB finishes loading.
      ship.children.slice().forEach((c) => { if (!c.userData?.isInterior) ship.remove(c); });
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
      syncShipVisibility();
    });
  }
  ship.scale.setScalar(SHIP_SCALE);
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
  const _trialRockCount = opts.mode === 'trials4' ? 210
    : opts.mode === 'trials3' ? 180
      : opts.mode === 'trials2' ? 150
        : isTrialsMode ? 120 : 60;
  const _avoidList = moonAvoid ? [...motherships, moonAvoid] : [...motherships];
  function genCampaignAsteroids() {
    const data = [];
    let id = 1;
    const ZONES = [
      { zMin: -520, zMax: -150, count: 90, xRange: 110, yRange: 55 },
      { zMin: -180, zMax: 200, count: 100, xRange: 130, yRange: 65 },
      { zMin: 160, zMax: 540, count: 90, xRange: 110, yRange: 55 },
    ];
    const TIERS_LOCAL = [
      { name: 'small', minSize: 5, maxSize: 7, hp: 5, w: 0.45 },
      { name: 'medium', minSize: 9, maxSize: 15, hp: 10, w: 0.30 },
      { name: 'big', minSize: 18, maxSize: 30, hp: 30, w: 0.18 },
      { name: 'huge', minSize: 38, maxSize: 55, hp: 50, w: 0.07 },
    ];
    for (const zone of ZONES) {
      for (let i = 0; i < zone.count; i++) {
        const r = Math.random();
        let acc = 0; let tier = TIERS_LOCAL[0];
        for (const t of TIERS_LOCAL) { acc += t.w; if (r < acc) { tier = t; break; } }
        const size = tier.minSize + Math.random() * (tier.maxSize - tier.minSize);
        data.push({
          id: id++, size,
          pos: [
            (Math.random() - 0.5) * 2 * zone.xRange,
            (Math.random() - 0.5) * 2 * zone.yRange,
            zone.zMin + Math.random() * (zone.zMax - zone.zMin),
          ],
          rot: [Math.random() * Math.PI * 2, Math.random() * Math.PI * 2, 0],
          spin: [(Math.random() - 0.5) * 0.4, (Math.random() - 0.5) * 0.4, (Math.random() - 0.5) * 0.2],
          hp: tier.hp, tier: tier.name, variant: Math.floor(Math.random() * 6),
        });
      }
    }
    return data;
  }
  const asteroids = isTerrainMap
    ? createAsteroidFieldFromData([])
    : (opts.asteroids
      ? createAsteroidFieldFromData(opts.asteroids)
      : isCampaign
        ? createAsteroidFieldFromData(genCampaignAsteroids())
        : createAsteroidField({ count: _trialRockCount, radius: 400, avoid: _avoidList }));
  scene.add(asteroids.group);
  const TRIAL1_CPS = [
    new THREE.Vector3(0, 20, -380),
    new THREE.Vector3(180, 60, -260),
    new THREE.Vector3(340, 0, -80),
    new THREE.Vector3(360, -50, 120),
    new THREE.Vector3(220, 80, 280),
    new THREE.Vector3(60, -60, 370),
    new THREE.Vector3(-150, 40, 360),
    new THREE.Vector3(-320, -40, 180),
    new THREE.Vector3(-370, 60, -60),
    new THREE.Vector3(-260, -80, -240),
    new THREE.Vector3(-100, 30, -360),
    new THREE.Vector3(100, -40, -350),
  ];
  const TRIAL2_CPS = [
    new THREE.Vector3(0, 20, -360),
    new THREE.Vector3(160, 80, -220),
    new THREE.Vector3(290, -40, -80),
    new THREE.Vector3(310, -80, 100),
    new THREE.Vector3(190, 100, 270),
    new THREE.Vector3(40, -90, 330),
    new THREE.Vector3(-120, 70, 310),
    new THREE.Vector3(-270, -60, 190),
    new THREE.Vector3(-300, 90, 20),
    new THREE.Vector3(-270, -100, -170),
    new THREE.Vector3(-120, 60, -310),
    new THREE.Vector3(20, -80, -310),
    new THREE.Vector3(140, 90, -240),
    new THREE.Vector3(260, -60, -120),
  ];
  const TRIAL3_CPS = [
    new THREE.Vector3(0, -30, -370),
    new THREE.Vector3(150, 100, -240),
    new THREE.Vector3(300, -80, -60),
    new THREE.Vector3(350, 100, 120),
    new THREE.Vector3(220, -110, 280),
    new THREE.Vector3(60, 100, 350),
    new THREE.Vector3(-80, -110, 300),
    new THREE.Vector3(-240, 100, 160),
    new THREE.Vector3(-330, -90, 0),
    new THREE.Vector3(-260, 110, -180),
    new THREE.Vector3(-120, -100, -290),
    new THREE.Vector3(20, 110, -350),
    new THREE.Vector3(170, -100, -250),
    new THREE.Vector3(310, 100, -70),
    new THREE.Vector3(220, -110, 120),
    new THREE.Vector3(80, 80, -200),
  ];
  const TRIAL4_CPS = [
    new THREE.Vector3(0, 50, -370),
    new THREE.Vector3(180, -100, -210),
    new THREE.Vector3(340, 110, -40),
    new THREE.Vector3(210, -110, 240),
    new THREE.Vector3(40, 110, 340),
    new THREE.Vector3(-180, -110, 210),
    new THREE.Vector3(-160, 80, 0),
    new THREE.Vector3(-200, -100, -210),
    new THREE.Vector3(0, 110, -180),
    new THREE.Vector3(200, -100, -40),
    new THREE.Vector3(300, 100, 180),
    new THREE.Vector3(80, -110, 320),
    new THREE.Vector3(-200, 100, 180),
    new THREE.Vector3(-320, -100, -40),
    new THREE.Vector3(-200, 100, -220),
    new THREE.Vector3(0, -110, -340),
    new THREE.Vector3(200, 100, -220),
    new THREE.Vector3(100, -80, -330),
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
    cpCooldown = 1.5;
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
    const _cdNum = document.getElementById('trials-countdown-num');
    if (_cdWrap) _cdWrap.style.display = 'flex';
    if (_cdNum) { _cdNum.textContent = '3'; _cdNum.style.color = '#ff5566'; }
  }
  const coarseAim = !!opts.noMouse || opts.controlScheme === 'keyboard' || opts.controlScheme === 'mobile';
  const bullets = createBullets({ shipHitRadius: coarseAim ? 7.0 : 6.0 });
  scene.add(bullets.group);
  const beams = createBeams();
  scene.add(beams.group);
  const missileSystem = createMissiles();
  scene.add(missileSystem.group);
  const trails = createTrails();
  scene.add(trails.group);
  // Declared here rather than with the other player state further down: inCockpit()
  // reads it during camera setup below, and a `let` declared later sits in the temporal
  // dead zone. With viewMode 'third' the && short-circuits and hides the problem, so this
  // only threw for players who had already switched to the cockpit view.
  let myAlive = true;
  const tpCam = new ThirdPersonCamera(camera, ship);
  const cockpitProfile = getCockpitProfile(isLocalAdmin);
  const fpCam = new FirstPersonCamera(camera, ship, cockpitProfile);
  const cockpit = createCockpit(cockpitProfile);
  ship.add(cockpit.group);
  const TP_FOV = BASE_FOV;
  let viewMode = localStorage.getItem('spaceships:viewMode') === 'first' ? 'first' : 'third';
  // Dead pilots always watch in third person — the cockpit is hidden with the wreck.
  const inCockpit = () => viewMode === 'first' && myAlive;
  const activeCam = () => (inCockpit() ? fpCam : tpCam);
  // Telemetry channel shared with the cockpit dash; ThirdPersonCamera ignores it.
  const camTel = {
    steerX: 0, steerY: 0, throttle01: 0, speed: 0, hpFrac: 1, boosting: false,
    missiles: 0, flares: 0, heat01: 1, gunMode: 'bullet', boost01: 1, charge01: 0,
    targetLock: false, missileLock: false, hitFlash: 0, contacts: [],
  };
  // Arena scale: the two motherships sit at z -600 and +600, so a 500-unit scope showed
  // an empty screen for most of a match.
  const RADAR_RANGE = 1200;
  const _radarQ = new THREE.Quaternion();
  const _radarV = new THREE.Vector3();
  // In first person the hull stays DRAWN, so you can see your own nose, wings and engines
  // from the cockpit rather than floating in a detached box. It is forced to FrontSide, which
  // back-face culls everything enclosing the eye — the admin hull is authored DoubleSide
  // (ship.js:47) and would otherwise be a solid black wall from the inside. Only the canopy
  // shell itself is hidden, since the cockpit interior provides its own.
  const CANOPY_RE = /cockpit|canopy|glass|windshield|window/i;
  const isCanopyMesh = (o) => CANOPY_RE.test(o.name || '') || CANOPY_RE.test(o.material?.name || '');
  function applyExteriorMode(fp) {
    for (const child of ship.children) {
      if (child.userData?.isInterior) continue;
      child.traverse((o) => {
        if (!o.isMesh || !o.material) return;
        if (o.userData._origSide === undefined) o.userData._origSide = o.material.side;
        if (fp) {
          o.material.side = THREE.FrontSide;
          o.visible = !isCanopyMesh(o);
        } else {
          o.material.side = o.userData._origSide;
          o.visible = true;
        }
      });
      child.visible = true;
    }
  }
  function syncShipVisibility() {
    const fp = inCockpit();
    applyExteriorMode(fp);
    // Dev hook: hide the interior to inspect what the hull looks like from the eye point.
    cockpit.group.visible = fp && !window.__fpHideInterior;
    // The 3D dash replaces the DOM meters, which would otherwise sit right on top of it.
    document.body.classList.toggle('cockpit-view', fp);
  }
  function setViewMode(mode) {
    if (mode === viewMode) return;
    viewMode = mode;
    try { localStorage.setItem('spaceships:viewMode', mode); } catch { }
    syncShipVisibility();
    activeCam().snap();
  }
  function updateCamera(dt) {
    const cam = activeCam();
    // fpCam re-asserts its own FOV each frame; the third-person path must restore the base.
    if (cam === tpCam && camera.fov !== TP_FOV) {
      camera.fov = TP_FOV;
      camera.updateProjectionMatrix();
    }
    cam.update(dt, input, camTel);
    syncShipVisibility();
  }
  window.__fpDebug = () => ({
    viewMode, inCockpit: inCockpit(), profile: cockpitProfile.id, fov: camera.fov,
    contacts: camTel.contacts.length,
  });
  syncShipVisibility();
  activeCam().snap();
  const input = new Input(renderer.domElement);
  const controlScheme = opts.controlScheme
    || (opts.noMouse ? 'keyboard' : 'mouse_keys');
  const noMouseMode = controlScheme === 'keyboard';
  const isMobileScheme = controlScheme === 'mobile';
  input.mouseDisabled = noMouseMode || isMobileScheme;
  input.touchEnabled = isMobileScheme;
  if (noMouseMode) {
    document.body.classList.add('mouse-hidden');
    window.addEventListener('keydown', (e) => {
      if (e.code === 'Escape') document.body.classList.toggle('mouse-hidden');
    });
  }
  const touchHud = createTouchHud({ input, scheme: controlScheme });
  const audio = createAudio();
  const savedMusic = parseFloat(localStorage.getItem('spaceships:musicVolume'));
  const savedSfx = parseFloat(localStorage.getItem('spaceships:sfxVolume'));
  audio.setMusicVolume(Number.isFinite(savedMusic) ? savedMusic : 0.6);
  audio.setSfxVolume(Number.isFinite(savedSfx) ? savedSfx : 1.0);
  window.__shipAudio = audio;
  const ZERO_VEC = new THREE.Vector3();
  const SFX_NEAR_DIST = 80;
  const SFX_FAR_DIST = 900;
  function distanceVol(pos) {
    const d = ship.position.distanceTo(pos);
    if (d <= SFX_NEAR_DIST) return 1.0;
    if (d >= SFX_FAR_DIST) return 0;
    const u = 1 - (d - SFX_NEAR_DIST) / (SFX_FAR_DIST - SFX_NEAR_DIST);
    return u * u;
  }
  const MOVE_MAX_VOL = 0.25;
  const BOOST_MAX_VOL = 0.4;
  const SPEED_FOR_FULL_VOL = 80;
  const MOVE_DUCK_BOOST = 0.25;
  const MOVE_DUCK_BRAKE = 0.4;
  let moveVol = 0;
  let boostVol = 0;
  const ws = opts.ws;
  const myId = opts.you;
  const isSolo = !!opts.solo;
  const remotePlayers = new Map();
  const remoteColors = new Map();
  const remoteModels = new Map();
  const PALETTE = [0xff5577, 0x55ff88, 0xffcc55, 0xaa66ff, 0x55ddff, 0xff99cc, 0xff8833, 0x99ff55];
  function makeDotTexture(fill) {
    const c = document.createElement('canvas');
    c.width = c.height = 32;
    const ctx = c.getContext('2d');
    ctx.fillStyle = fill;
    ctx.beginPath();
    ctx.moveTo(16, 2);
    ctx.lineTo(30, 16);
    ctx.lineTo(16, 30);
    ctx.lineTo(2, 16);
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
  let myRespawnTimer = 0;
  let myInvulnTimer = SPAWN_INVULN_DURATION;
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
  }
  const scoreboardEl = document.getElementById('scoreboard');
  const scoreboardBody = document.getElementById('scoreboard-body');
  function renderScoreboard() {
    if (!scoreboardBody) return;
    const rows = [...scores.entries()]
      .map(([id, s]) => ({ id, ...s }))
      .sort((a, b) => {
        const ta = a.team ?? 99, tb = b.team ?? 99;
        if (ta !== tb) return ta - tb;
        return b.kills - a.kills || a.deaths - b.deaths;
      });
    const hasTeams = rows.some(r => r.team !== null && r.team !== undefined);
    scoreboardBody.innerHTML = '';
    let lastTeam = undefined;
    for (const r of rows) {
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
    const remoteModelUrl = isRemoteAdmin ? ADMIN_MODEL_URL : 'spaceship.glb';
    const remoteShip = colors
      ? createShip({ hullColor: colors.hullColor, accentColor: colors.accentColor, modelUrl: remoteModelUrl, doubleSided: isRemoteAdmin })
      : createShip({ tint: PALETTE[id % PALETTE.length], modelUrl: remoteModelUrl, doubleSided: isRemoteAdmin });
    remoteShip.scale.setScalar(SHIP_SCALE);
    const teamHint = scores.get(id)?.team ?? null;
    const marker = new THREE.Sprite(pickMarkerMat(teamHint));
    marker.scale.set(0.011, 0.011, 1);
    marker.position.y = 1.6;
    marker.renderOrder = 999;
    remoteShip.add(marker);
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
      hitFlash: 0,
      marker,
      box, label, lead,
      vel: new THREE.Vector3(),
      lastStateTime: 0,
      lastStatePos: new THREE.Vector3(),
      team: scores.get(id)?.team ?? null,
    };
    remotePlayers.set(id, r);
    if (isRemoteAdmin && !isModelCached(ADMIN_MODEL_URL)) {
      adminModelReady.then((adminScene) => {
        const rec = remotePlayers.get(id);
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
    flaresLeft = FLARE_MAX;
    ship.position.fromArray(pos);
    ship.quaternion.fromArray(quat);
    shipVelocity.set(0, 0, 0);
    targetThrottle = 0;
    throttle = 0;
    ship.visible = true;
    syncShipVisibility();
    activeCam().snap();
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
      hullColor: parseInt(getSavedShipColor().replace('#', ''), 16),
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
        const hull = typeof msg.hullColor === 'number' ? msg.hullColor : parseInt(String(msg.hullColor).replace('#', ''), 16);
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
            if (msg.hp < r.hp) {
              r.hitFlash = 1;
              const mpb = mpBots.find(b => b.id === msg.id);
              if (mpb) mpb.ai.notifyHit();
            }
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
          const mpb = mpBots.find(b => b.id === msg.id);
          if (mpb) mpb.ai.notifyRespawn();
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
        } else if (msg.kind === 'missile') {
          for (const shot of (msg.shots || [])) {
            const origin = new THREE.Vector3().fromArray(shot.pos);
            const dir = new THREE.Vector3().fromArray(shot.dir);
            const targetRecord = (shot.targetId === myId)
              ? localShipRecord
              : (remotePlayers.get(shot.targetId) ?? null);
            missileSystem.fire(origin, dir, targetRecord, msg.id, shooterTeam);
          }
        } else {
          for (const shot of msg.shots) {
            const origin = new THREE.Vector3().fromArray(shot.pos);
            const dir = new THREE.Vector3().fromArray(shot.dir);
            bullets.fire(origin, dir, faction);
          }
        }
        if (msg.shots.length > 0) {
          const o = msg.shots[0].pos;
          audio.play('shoot', distanceVol(new THREE.Vector3(o[0], o[1], o[2])));
        }
      } else if (msg.type === 'flare' && msg.id !== myId) {
        const fPos = new THREE.Vector3().fromArray(msg.pos);
        const fQuat = new THREE.Quaternion().fromArray(msg.quat);
        missileSystem.deployFlare(fPos, fQuat, msg.id);
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
  let arrowKx = 0, arrowKy = 0;
  const ARROW_RAMP_UP_RATE = 3;
  const ARROW_RAMP_UP_RATE_FINE = 1.5;
  const ARROW_RAMP_DOWN_RATE = 12;
  let aimAssistEnabled = coarseAim
    ? true
    : localStorage.getItem('spaceships:aimAssist') === '1';
  let prevKeyC = false;
  const ASSIST_CONE_DOT = coarseAim ? 0.5 : 0.60;
  const ASSIST_MIN_RANGE = 0;
  const ASSIST_RANGE = 1000;
  const MARKER_VISIBLE_DIST = 1500;
  const ASSIST_STRENGTH = coarseAim ? 2.2 : 2.6;
  const ASSIST_FALLOFF_START = coarseAim ? 0.30 : 0.28;
  const ASSIST_DEAD_ANGLE = coarseAim ? 0.0 : 0.005;
  const ASSIST_STICKY_DOT_BONUS = 0.05;
  const ASSIST_INTENT_BREAK = coarseAim ? 0.25 : 1.8;
  const BRAKE_PITCH_MULT = 1.3;
  const BRAKE_YAW_MULT = 1.7;
  const BRAKE_FULL_TIME = 1.4;
  const BRAKE_BOOST_MIN = 0.18;
  const BRAKE_BOOST_DURATION_MAX = 1.0;
  const BRAKE_BOOST_BONUS_MAX = 50;
  const DRIFT_DRAG = 0.9;
  const DRIFT_GRIP = 0.3;
  const DRIFT_BRAKE = 0.1;
  const VELOCITY_BLEND_RELEASE = 1.5;
  const BRAKE_OVERCHARGE_WARN = 1.0;
  const BRAKE_OVERCHARGE_DAMAGE = 2.0;
  const BRAKE_OVERCHARGE_DPS = 10;
  let brakeOverchargeTime = 0;
  let selfDamageAccum = 0;
  let brakeCharge = 0;
  let prevBraking = false;
  let brakeBoostTimer = 0;
  let brakeBoostCharge = 0;
  const chargeBar = document.getElementById('chargebar');
  const chargeFill = document.getElementById('chargebar-fill');
  const BULLET_COOLDOWN = 0.05;
  const BEAM_COOLDOWN = 0.25;
  const MUZZLE_OFFSETS = [new THREE.Vector3(0, 0, 0.6)];
  let fireTimer = 0;
  let gunMode = 'bullet';
  let prevKeyP = false;
  let prevKeyO = false;
  let prevKeyL = false;
  let prevKeyV = false;
  const BEAM_RANGE = 1000;
  const BEAM_SHIP_RADIUS = 5.5;
  const BEAM_FORWARD_OFFSET = 4;
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
    for (const o of obstacles) {
      const t = raySphereDist(
        origin.x, origin.y, origin.z, dir.x, dir.y, dir.z,
        o.pos.x, o.pos.y, o.pos.z, o.radius,
      );
      if (t !== null && t < bestT) { bestT = t; hitShipId = null; hitAsteroidId = null; }
    }
    return { dist: bestT, hitShipId, hitAsteroidId };
  }
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
  const REGEN_DELAY = 1.0;
  const MAX_AMMO = 90;
  const AMMO_REGEN = 36;
  let ammo = MAX_AMMO;
  let ammoIdle = REGEN_DELAY;
  const MISSILE_MAX = 4;
  let missilesLeft = MISSILE_MAX;
  let prevKeyE = false;
  const mslPips = [
    document.getElementById('msl-pip-1'),
    document.getElementById('msl-pip-2'),
    document.getElementById('msl-pip-3'),
    document.getElementById('msl-pip-4'),
  ];
  const FLARE_MAX = 3;
  let flaresLeft = FLARE_MAX;
  let prevKeyQ = false;
  const flarePips = [
    document.getElementById('fla-pip-1'),
    document.getElementById('fla-pip-2'),
    document.getElementById('fla-pip-3'),
  ];
  const MAX_BOOST = 10;
  const BOOST_DRAIN = 2;
  const BOOST_RECHARGE = 4;
  const BOOST_REGEN_DELAY = 1.0;
  let boostMeter = MAX_BOOST;
  let boostIdle = REGEN_DELAY;
  const HEALTH_REGEN_DELAY = 2.0;
  const HEALTH_REGEN_INTERVAL = 0.1;
  let healthIdleDamage = HEALTH_REGEN_DELAY;
  let healthIdleShot = HEALTH_REGEN_DELAY;
  let healthRegenTick = 0;
  const boostBar = document.getElementById('boostbar');
  const boostFill = document.getElementById('boostbar-fill');
  const heatBar = document.getElementById('heatbar');
  const heatFill = document.getElementById('heatbar-fill');
  const hitVignette = document.getElementById('hit-vignette');
  let prevHpForFlash = SHIP_MAX_HP;
  let vignetteAlpha = 0;
  const VIGNETTE_DECAY = 2.4;
  const TRAIL_OFFSETS = [
    new THREE.Vector3(-2.2, -0.05, -1.8),
    new THREE.Vector3(2.2, -0.05, -1.8),
  ];
  const ADMIN_TRAIL_OFFSETS = [
    new THREE.Vector3(-0.9, -0.05, -2.4),
    new THREE.Vector3(0.9, -0.05, -2.4),
  ];
  const localTrailOffsets = isLocalAdmin ? ADMIN_TRAIL_OFFSETS : TRAIL_OFFSETS;
  const enemyTrailsEnabled = localStorage.getItem('spaceships:enemyTrails') !== '0';
  const EMIT_CONFIG = {
    move: { rate: 18, scale: [0.16, 0.28], colors: [0xffffff], jitter: 0.05, life: [0.18, 0.30] },
    boost: { rate: 45, scale: [0.50, 0.85], colors: [0x66ddff, 0xffffff], jitter: 0.13, life: [0.45, 0.65] },
    brake: { rate: 35, scale: [0.36, 0.60], colors: [0xffd933, 0xffaa33], jitter: 0.10, life: [0.28, 0.45] },
  };
  let trailTimer = 0;
  const savedTrailColorHex = parseInt(getSavedTrailColor().replace('#', ''), 16);
  const savedTrailShape = getSavedTrailShape();
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
  function update(dt) {
    input.pollGamepad();
    if (input.gp.menuBtn) {
      if (pauseOpen) closePause(); else openPause();
    }
    if (pauseOpen) {
      pauseNavCooldown = Math.max(0, pauseNavCooldown - dt);
      const rawGp = [...(navigator.getGamepads?.() ?? [])].find(g => g?.connected);
      if (rawGp) {
        const navUp = rawGp.buttons[12]?.pressed || rawGp.axes[1] < -0.5;
        const navDown = rawGp.buttons[13]?.pressed || rawGp.axes[1] > 0.5;
        const confirm = rawGp.buttons[0]?.pressed;
        if (pauseNavCooldown === 0 && (navUp || navDown)) {
          pauseFocusIdx = pauseFocusIdx === 0 ? 1 : 0;
          pauseBtns[pauseFocusIdx].focus();
          pauseNavCooldown = (pausePrevNavUp || pausePrevNavDown) ? 0.12 : 0.25;
        }
        if (confirm && !pausePrevConfirm) pauseBtns[pauseFocusIdx].click();
        pausePrevNavUp = navUp;
        pausePrevNavDown = navDown;
        pausePrevConfirm = confirm;
      }
    }
    warpEffect.update(dt);
    if (isTrialsMode && trialsCountdownActive) {
      trialsCountdown -= dt;
      const cdWrap = document.getElementById('trials-countdown');
      const cdNum = document.getElementById('trials-countdown-num');
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
      updateCamera(dt);
      return;
    }
    const braking = myAlive && (input.keys.has('Space') || input.gp.drift);
    camTel.steerX = 0;
    camTel.steerY = 0;
    camTel.throttle01 = MAX_THROTTLE > 0 ? throttle / MAX_THROTTLE : 0;
    if (myAlive) {
      if (input.throttleOverride !== null) {
        targetThrottle = input.throttleOverride * MAX_THROTTLE;
        input.consumeWheel();
      } else {
        const wheel = input.consumeWheel();
        if (wheel !== 0) targetThrottle += wheel * THROTTLE_STEP;
        if (input.keys.has('KeyW') || input.gp.throttleAxis > 0.01)
          targetThrottle += KEY_THROTTLE_RATE * dt * (input.gp.throttleAxis > 0.01 ? input.gp.throttleAxis : 1);
        if (input.keys.has('KeyS') || input.gp.throttleAxis < -0.01)
          targetThrottle -= KEY_THROTTLE_RATE * dt * (input.gp.throttleAxis < -0.01 ? -input.gp.throttleAxis : 1);
      }
      targetThrottle = Math.max(0, Math.min(MAX_THROTTLE, targetThrottle));
      throttle = THREE.MathUtils.damp(throttle, targetThrottle, 3, dt);
      let sx = input.rmb ? 0 : input.steerX;
      let sy = input.rmb ? 0 : input.steerY;
      if (!input.gp.freeLook
        && (Math.abs(input.gp.steerX) > 0.01 || Math.abs(input.gp.steerY) > 0.01)) {
        sx = input.gp.steerX;
        sy = input.gp.steerY;
      }
      if (Math.abs(sx) < STEER_DEADZONE) sx = 0;
      if (Math.abs(sy) < STEER_DEADZONE) sy = 0;
      sx = Math.sign(sx) * Math.pow(Math.abs(sx), 1.6);
      sy = Math.sign(sy) * Math.pow(Math.abs(sy), 1.6);
      let kxTarget = 0, kyTarget = 0;
      if (input.keys.has('ArrowLeft')) kxTarget -= 1;
      if (input.keys.has('ArrowRight')) kxTarget += 1;
      if (input.keys.has('ArrowUp')) kyTarget -= 1;
      if (input.keys.has('ArrowDown')) kyTarget += 1;
      const upRate = input.keys.has('KeyQ') ? ARROW_RAMP_UP_RATE_FINE : ARROW_RAMP_UP_RATE;
      const rateX = kxTarget !== 0 ? upRate : ARROW_RAMP_DOWN_RATE;
      const rateY = kyTarget !== 0 ? upRate : ARROW_RAMP_DOWN_RATE;
      arrowKx = THREE.MathUtils.damp(arrowKx, kxTarget, rateX, dt);
      arrowKy = THREE.MathUtils.damp(arrowKy, kyTarget, rateY, dt);
      if (kxTarget !== 0 || Math.abs(arrowKx) > 0.01) sx = arrowKx;
      if (kyTarget !== 0 || Math.abs(arrowKy) > 0.01) sy = arrowKy;
      camTel.steerX = sx;
      camTel.steerY = sy;
      const pitchMult = braking ? BRAKE_PITCH_MULT : 1;
      const yawMult = braking ? BRAKE_YAW_MULT : 1;
      const pitchRate = (sy < 0 ? PITCH_RATE * PITCH_UP_BOOST : PITCH_RATE) * pitchMult;
      const pitch = sy * pitchRate * dt;
      const yaw = -sx * YAW_RATE * yawMult * dt;
      let roll = 0;
      if (input.keys.has('KeyD')) roll += ROLL_RATE * pitchMult * dt;
      if (input.keys.has('KeyA')) roll -= ROLL_RATE * pitchMult * dt;
      if (input.gp.rollAxis !== 0) roll = input.gp.rollAxis * ROLL_RATE * pitchMult * dt;
      if (pitch) ship.quaternion.multiply(tmpQ.setFromAxisAngle(xAxis, pitch));
      if (yaw) ship.quaternion.multiply(tmpQ.setFromAxisAngle(yAxis, yaw));
      if (roll) ship.quaternion.multiply(tmpQ.setFromAxisAngle(zAxis, roll));
      ship.quaternion.normalize();
      if (aimAssistEnabled) {
        const steerMag = Math.max(Math.abs(sx), Math.abs(sy));
        applyAimAssist(dt, steerMag);
      }
    }
    if (brakeBoostTimer > 0) brakeBoostTimer = Math.max(0, brakeBoostTimer - dt);
    const wantShift = input.keys.has('ShiftLeft') || input.keys.has('ShiftRight') || input.gp.boost;
    const shiftBoost = myAlive && !braking && wantShift && boostMeter > 0;
    const brakeReleaseBoost = myAlive && brakeBoostTimer > 0;
    const boosting = myAlive && (shiftBoost || brakeReleaseBoost);
    boostIdle += dt;
    if (shiftBoost) {
      boostMeter = Math.max(0, boostMeter - BOOST_DRAIN * dt);
      boostIdle = 0;
    } else if (wantShift) {
      boostIdle = 0;
    }
    if (boostMeter < MAX_BOOST && boostIdle >= BOOST_REGEN_DELAY) {
      boostMeter = Math.min(MAX_BOOST, boostMeter + BOOST_RECHARGE * dt);
    }
    if (myAlive) {
      if (braking) {
        const speed = shipVelocity.length();
        if (speed > 0.001 && DRIFT_GRIP > 0) {
          const fwd = new THREE.Vector3(0, 0, 1).applyQuaternion(ship.quaternion);
          const desired = fwd.multiplyScalar(speed);
          shipVelocity.lerp(desired, 1 - Math.pow(0.001, dt * DRIFT_GRIP / 6));
        }
        const drag = input.keys.has('KeyS') ? DRIFT_BRAKE : DRIFT_DRAG;
        shipVelocity.multiplyScalar(Math.pow(drag, dt));
      } else {
        const speedMult = shiftBoost ? BOOST_FACTOR : 1;
        const forward = new THREE.Vector3(0, 0, 1).applyQuaternion(ship.quaternion);
        const target = forward.clone().multiplyScalar(throttle * speedMult);
        if (brakeReleaseBoost) {
          target.addScaledVector(forward, BRAKE_BOOST_BONUS_MAX * brakeBoostCharge);
        }
        const blend = brakeReleaseBoost ? VELOCITY_BLEND_RELEASE : VELOCITY_BLEND;
        shipVelocity.lerp(target, 1 - Math.pow(0.001, dt * blend / 6));
      }
      ship.position.addScaledVector(shipVelocity, dt);
    }
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
    if (braking && brakeCharge >= 1 && myAlive) {
      brakeOverchargeTime += dt;
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
      heatBar.classList.toggle('overheated', ammo < (gunMode === 'beam' ? 3 : 1));
    }
    for (let _pi = 0; _pi < mslPips.length; _pi++) {
      if (mslPips[_pi]) mslPips[_pi].classList.toggle('empty', _pi >= missilesLeft);
    }
    for (let _pi = 0; _pi < flarePips.length; _pi++) {
      if (flarePips[_pi]) flarePips[_pi].classList.toggle('empty', _pi >= flaresLeft);
    }
    const _missileLocked = missileSystem.isTargetingLocal(localShipRecord) && myAlive;
    const _lockWarnEl = document.getElementById('missile-lock-warning');
    if (_lockWarnEl) _lockWarnEl.style.display = _missileLocked ? '' : 'none';
    const nowKeyP = input.keys.has('KeyP');
    if (nowKeyP && !prevKeyP) {
      gunMode = gunMode === 'beam' ? 'bullet' : 'beam';
    }
    prevKeyP = nowKeyP;
    const nowKeyC = input.keys.has('KeyC');
    if (nowKeyC && !prevKeyC) {
      aimAssistEnabled = !aimAssistEnabled;
      try { localStorage.setItem('spaceships:aimAssist', aimAssistEnabled ? '1' : '0'); } catch { }
      showAimAssistToast(aimAssistEnabled);
    }
    prevKeyC = nowKeyC;
    const nowKeyO = input.keys.has('KeyO');
    if (nowKeyO && !prevKeyO && !noMouseMode) {
      if (document.pointerLockElement) {
        document.exitPointerLock();
      } else {
        renderer.domElement.requestPointerLock?.();
      }
    }
    prevKeyO = nowKeyO;
    const nowKeyL = input.keys.has('KeyL');
    if (nowKeyL && !prevKeyL) {
      if (document.fullscreenElement) {
        document.exitFullscreen?.();
      } else {
        document.documentElement.requestFullscreen?.();
      }
    }
    prevKeyL = nowKeyL;
    const nowKeyV = input.keys.has('KeyV');
    if (nowKeyV && !prevKeyV) {
      setViewMode(viewMode === 'first' ? 'third' : 'first');
    }
    prevKeyV = nowKeyV;
    const nowKeyE = input.keys.has('KeyE');
    if (nowKeyE && !prevKeyE && myAlive && missilesLeft > 0) {
      let closestRecord = null;
      let closestDist = Infinity;
      for (const r of remotePlayers.values()) {
        if (!r.alive || !r.hasTarget) continue;
        if (myTeam !== undefined && myTeam !== null && r.team === myTeam) continue;
        const d = ship.position.distanceTo(r.ship.position);
        if (d >= closestDist) continue;
        const lx = (r.ship.position.x - ship.position.x) / d;
        const ly = (r.ship.position.y - ship.position.y) / d;
        const lz = (r.ship.position.z - ship.position.z) / d;
        let occluded = false;
        for (const a of asteroids.list) {
          const hit = raySphereDist(
            ship.position.x, ship.position.y, ship.position.z,
            lx, ly, lz,
            a.mesh.position.x, a.mesh.position.y, a.mesh.position.z,
            a.radius,
          );
          if (hit !== null && hit < d) { occluded = true; break; }
        }
        if (!occluded) {
          for (const o of obstacles) {
            const hit = raySphereDist(
              ship.position.x, ship.position.y, ship.position.z,
              lx, ly, lz,
              o.pos.x, o.pos.y, o.pos.z, o.radius,
            );
            if (hit !== null && hit < d) { occluded = true; break; }
          }
        }
        if (occluded) continue;
        closestDist = d;
        closestRecord = r;
      }
      if (closestRecord !== null) {
        const fwd = new THREE.Vector3(0, 0, 1).applyQuaternion(ship.quaternion);
        const mslOrigin = ship.position.clone().addScaledVector(fwd, 6);
        missileSystem.fire(mslOrigin, fwd, closestRecord, myId, myTeam);
        missilesLeft--;
        audio.play('shoot');
        if (ws && ws.readyState === WebSocket.OPEN) {
          ws.send(JSON.stringify({
            type: 'fire',
            kind: 'missile',
            shots: [{
              pos: [mslOrigin.x, mslOrigin.y, mslOrigin.z],
              dir: [fwd.x, fwd.y, fwd.z],
              targetId: closestRecord.id,
            }],
          }));
        }
      }
    }
    prevKeyE = nowKeyE;
    const nowKeyQ = input.keys.has('KeyQ');
    if (nowKeyQ && !prevKeyQ && myAlive && flaresLeft > 0) {
      missileSystem.deployFlare(ship.position.clone(), ship.quaternion, myId);
      flaresLeft--;
      audio.play('shoot');
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({
          type: 'flare',
          pos: [ship.position.x, ship.position.y, ship.position.z],
          quat: [ship.quaternion.x, ship.quaternion.y, ship.quaternion.z, ship.quaternion.w],
        }));
      }
    }
    prevKeyQ = nowKeyQ;
    fireTimer -= dt;
    ammoIdle += dt;
    const ammoCost = gunMode === 'beam' ? 3 : 1;
    const canFire = ammo >= ammoCost;
    if ((input.lmb || input.keys.has('KeyF') || input.gp.fire) && fireTimer <= 0 && myAlive && canFire) {
      const dir = new THREE.Vector3(0, 0, 1).applyQuaternion(ship.quaternion);
      const shots = [];
      for (const off of MUZZLE_OFFSETS) {
        const origin = off.clone().applyQuaternion(ship.quaternion).add(ship.position);
        if (gunMode === 'beam') {
          const cast = castWorldRay(origin, dir, BEAM_RANGE, { skipTeam: myTeam });
          let beamDist = cast.dist;
          let hitTargetId = cast.hitShipId;
          const hitAsteroidId = cast.hitAsteroidId;
          let hitBoss = false;
          if (isCampaign && bossActive) {
            const capPos = capitalShipMesh ? capitalShipMesh.position : platformB.position;
            const bossT = raySphereDist(
              origin.x, origin.y, origin.z, dir.x, dir.y, dir.z,
              capPos.x, capPos.y, capPos.z, 95,
            );
            if (bossT !== null && bossT < beamDist) {
              hitBoss = true; hitTargetId = null; beamDist = bossT;
            }
          }
          const end = origin.clone().addScaledVector(dir, beamDist);
          const visualStart = beamDist > BEAM_FORWARD_OFFSET
            ? origin.clone().addScaledVector(dir, BEAM_FORWARD_OFFSET)
            : origin;
          beams.fire(visualStart, end, 'self');
          if (hitTargetId !== null || hitBoss) {
            bullets.spawnExplosion(end, hitBoss ? 2.5 : 1.0);
            audio.play('hitmarker_2');
            if (hitBoss) {
              applyBossHit(10);
            } else if (ws && ws.readyState === WebSocket.OPEN) {
              ws.send(JSON.stringify({ type: 'hit', targetId: hitTargetId, kind: 'beam' }));
            } else if (isSolo) {
              applyHitToBot(hitTargetId, 10, opts.you, myTeam);
            }
          } else if (hitAsteroidId !== null && hitAsteroidId !== undefined) {
            if (ws && ws.readyState === WebSocket.OPEN) {
              ws.send(JSON.stringify({ type: 'asteroid-hit', id: hitAsteroidId }));
            } else if (isSolo) {
              damageAsteroidLocal(hitAsteroidId);
            }
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
    if (myAlive) {
      healthIdleDamage += dt;
      healthIdleShot += dt;
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
    let emitMode = null;
    if (myAlive) {
      if (braking) emitMode = 'brake';
      else if (boosting) emitMode = 'boost';
      else if (shipVelocity.length() > 5) emitMode = 'move';
    }
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
          const baseColor = cfg.colors[Math.floor(Math.random() * cfg.colors.length)];
          const color = (emitMode !== 'brake') ? savedTrailColorHex : baseColor;
          const life = cfg.life[0] + Math.random() * (cfg.life[1] - cfg.life[0]);
          trails.emit(p, scale, color, life, savedTrailShape);
        }
      }
    } else {
      trailTimer = 0;
    }
    if (enemyTrailsEnabled) {
      for (const r of remotePlayers.values()) {
        if (!r.alive || !r.hasTarget) { r.trailTimer = 0; continue; }
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
        if (isCampaign && targetId >= BOSS_ID_BASE && targetId < BOSS_ID_BASE + BOSS_HITBOX_COUNT) {
          applyBossHit(10);
        } else if (ws && ws.readyState === WebSocket.OPEN) {
          ws.send(JSON.stringify({ type: 'hit', targetId, kind: 'bullet' }));
        } else if (isSolo) {
          applyHitToBot(targetId, 10, opts.you, myTeam);
        }
      },
      (asteroidId) => {
        if (ws && ws.readyState === WebSocket.OPEN) {
          ws.send(JSON.stringify({ type: 'asteroid-hit', id: asteroidId }));
        } else if (isSolo) {
          damageAsteroidLocal(asteroidId);
        }
        audio.play('impact');
      },
      myTeam,
      obstacles,
    );
    missileSystem.update(
      dt,
      remotePlayers,
      (targetId, ownerId, ownerTeam) => {
        const mine = ownerId == null || ownerId === myId;
        if (mine) audio.play('hitmarker_2');
        if (isCampaign && targetId >= BOSS_ID_BASE && targetId < BOSS_ID_BASE + BOSS_HITBOX_COUNT) {
          if (mine) applyBossHit(50);
        } else if (isSolo) {
          applyHitToBot(targetId, 50, ownerId ?? opts.you, ownerTeam ?? myTeam);
        } else if (ws && ws.readyState === WebSocket.OPEN) {
          if (mine) {
            ws.send(JSON.stringify({ type: 'hit', targetId, kind: 'missile' }));
          } else if (mpBots.some((b) => b.id === ownerId)) {
            ws.send(JSON.stringify({ type: 'hit', targetId, fromBotId: ownerId, kind: 'missile' }));
          }
        }
      },
      myTeam,
      asteroids,
      obstacles,
      {
        id: myId,
        team: myTeam,
        record: localShipRecord,
        onHit: (ownerId, ownerTeam) => {
          if (isSolo) {
            applyPlayerDamageLocal(50, ownerId, ownerTeam);
          } else if (mpBots.some((b) => b.id === ownerId)
            && ws && ws.readyState === WebSocket.OPEN) {
            ws.send(JSON.stringify({ type: 'hit', targetId: myId, fromBotId: ownerId, kind: 'missile' }));
          }
        },
      },
    );
    trails.update(dt, camera);
    if (clouds) clouds.update(dt);
    if (terrainSun) {
      terrainSun.target.position.copy(ship.position);
      terrainSun.target.updateMatrixWorld();
      terrainSun.position.set(ship.position.x, ship.position.y + 500, ship.position.z);
    }
    if (myAlive) {
      resolveCollisions();
      resolveMothershipCollisions();
    }
    updateCamera(dt);
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
    if (!isSolo && mpBots.length > 0 && !matchOver) {
      mpBotStateTimer += dt;
      const sendBotState = mpBotStateTimer >= STATE_INTERVAL;
      if (sendBotState) mpBotStateTimer = 0;
      for (const b of mpBots) {
        b.ai.update(dt);
        if (sendBotState && b.record.alive && ws && ws.readyState === WebSocket.OPEN) {
          ws.send(JSON.stringify({
            type: 'bot-state',
            botId: b.id,
            pos: b.record.ship.position.toArray(),
            quat: b.record.ship.quaternion.toArray(),
          }));
        }
      }
    }
    const remoteLerp = 1 - Math.pow(0.001, dt * 8);
    for (const r of remotePlayers.values()) {
      if (r.isBot && (isSolo || r.isMpBot)) continue;
      r.ship.position.lerp(r.targetPos, remoteLerp);
      r.ship.quaternion.slerp(r.targetQuat, remoteLerp);
    }
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
      if (isCampaign && !campaignOver) {
        updateCampaign(dt);
      }
    }
    if (tutorial) tutorial.update(dt);
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
    const TARGETING_MAX_DIST = MARKER_VISIBLE_DIST;
    for (const r of remotePlayers.values()) {
      if (!r.alive || !r.hasTarget) {
        r.box.style.display = 'none';
        r.lead.style.display = 'none';
        if (r.marker) r.marker.visible = false;
        continue;
      }
      const dist = ship.position.distanceTo(r.ship.position);
      r.ship.visible = dist <= MARKER_VISIBLE_DIST;
      if (r.hitFlash > 0) {
        r.hitFlash = Math.max(0, r.hitFlash - dt * 4);
        const f = r.hitFlash;
        r.ship.traverse((o) => {
          if (o.isMesh && o.material && o.material.emissive) {
            o.material.emissive.setRGB(f, f, f);
          }
        });
      }
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
      r.lead.style.display = '';
      r.lead.style.left = sx + 'px';
      r.lead.style.top = sy + 'px';
      const lx = sx, ly = sy;
      const dx = lx - reticleX, dy = ly - reticleY;
      const screenDist = Math.sqrt(dx * dx + dy * dy);
      r.lead.classList.toggle('aligned', screenDist < 22);
      if (screenDist < bestAlignment) bestAlignment = screenDist;
    }
    const hasTargetLock = anyVisible && bestAlignment < 22;
    if (reticleEl) {
      reticleEl.classList.toggle('locked', hasTargetLock);
    }
    if (inCockpit()) {
      camTel.speed = shipVelocity.length();
      camTel.hpFrac = Math.max(0, myHp / SHIP_MAX_HP);
      camTel.boosting = boosting;
      camTel.missiles = missilesLeft;
      camTel.flares = flaresLeft;
      camTel.heat01 = ammo / MAX_AMMO;
      camTel.gunMode = gunMode;
      camTel.boost01 = boostMeter / MAX_BOOST;
      camTel.charge01 = brakeCharge;
      camTel.hitFlash = vignetteAlpha;
      // Radar contacts, rotated into the ship's frame so the scope reads heading-up.
      // Offsets stay in world units (no worldToLocal, which would divide by SHIP_SCALE).
      camTel.contacts.length = 0;
      _radarQ.copy(ship.quaternion).invert();
      for (const r of remotePlayers.values()) {
        if (!r.alive || !r.hasTarget) continue;
        _radarV.subVectors(r.ship.position, ship.position);
        if (_radarV.lengthSq() > RADAR_RANGE * RADAR_RANGE) continue;
        _radarV.applyQuaternion(_radarQ);
        camTel.contacts.push({
          x: _radarV.x / RADAR_RANGE,
          z: _radarV.z / RADAR_RANGE,
          hostile: !(myTeam !== undefined && myTeam !== null && r.team === myTeam),
        });
      }
      camTel.targetLock = hasTargetLock;
      camTel.missileLock = _missileLocked;
      // Dev hook: force instrument states from the console to check the panel without combat.
      if (window.__fpForce) Object.assign(camTel, window.__fpForce);
      cockpit.update(dt, camTel);
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
      const hue = Math.round(pct * 120);
      hpFill.style.background = `linear-gradient(180deg, hsl(${hue}, 80%, 60%) 0%, hsl(${hue}, 70%, 38%) 100%)`;
      hpText.textContent = `${myHp} / ${SHIP_MAX_HP}`;
    }
    if (myAlive && myHp < prevHpForFlash) {
      vignetteAlpha = 1;
    }
    prevHpForFlash = myHp;
    vignetteAlpha = Math.max(0, vignetteAlpha - VIGNETTE_DECAY * dt);
    if (hitVignette) hitVignette.style.opacity = vignetteAlpha.toFixed(3);
    if (myInvulnTimer > 0) {
      myInvulnTimer = Math.max(0, myInvulnTimer - dt);
      if (myAlive) {
        // In the cockpit the exterior is already hidden, so only strobe in third person.
        ship.visible = inCockpit() || (Math.floor(performance.now() * 0.012) % 2 === 0);
        if (myInvulnTimer === 0) ship.visible = true;
      }
    }
    if (deathBanner) {
      deathBanner.style.display = myAlive ? 'none' : 'block';
    }
  }
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
    const intentDamp = Math.max(0, 1 - steerMag / ASSIST_INTENT_BREAK);
    const intentFactor = intentDamp * intentDamp;
    if (intentFactor <= 0) {
      assistStrengthSmoothed = THREE.MathUtils.damp(assistStrengthSmoothed, 0, 6, dt);
      assistHasTarget = false;
      return;
    }
    _assistFwd.set(0, 0, 1).applyQuaternion(ship.quaternion);
    let bestDot = ASSIST_CONE_DOT;
    let bestTarget = null;
    let bestLead = null;
    for (const r of remotePlayers.values()) {
      if (!r.alive || !r.hasTarget) continue;
      if (r.team !== null && r.team !== undefined && r.team === myTeam) continue;
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
    const targetPresence = bestTarget ? 1 : 0;
    assistStrengthSmoothed = THREE.MathUtils.damp(assistStrengthSmoothed, targetPresence, 6, dt);
    if (bestTarget) {
      _assistTo.subVectors(bestLead, ship.position).normalize();
      if (!assistHasTarget || lastAssistTargetId !== bestTarget.id) {
        assistTargetDir.copy(_assistTo);
      } else {
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
    if (angle <= ASSIST_DEAD_ANGLE) return;
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
      else if (py < pz) { nx = 0; ny = Math.sign(dy) || 1; nz = 0; push = py + radius; }
      else { nx = 0; ny = 0; nz = Math.sign(dz) || 1; push = pz + radius; }
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
  const touchingAsteroids = new Set();
  let touchingMoon = false;
  let touchingWater = false;
  function dealSelfDamage(dmg) {
    if (myInvulnTimer > 0) return;
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: 'self-damage', dmg }));
    } else if (isSolo) {
      applyPlayerDamageLocal(dmg);
    }
  }
  // Solo mirror of the server's 'asteroid-hit' handling (server/index.js). Without this,
  // asteroids only ever took damage in multiplayer -- every solo hit was sent to a socket
  // that does not exist and silently dropped.
  function damageAsteroidLocal(id) {
    if (id === undefined || id === null || !asteroids.destroy) return;
    const a = asteroids.list.find((x) => x.id === id);
    if (!a || a.hp <= 0) return;
    a.hp = Math.max(0, a.hp - 1);
    a.hitFlash = 1;
    if (a.hp > 0) return;
    const rec = asteroids.destroy(id);
    if (rec) {
      bullets.spawnExplosion(rec.mesh.position, rec.radius);
      audio.play('rockbreak', distanceVol(rec.mesh.position));
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
        if (!touchingAsteroids.has(a) && SOLO_MODE !== 'tutorial') {
          const dmg = 15 + Math.floor(Math.random() * 15); // [15, 29]
          dealSelfDamage(dmg);
        }
      }
    }
    touchingAsteroids.clear();
    for (const a of nextAsteroids) touchingAsteroids.add(a);
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
    if (isTerrainMap) {
      const groundY = getTerrainHeight(ship.position.x, ship.position.z);
      const killY = groundY + TERRAIN_KILL_CLEARANCE;
      if (ship.position.y < killY) {
        ship.position.y = killY;
        if (shipVelocity.y < 0) shipVelocity.y *= -0.5;
        if (!touchingWater && SOLO_MODE !== 'tutorial') {
          dealSelfDamage(SHIP_MAX_HP);
        }
        touchingWater = true;
      } else {
        touchingWater = false;
      }
    }
  }
  const SOLO_MODE = isSolo ? (opts.mode || 'train') : null;
  const myTeam = isSolo ? 0 : (opts.spawn?.team ?? 0);
  const MATCH_DURATION = SOLO_MODE === 'train' ? 180 : 300;
  const teamKills = [0, 0];
  let matchTimer = MATCH_DURATION;
  let matchOver = false;
  const matchActive = SOLO_MODE === 'skirmish' || SOLO_MODE === 'train' || !isSolo;
  let soloBotsKilled = 0;
  const BOSS_ID_BASE = 9000;
  const BOSS_HITBOX_COUNT = 20;
  const BOSS_MAX_HP = 2500;
  const CAMPAIGN_WAVES = CAMPAIGN_MISSION === 3
    ? [
      { count: 5, label: 'WAVE 1 / 3', objective: 'Destroy the assault wing', spawnZ: -280 },
      { count: 7, label: 'WAVE 2 / 3', objective: 'Eliminate the heavy fighters', spawnZ: 20 },
      { count: 6, label: 'WAVE 3 / 3', objective: 'Crush the elite vanguard', spawnZ: 330 },
    ]
    : CAMPAIGN_MISSION === 2
      ? [
        { count: 4, label: 'WAVE 1 / 3', objective: 'Destroy the patrol fleet', spawnZ: -280 },
        { count: 6, label: 'WAVE 2 / 3', objective: 'Eliminate the fighter escort', spawnZ: 20 },
        { count: 5, label: 'WAVE 3 / 3', objective: 'Break through the elite guard', spawnZ: 330 },
      ]
      : [
        { count: 3, label: 'WAVE 1 / 3', objective: 'Destroy the enemy scout drones', spawnZ: -280 },
        { count: 5, label: 'WAVE 2 / 3', objective: 'Destroy the enemy fighter squadron', spawnZ: 20 },
        { count: 4, label: 'WAVE 3 / 3', objective: 'Eliminate the elite guard', spawnZ: 330 },
      ];
  const MISSION_BRIEFINGS = [
    '',
    'OPERATION: IRONCLAD\nFight through enemy waves and destroy the Capital Ship',
    'OPERATION: STORMFRONT\nHeavier defenses stand between you and the dreadnought',
    'OPERATION: FINAL SIEGE\nEverything or nothing — destroy the flagship and end it',
  ];
  const BOSS_HB_OFFSETS_WORLD = [
    new THREE.Vector3(-85, 0, -150),
    new THREE.Vector3(-28, 0, -150),
    new THREE.Vector3(28, 0, -150),
    new THREE.Vector3(85, 0, -150),
    new THREE.Vector3(-85, 0, -75),
    new THREE.Vector3(-28, 0, -75),
    new THREE.Vector3(28, 0, -75),
    new THREE.Vector3(85, 0, -75),
    new THREE.Vector3(-85, 0, 0),
    new THREE.Vector3(0, 0, 0),
    new THREE.Vector3(85, 0, 0),
    new THREE.Vector3(-85, 0, 75),
    new THREE.Vector3(-28, 0, 75),
    new THREE.Vector3(28, 0, 75),
    new THREE.Vector3(85, 0, 75),
    new THREE.Vector3(-85, 0, 150),
    new THREE.Vector3(-28, 0, 150),
    new THREE.Vector3(28, 0, 150),
    new THREE.Vector3(85, 0, 150),
    new THREE.Vector3(0, 30, 50),
  ];
  let campaignPhase = 0;
  let campaignWaveBotIds = new Set();
  let campaignBotsAlive = 0;
  let campaignBetween = false;
  let campaignBetweenTimer = 0;
  let bossHp = BOSS_MAX_HP;
  let bossActive = false;
  let bossBullets = [];
  let bossFireTimer = 0;
  let campaignOver = false;
  let campaignNextBotId = 100;
  let campaignMsgTimer = 0;
  let campaignLives = 3;
  let campaignCheckpointPos = [0, 0, -540];
  let campaignWarpActive = false;
  let campaignWarpTimer = 0;
  let capitalShipMesh = null;
  let capitalShipTurrets = [];
  let capitalShipTime = 0;
  const CAPITAL_SHIP_BASE_POS = new THREE.Vector3(0, 0, 600);
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
    } catch { }
  }
  function updateCachedCredits(total) {
    if (Number.isFinite(total)) localStorage.setItem('spaceships:credits', String(total));
  }
  async function reportSoloResult(kills, deaths, won, botsKilled) {
    const token = localStorage.getItem('spaceships:token');
    if (!token) return;
    try {
      const res = await fetch('/spaceships/api/solo-result', {
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
  async function reportCampaignResult(missionNum, livesRemaining) {
    const token = localStorage.getItem('spaceships:token');
    if (!token) return;
    try {
      const res = await fetch('/spaceships/api/campaign-result', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'Authorization': 'Bearer ' + token },
        body: JSON.stringify({ missionNum, livesRemaining }),
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
      console.warn('[campaign-result] could not report:', e);
    }
  }
  async function reportTrialTime(trialNum, time) {
    const token = localStorage.getItem('spaceships:token');
    if (!token) return;
    try {
      const res = await fetch('/spaceships/api/trial-result', {
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
  const localShipRecord = {
    get alive() { return myAlive; },
    ship,
  };
  const bots = [];
  const mpBots = [];
  let mpBotStateTimer = 0;
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
      missileMax: opts.hardMode ? 3 : 1,
      fireMissile: (targetEntity) => {
        const targetRecord = targetEntity.id === myId
          ? localShipRecord
          : (remotePlayers.get(targetEntity.id) ?? null);
        if (!targetRecord) return false;
        const fwd = new THREE.Vector3(0, 0, 1).applyQuaternion(r.ship.quaternion);
        const mslOrigin = r.ship.position.clone().addScaledVector(fwd, 6);
        missileSystem.fire(mslOrigin, fwd, targetRecord, id, team);
        audio.play('shoot', distanceVol(r.ship.position));
        return true;
      },
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
  function updateCampaignHud() {
    if (!isCampaign) return;
    const waveEl = document.getElementById('campaign-wave');
    const objEl = document.getElementById('campaign-objective');
    const enemyEl = document.getElementById('campaign-enemies');
    const fillEl = document.getElementById('boss-bar-fill');
    const hpEl = document.getElementById('boss-hp-text');
    if (campaignPhase < 3) {
      const wave = CAMPAIGN_WAVES[campaignPhase];
      if (waveEl) waveEl.textContent = wave.label;
      if (objEl) objEl.textContent = wave.objective;
      if (enemyEl) enemyEl.textContent = campaignBotsAlive > 0
        ? `Enemies remaining: ${campaignBotsAlive}`
        : (campaignBetween ? 'Sector clear' : '');
    } else if (campaignPhase === 3) {
      if (waveEl) waveEl.textContent = '— BOSS PHASE —';
      if (objEl) objEl.textContent = 'Destroy the Capital Ship';
      if (enemyEl) enemyEl.textContent = '';
      if (fillEl) fillEl.style.width = `${(bossHp / BOSS_MAX_HP * 100).toFixed(1)}%`;
      if (hpEl) hpEl.textContent = `${Math.max(0, bossHp).toLocaleString()} / ${BOSS_MAX_HP.toLocaleString()}`;
    } else {
      if (waveEl) waveEl.textContent = 'VICTORY';
      if (objEl) objEl.textContent = 'Mission accomplished';
      if (enemyEl) enemyEl.textContent = '';
    }
  }
  function showCampaignMsg(text, duration) {
    const el = document.getElementById('campaign-msg');
    const textEl = document.getElementById('campaign-msg-text');
    if (textEl) textEl.textContent = text;
    if (el) el.style.display = 'flex';
    campaignMsgTimer = duration;
  }
  function updateCampaignLivesDisplay() {
    const el = document.getElementById('campaign-lives');
    if (!el) return;
    el.textContent = '❤'.repeat(Math.max(0, campaignLives));
  }
  function buildCapitalShip() {
    const group = new THREE.Group();
    const hullMat = new THREE.MeshStandardMaterial({ color: 0x16192a, metalness: 0.75, roughness: 0.38 });
    const accentMat = new THREE.MeshStandardMaterial({ color: 0x3d0909, metalness: 0.5, roughness: 0.6 });
    const glowMat = new THREE.MeshBasicMaterial({ color: 0xff3300 });
    const turretMat = new THREE.MeshStandardMaterial({ color: 0x20283a, metalness: 0.85, roughness: 0.28 });
    const barrelMat = new THREE.MeshStandardMaterial({ color: 0x343d50, metalness: 0.92, roughness: 0.18 });
    const hull = new THREE.Mesh(new THREE.BoxGeometry(200, 30, 360), hullMat);
    group.add(hull);
    const spine = new THREE.Mesh(new THREE.BoxGeometry(40, 10, 340), hullMat);
    spine.position.set(0, 20, 0);
    group.add(spine);
    for (const sx of [-115, 115]) {
      const wing = new THREE.Mesh(new THREE.BoxGeometry(36, 16, 260), hullMat);
      wing.position.set(sx, -5, 0);
      group.add(wing);
      const stripe = new THREE.Mesh(new THREE.BoxGeometry(38, 3, 260), accentMat);
      stripe.position.set(sx, 7, 0);
      group.add(stripe);
      const tipMat = new THREE.MeshBasicMaterial({ color: 0xff2200 });
      const tipLight = new THREE.Mesh(new THREE.SphereGeometry(2.5, 6, 4), tipMat);
      tipLight.position.set(sx, 0, 0);
      group.add(tipLight);
    }
    const bridge = new THREE.Mesh(new THREE.BoxGeometry(60, 30, 110), hullMat);
    bridge.position.set(0, 30, 55);
    group.add(bridge);
    const dome = new THREE.Mesh(
      new THREE.SphereGeometry(18, 10, 6, 0, Math.PI * 2, 0, Math.PI * 0.5), hullMat,
    );
    dome.position.set(0, 46, 65);
    group.add(dome);
    const winMat = new THREE.MeshBasicMaterial({ color: 0x44aaff, transparent: true, opacity: 0.8 });
    const winRow = new THREE.Mesh(new THREE.BoxGeometry(52, 4, 2), winMat);
    winRow.position.set(0, 32, 112);
    group.add(winRow);
    for (const z of [-120, -40, 40, 120]) {
      const acc = new THREE.Mesh(new THREE.BoxGeometry(202, 3, 4), accentMat);
      acc.position.set(0, 10, z);
      group.add(acc);
    }
    const enginePositions = [
      [-80, -4], [-48, -4], [-16, -4], [16, -4], [48, -4], [80, -4],
      [-38, 10], [38, 10],
    ];
    for (const [ex, ey] of enginePositions) {
      const ring = new THREE.Mesh(new THREE.CylinderGeometry(7, 7, 5, 10), accentMat);
      ring.rotation.x = Math.PI / 2;
      ring.position.set(ex, ey, -183);
      group.add(ring);
      const glow = new THREE.Mesh(new THREE.CircleGeometry(6.5, 10), glowMat);
      glow.position.set(ex, ey, -186);
      glow.rotation.y = Math.PI;
      group.add(glow);
    }
    const engLight = new THREE.PointLight(0xff3300, 4.5, 240);
    engLight.position.set(0, 0, -195);
    group.add(engLight);
    const runMat = new THREE.MeshBasicMaterial({ color: 0xffaa22 });
    for (let i = -6; i <= 6; i++) {
      if (i === 0) continue;
      const dot = new THREE.Mesh(new THREE.SphereGeometry(1.2, 5, 3), runMat);
      dot.position.set((i / 6) * 95, 16, 170);
      group.add(dot);
    }
    const turretLocalPositions = [
      new THREE.Vector3(-80, 18, 110),
      new THREE.Vector3(80, 18, 110),
      new THREE.Vector3(-80, 18, -110),
      new THREE.Vector3(80, 18, -110),
    ];
    capitalShipTurrets = [];
    for (let i = 0; i < turretLocalPositions.length; i++) {
      const tPos = turretLocalPositions[i];
      const base = new THREE.Mesh(new THREE.CylinderGeometry(7, 8, 5, 8), turretMat);
      base.position.copy(tPos);
      group.add(base);
      const pivot = new THREE.Group();
      pivot.position.copy(tPos).add(new THREE.Vector3(0, 4, 0));
      group.add(pivot);
      const head = new THREE.Mesh(new THREE.CylinderGeometry(6, 7, 6, 8), turretMat);
      pivot.add(head);
      const barrel = new THREE.Mesh(new THREE.BoxGeometry(2, 2, 28), barrelMat);
      barrel.position.set(0, 2, 14);
      pivot.add(barrel);
      const muzzleLight = new THREE.PointLight(0xff6600, 0, 45);
      muzzleLight.position.set(0, 2, 28);
      pivot.add(muzzleLight);
      capitalShipTurrets.push({ pivot, muzzleLight, localPos: tPos.clone(), fireTimer: i * 0.85 + 0.4 });
    }
    group.position.copy(CAPITAL_SHIP_BASE_POS);
    group.quaternion.setFromAxisAngle(new THREE.Vector3(0, 1, 0), Math.PI);
    scene.add(group);
    return group;
  }
  function updateCapitalShip(dt) {
    if (!capitalShipMesh || !bossActive || campaignOver) return;
    capitalShipTime += dt;
    capitalShipMesh.position.x = CAPITAL_SHIP_BASE_POS.x + 88 * Math.sin(capitalShipTime * 0.09);
    capitalShipMesh.position.y = CAPITAL_SHIP_BASE_POS.y + 9 * Math.sin(capitalShipTime * 0.055);
    for (let i = 0; i < BOSS_HITBOX_COUNT; i++) {
      const r = remotePlayers.get(BOSS_ID_BASE + i);
      if (r) {
        r.ship.position.copy(capitalShipMesh.position).add(BOSS_HB_OFFSETS_WORLD[i]);
        r.targetPos.copy(r.ship.position);
      }
    }
    const invQ = capitalShipMesh.quaternion.clone().invert();
    for (const t of capitalShipTurrets) {
      const muzzleWorld = new THREE.Vector3(0, 1.8, 22).applyQuaternion(t.pivot.quaternion);
      muzzleWorld.add(t.pivot.position).applyQuaternion(capitalShipMesh.quaternion).add(capitalShipMesh.position);
      const pivotWorld = t.pivot.position.clone().applyQuaternion(capitalShipMesh.quaternion).add(capitalShipMesh.position);
      const toPlayer = ship.position.clone().sub(pivotWorld);
      const localDir = toPlayer.clone().applyQuaternion(invQ);
      const yaw = Math.atan2(localDir.x, localDir.z);
      const horizDist = Math.sqrt(localDir.x * localDir.x + localDir.z * localDir.z);
      const pitch = -Math.atan2(localDir.y, horizDist);
      t.pivot.rotation.y = yaw;
      t.pivot.rotation.x = Math.max(-0.7, Math.min(0.7, pitch));
      if (myAlive) {
        t.fireTimer -= dt;
        if (t.fireTimer <= 0) {
          const fireDir = ship.position.clone().sub(muzzleWorld).normalize();
          fireDir.x += (Math.random() - 0.5) * 0.09;
          fireDir.y += (Math.random() - 0.5) * 0.09;
          fireDir.normalize();
          bullets.fire(muzzleWorld.clone(), fireDir, 'enemy');
          bossBullets.push({ pos: muzzleWorld.clone(), vel: fireDir.clone().multiplyScalar(430), life: 4.2 });
          t.muzzleLight.intensity = 7;
          setTimeout(() => { if (t.muzzleLight) t.muzzleLight.intensity = 0; }, 65);
          const hpFrac = bossHp / BOSS_MAX_HP;
          t.fireTimer = hpFrac > 0.65 ? 2.8 + Math.random() * 0.7
            : hpFrac > 0.35 ? 1.6 + Math.random() * 0.5
              : 0.9 + Math.random() * 0.3;
          audio.play('shoot');
        }
      }
    }
  }
  function spawnCampaignWave(waveIdx) {
    const wave = CAMPAIGN_WAVES[waveIdx];
    campaignWaveBotIds.clear();
    campaignBotsAlive = 0;
    const ENEMY_ANCHOR = new THREE.Vector3(0, 20, wave.spawnZ ?? 380);
    for (let i = 0; i < wave.count; i++) {
      const id = campaignNextBotId++;
      const pos = ENEMY_ANCHOR.clone().add(new THREE.Vector3(
        (Math.random() - 0.5) * 160,
        (Math.random() - 0.5) * 60,
        (Math.random() - 0.5) * 130,
      ));
      const bot = spawnBot(id, 1, pos, `Enemy ${i + 1}`);
      bot.record.isCampaignBot = true;
      campaignWaveBotIds.add(id);
      campaignBotsAlive++;
    }
    updateCampaignHud();
  }
  function applyBossHit(dmg) {
    if (!bossActive || campaignOver) return;
    bossHp = Math.max(0, bossHp - dmg);
    const centerHb = remotePlayers.get(BOSS_ID_BASE);
    if (centerHb) centerHb.hp = bossHp;
    updateCampaignHud();
    if (bossHp <= 0) endCampaignVictory();
  }
  function activateBossPhase() {
    bossActive = true;
    bossHp = BOSS_MAX_HP;
    bossFireTimer = 2.0;
    for (let i = 0; i < BOSS_HITBOX_COUNT; i++) {
      const r = remotePlayers.get(BOSS_ID_BASE + i);
      if (r) {
        const shipPos = capitalShipMesh ? capitalShipMesh.position : platformB.position;
        r.ship.position.copy(shipPos).add(BOSS_HB_OFFSETS_WORLD[i]);
        r.targetPos.copy(r.ship.position);
        r.alive = true;
        r.hasTarget = (i === 0);
      }
    }
    scores.set(BOSS_ID_BASE, { name: 'Capital Ship', kills: 0, deaths: 0, team: 1 });
    const barEl = document.getElementById('campaign-boss-bar');
    if (barEl) barEl.style.display = 'flex';
    const bossGlow = new THREE.PointLight(0xff2200, 3.5, 320);
    bossGlow.position.set(0, 30, -20);
    if (capitalShipMesh) capitalShipMesh.add(bossGlow); else scene.add(bossGlow);
    updateCampaignHud();
  }
  function fireFromBoss() {
    if (!myAlive || !bossActive) return;
    const hpFrac = bossHp / BOSS_MAX_HP;
    const count = hpFrac > 0.6 ? 2 : hpFrac > 0.3 ? 4 : 6;
    const spread = hpFrac > 0.6 ? 0.06 : hpFrac > 0.3 ? 0.10 : 0.15;
    const origin = platformB.position.clone().add(new THREE.Vector3(0, 5, -32));
    const toPlayer = ship.position.clone().sub(origin).normalize();
    for (let i = 0; i < count; i++) {
      const dir = toPlayer.clone().add(new THREE.Vector3(
        (Math.random() - 0.5) * spread * 2,
        (Math.random() - 0.5) * spread * 2,
        (Math.random() - 0.5) * spread * 2,
      )).normalize();
      bullets.fire(origin.clone(), dir, 'enemy');
      bossBullets.push({ pos: origin.clone(), vel: dir.clone().multiplyScalar(480), life: 3.5 });
    }
    audio.play('shoot');
  }
  function updateBoss(dt) {
    if (!bossActive || campaignOver) return;
    const PLAYER_HIT_R = 7.0;
    const BOSS_BULLET_DMG = 14;
    for (let i = bossBullets.length - 1; i >= 0; i--) {
      const b = bossBullets[i];
      b.pos.addScaledVector(b.vel, dt);
      b.life -= dt;
      if (b.life <= 0) { bossBullets.splice(i, 1); continue; }
      if (myAlive) {
        const dx = b.pos.x - ship.position.x;
        const dy = b.pos.y - ship.position.y;
        const dz = b.pos.z - ship.position.z;
        if (dx * dx + dy * dy + dz * dz < PLAYER_HIT_R * PLAYER_HIT_R) {
          applyPlayerDamageLocal(BOSS_BULLET_DMG, BOSS_ID_BASE, 1);
          bossBullets.splice(i, 1);
        }
      }
    }
  }
  function endCampaignVictory() {
    campaignOver = true;
    campaignPhase = 4;
    bossActive = false;
    for (let i = 0; i < BOSS_HITBOX_COUNT; i++) {
      const r = remotePlayers.get(BOSS_ID_BASE + i);
      if (r) { r.alive = false; r.hasTarget = false; }
    }
    localStorage.setItem(`spaceships:campaign${CAMPAIGN_MISSION}Beat`, '1');
    reportCampaignResult(CAMPAIGN_MISSION, campaignLives);
    const bossPos = (capitalShipMesh ? capitalShipMesh.position : platformB.position).clone();
    let k = 0;
    const explodeInterval = setInterval(() => {
      const off = new THREE.Vector3(
        (Math.random() - 0.5) * 90,
        (Math.random() - 0.5) * 36,
        (Math.random() - 0.5) * 70,
      );
      bullets.spawnExplosion(bossPos.clone().add(off), 3.5 + Math.random() * 4);
      if (k % 4 === 0) audio.play('shipdeath');
      k++;
      if (k >= 20) clearInterval(explodeInterval);
    }, 200);
    showCampaignMsg('CAPITAL SHIP DESTROYED\nMISSION COMPLETE', 99);
    setTimeout(() => {
      if (matchResultEl) {
        matchResultEl.innerHTML =
          `<span style="color:#ffd97a;display:block;margin-bottom:6px">MISSION COMPLETE</span>` +
          `<span class="sub" style="font-size:14px;color:#9cf">Capital Ship Destroyed — Sector Secured</span>` +
          `<button class="lobby-btn" id="btnBackToLobby" style="margin-top:18px">Return to Hangar</button>`;
        matchResultEl.style.display = 'block';
        const btn = matchResultEl.querySelector('#btnBackToLobby');
        if (btn) btn.addEventListener('click', () => {
          const overlay = document.getElementById('ad-overlay');
          const skipBtn = document.getElementById('ad-skip');
          if (overlay && skipBtn) {
            skipBtn.onclick = () => location.reload();
            overlay.style.display = 'flex';
            try { (window.adsbygoogle = window.adsbygoogle || []).push({}); } catch { }
          } else {
            location.reload();
          }
        });
      }
    }, 4500);
    updateCampaignHud();
  }
  function updateCampaign(dt) {
    if (campaignOver) return;
    if (campaignMsgTimer > 0) {
      campaignMsgTimer -= dt;
      if (campaignMsgTimer <= 0) {
        const el = document.getElementById('campaign-msg');
        if (el) el.style.display = 'none';
      }
    }
    if (campaignBetween) {
      campaignBetweenTimer -= dt;
      if (campaignBetweenTimer <= 0) {
        campaignBetween = false;
        if (campaignPhase === 3) {
          activateBossPhase();
        } else {
          spawnCampaignWave(campaignPhase);
        }
      }
      return;
    }
    if (campaignPhase < 3) {
      let alive = 0;
      for (const id of campaignWaveBotIds) {
        const r = remotePlayers.get(id);
        if (r && r.alive) alive++;
      }
      if (alive !== campaignBotsAlive) {
        campaignBotsAlive = alive;
        updateCampaignHud();
      }
      if (alive === 0 && campaignWaveBotIds.size > 0) {
        campaignWaveBotIds.clear();
        if (campaignPhase < 2) {
          const nextWave = CAMPAIGN_WAVES[campaignPhase + 1];
          campaignCheckpointPos = [0, 20, (nextWave?.spawnZ ?? 20) - 80];
          campaignPhase++;
          showCampaignMsg('WAVE COMPLETE\nPrepare for incoming hostiles...', 3.2);
          campaignBetween = true;
          campaignBetweenTimer = 3.5;
        } else {
          campaignCheckpointPos = [0, 10, 450];
          campaignPhase = 3;
          showCampaignMsg('CAPITAL SHIP SHIELDS OFFLINE\nPrepare to engage', 4.5);
          campaignBetween = true;
          campaignBetweenTimer = 4.8;
        }
        updateCampaignHud();
      }
    } else if (campaignPhase === 3) {
      updateBoss(dt);
      updateCapitalShip(dt);
    }
    if (campaignWarpActive) {
      campaignWarpTimer -= dt;
      if (campaignWarpTimer <= 0) {
        campaignWarpActive = false;
        const flashEl = document.getElementById('campaign-warp-flash');
        if (flashEl) flashEl.classList.remove('active');
      }
    }
  }
  function spawnSoloEntities() {
    if (SOLO_MODE === 'train') {
      const fwd = new THREE.Vector3(0, 0, 1).applyQuaternion(ship.quaternion);
      const pos = ship.position.clone().addScaledVector(fwd, 250);
      spawnBot(1, 1, pos, 'Bot');
    } else if (SOLO_MODE === 'skirmish') {
      const FRIENDLY_ANCHOR = isTerrainMap ? new THREE.Vector3(0, 40, -1400) : new THREE.Vector3(0, 0, -540);
      const ENEMY_ANCHOR = isTerrainMap ? new THREE.Vector3(0, 40, 1400) : new THREE.Vector3(0, 0, 540);
      const jitter = (range) => (Math.random() - 0.5) * range;
      for (let i = 0; i < 4; i++) {
        const pos = FRIENDLY_ANCHOR.clone().add(new THREE.Vector3(jitter(80), jitter(30), jitter(80)));
        spawnBot(1 + i, 0, pos, `Ally ${i + 1}`);
      }
      for (let i = 0; i < 5; i++) {
        const pos = ENEMY_ANCHOR.clone().add(new THREE.Vector3(jitter(80), jitter(30), jitter(80)));
        spawnBot(5 + i, 1, pos, `Enemy ${i + 1}`);
      }
    } else if (isCampaign) {
      spawnCampaignWave(0);
    }
  }
  if (isSolo) spawnSoloEntities();
  if (isCampaign) {
    capitalShipMesh = buildCapitalShip();
    const bossCenter = CAPITAL_SHIP_BASE_POS;
    for (let i = 0; i < BOSS_HITBOX_COUNT; i++) {
      const hbGroup = new THREE.Group();
      hbGroup.position.copy(bossCenter).add(BOSS_HB_OFFSETS_WORLD[i]);
      scene.add(hbGroup);
      const hbBox = document.createElement('div');
      hbBox.style.display = 'none';
      document.body.appendChild(hbBox);
      const hbLabel = document.createElement('div');
      hbBox.appendChild(hbLabel);
      const hbLead = document.createElement('div');
      hbLead.style.display = 'none';
      document.body.appendChild(hbLead);
      remotePlayers.set(BOSS_ID_BASE + i, {
        id: BOSS_ID_BASE + i,
        ship: hbGroup,
        targetPos: hbGroup.position.clone(),
        targetQuat: new THREE.Quaternion(),
        alive: false,
        team: 1,
        hasTarget: false,
        isBot: true,
        isBossHitbox: true,
        hp: BOSS_MAX_HP,
        hitFlash: 0,
        hitRadius: 28,
        marker: null,
        box: hbBox,
        lead: hbLead,
        label: hbLabel,
        vel: new THREE.Vector3(0, 0, 0),
      });
    }
  }
  function spawnMultiplayerBot(id, team, spawnPos, spawnQuat) {
    scores.set(id, { name: 'Bot [Hard]', team, kills: 0, deaths: 0 });
    const r = getOrCreateRemote(id);
    r.isBot = true;
    r.isMpBot = true;
    r.team = team;
    refreshMarker(r);
    r.alive = true;
    r.hp = SHIP_MAX_HP;
    r.ship.position.copy(spawnPos);
    r.ship.quaternion.copy(spawnQuat);
    r.targetPos.copy(spawnPos);
    r.targetQuat.copy(spawnQuat);
    r.hasTarget = true;
    const localOpponent = {
      id: myId,
      get team() { return myTeam; },
      get position() { return ship.position; },
      get velocity() { return shipVelocity; },
      get alive() { return myAlive; },
      takeHit(_dmg, killerBotId) {
        if (ws && ws.readyState === WebSocket.OPEN) {
          ws.send(JSON.stringify({ type: 'hit', targetId: myId, fromBotId: killerBotId, kind: 'bullet' }));
        }
      },
    };
    function makeRemoteOpponent(rp) {
      return {
        id: rp.id,
        get team() { return rp.team; },
        get position() { return rp.ship.position; },
        get velocity() { return rp.vel; },
        get alive() { return rp.alive; },
        takeHit(_dmg, killerBotId) {
          if (ws && ws.readyState === WebSocket.OPEN) {
            ws.send(JSON.stringify({ type: 'hit', targetId: rp.id, fromBotId: killerBotId, kind: 'bullet' }));
          }
        },
      };
    }
    const ai = createBotAI(r, {
      team,
      faction: team === myTeam ? 'ally' : 'enemy',
      beams, bullets, asteroids, obstacles,
      solveIntercept, raySphereDist, audio, distanceVol,
      hardMode: true,
      terrainHeightFn: isTerrainMap ? getTerrainHeight : null,
      missileMax: 3,
      fireMissile: (targetEntity) => {
        const targetRecord = targetEntity.id === myId
          ? localShipRecord
          : (remotePlayers.get(targetEntity.id) ?? null);
        if (!targetRecord) return false;
        const fwd = new THREE.Vector3(0, 0, 1).applyQuaternion(r.ship.quaternion);
        const mslOrigin = r.ship.position.clone().addScaledVector(fwd, 6);
        missileSystem.fire(mslOrigin, fwd, targetRecord, id, team);
        audio.play('shoot', distanceVol(r.ship.position));
        if (ws && ws.readyState === WebSocket.OPEN) {
          ws.send(JSON.stringify({
            type: 'bot-fire',
            botId: id,
            kind: 'missile',
            shots: [{
              pos: [mslOrigin.x, mslOrigin.y, mslOrigin.z],
              dir: [fwd.x, fwd.y, fwd.z],
              targetId: targetEntity.id,
            }],
          }));
        }
        return true;
      },
      onFire: (start, dir) => {
        if (ws && ws.readyState === WebSocket.OPEN) {
          ws.send(JSON.stringify({
            type: 'bot-fire',
            botId: id,
            kind: 'bullet',
            shots: [{ pos: [start.x, start.y, start.z], dir: [dir.x, dir.y, dir.z] }],
          }));
        }
      },
      getOpponents: () => {
        const out = [];
        if (localOpponent.team !== team) out.push(localOpponent);
        for (const [, rp] of remotePlayers) {
          if (rp.alive && rp.team !== team) out.push(makeRemoteOpponent(rp));
        }
        return out;
      },
    });

    mpBots.push({ id, team, record: r, ai });
  }
  if (!isSolo && opts.host && Array.isArray(opts.botAssignments)) {
    for (const ba of opts.botAssignments) {
      const pos = new THREE.Vector3().fromArray(ba.pos || [0, 0, 0]);
      const quat = new THREE.Quaternion().fromArray(ba.quat || [0, 0, 0, 1]);
      spawnMultiplayerBot(ba.id, ba.team, pos, quat);
    }
  }
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
    if (myInvulnTimer > 0) return;
    healthIdleDamage = 0;
    myHp = Math.max(0, myHp - dmg);
    if (myHp <= 0) {
      audio.play('shipdeath');
      killSelf();
      if (isCampaign && !campaignOver) {
        campaignLives = Math.max(0, campaignLives - 1);
        updateCampaignLivesDisplay();
        if (campaignLives <= 0) {
          campaignOver = true;
          myRespawnTimer = 0;
          const failEl = document.getElementById('campaign-failed');
          if (failEl) failEl.style.display = 'flex';
          const retryBtn = document.getElementById('btnRetryMission');
          if (retryBtn) retryBtn.onclick = () => location.reload();
          const returnBtn = document.getElementById('btnFailedReturn');
          if (returnBtn) returnBtn.onclick = () => location.reload();
        } else {
          campaignWarpActive = true;
          campaignWarpTimer = 1.5;
          myRespawnTimer = 1.5;
          const flashEl = document.getElementById('campaign-warp-flash');
          if (flashEl) { flashEl.classList.remove('active'); void flashEl.offsetWidth; flashEl.classList.add('active'); }
        }
      } else {
        myRespawnTimer = RESPAWN_DELAY;
      }
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
    if (isCampaign && r.isCampaignBot) return;
    let anchor;
    if (SOLO_MODE === 'skirmish') {
      anchor = r.team === 0
        ? (isTerrainMap ? new THREE.Vector3(0, 40, -1400) : new THREE.Vector3(0, 0, -540))
        : (isTerrainMap ? new THREE.Vector3(0, 40, 1400) : new THREE.Vector3(0, 0, 540));
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
      } else if (isCampaign) {
        pos = campaignCheckpointPos.slice();
        quat = [0, 0, 0, 1];
      } else {
        const spawnZ = isTerrainMap ? -1400 : -540;
        const spawnY = isTerrainMap ? 40 : 0;
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
    if (isCampaign) {
      myHp = Math.floor(SHIP_MAX_HP * 0.55); // respawn at 55% HP
    }
  }
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
  const trialsHudEl = document.getElementById('trials-hud');
  const trialsTimerEl = document.getElementById('trials-timer');
  const trialsCpEl = document.getElementById('trials-checkpoint');
  const trialsBestEl = document.getElementById('trials-best');
  const trialsLastEl = document.getElementById('trials-last');
  const trialsLapEl = document.getElementById('trials-lap');
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
  if (isCampaign) {
    const hudEl = document.getElementById('campaign-hud');
    if (hudEl) hudEl.style.display = 'flex';
    updateCampaignHud();
    updateCampaignLivesDisplay();
    showCampaignMsg(MISSION_BRIEFINGS[CAMPAIGN_MISSION] || MISSION_BRIEFINGS[1], 4.5);
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
      const btn = matchResultEl.querySelector('#btnBackToLobby');
      if (btn) btn.addEventListener('click', () => {
        const overlay = document.getElementById('ad-overlay');
        const skipBtn = document.getElementById('ad-skip');
        if (overlay && skipBtn) {
          skipBtn.onclick = () => location.reload();
          overlay.style.display = 'flex';
          try { (window.adsbygoogle = window.adsbygoogle || []).push({}); } catch { }
        } else {
          location.reload();
        }
      });
    }
  }
  let pauseOpen = false;
  let pauseFocusIdx = 0;
  let pauseNavCooldown = 0;
  let pausePrevNavUp = false, pausePrevNavDown = false, pausePrevConfirm = false;
  const pauseOverlay = document.createElement('div');
  Object.assign(pauseOverlay.style, {
    position: 'fixed', inset: '0', display: 'none',
    flexDirection: 'column', alignItems: 'center', justifyContent: 'center',
    gap: '14px', zIndex: '9999', pointerEvents: 'auto',
    background: 'rgba(4,8,18,0.85)', backdropFilter: 'blur(14px)',
  });
  pauseOverlay.innerHTML = `
    <div style="font-family:'Orbitron',sans-serif;font-size:clamp(14px,2.5vw,22px);
      color:#4aa3ff;letter-spacing:8px;text-transform:uppercase;font-weight:800;
      margin-bottom:8px;text-shadow:0 0 24px rgba(74,163,255,0.55)">PAUSED</div>
    <button id="pauseResume"    class="big" style="min-width:220px">▶ &nbsp;RESUME</button>
    <button id="pauseBackLobby" class="big" style="min-width:220px">← &nbsp;BACK TO LOBBY</button>
    <div style="font-family:'Orbitron',sans-serif;font-size:10px;color:#3a5070;
      margin-top:10px;letter-spacing:2px;text-transform:uppercase">
      A / Click — confirm &nbsp;·&nbsp; Start — close
    </div>`;
  document.body.appendChild(pauseOverlay);
  const pauseResumeBtn = document.getElementById('pauseResume');
  const pauseBackLobbyBtn = document.getElementById('pauseBackLobby');
  const pauseBtns = [pauseResumeBtn, pauseBackLobbyBtn];
  function openPause() {
    pauseOpen = true;
    pauseOverlay.style.display = 'flex';
    pauseFocusIdx = 0;
    pauseResumeBtn.focus();
  }
  function closePause() {
    pauseOpen = false;
    pauseOverlay.style.display = 'none';
  }
  pauseResumeBtn.addEventListener('click', closePause);
  pauseBackLobbyBtn.addEventListener('click', () => {
    if (window.confirm('Leave match and return to menu?')) window.location.reload();
  });
  window.addEventListener('keydown', (e) => {
    if (e.code !== 'Tab' || e.repeat) return;
    e.preventDefault();
    if (scoreboardEl) scoreboardEl.classList.toggle('visible');
  }, true);
  // Ships, bolts and debris spawn throughout the match, so the ultra material
  // pass runs on a slow cadence rather than once. Already-upgraded materials
  // are tracked in a WeakSet, so repeat sweeps only cost the traversal.
  let ultraSweepTimer = 0;
  function ultraSweep(dt) {
    if (!ULTRA) return;
    ultraSweepTimer -= dt;
    if (ultraSweepTimer > 0) return;
    ultraSweepTimer = 0.5;
    sweepScene(scene);
  }
  function loop() {
    try {
      const dt = Math.min(0.05, clock.getDelta());
      update(dt);
      touchHud.update();
      ultraSweep(dt);
      renderFrame(dt);
    } catch (err) {
      console.error('Game loop error:', err);
    }
    requestAnimationFrame(loop);
  }
  window.addEventListener('resize', () => {
    camera.aspect = window.innerWidth / window.innerHeight;
    camera.updateProjectionMatrix();
    renderer.setSize(window.innerWidth, window.innerHeight);
    if (ultraFx) ultraFx.setSize(window.innerWidth, window.innerHeight);
    if (pixelRT) {
      pixelRT.setSize(
        Math.max(1, Math.floor(window.innerWidth / PIXEL_SCALE)),
        Math.max(1, Math.floor(window.innerHeight / PIXEL_SCALE)),
      );
    }
  });
  loop();
}