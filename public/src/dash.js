import * as THREE from 'three';

// Live cockpit instruments. Everything here is driven from state main.js already tracks, and
// is deliberately chunky and high-contrast: the default pixel filter (main.js PIXEL_SCALE = 3)
// renders the scene at a third of resolution, so fine detail turns to mush.

const CANVAS_W = 256;
const CANVAS_H = 128;

// A canvas-backed screen that redraws only when its rendered values actually change.
function createScreen(w, h, drawFn) {
  const canvas = document.createElement('canvas');
  canvas.width = CANVAS_W;
  canvas.height = CANVAS_H;
  const ctx = canvas.getContext('2d');
  const tex = new THREE.CanvasTexture(canvas);
  tex.magFilter = THREE.NearestFilter;
  tex.minFilter = THREE.LinearFilter;
  const mat = new THREE.MeshBasicMaterial({ map: tex, toneMapped: false });
  const mesh = new THREE.Mesh(new THREE.PlaneGeometry(w, h), mat);
  let lastKey = null;
  return {
    mesh,
    redraw(state) {
      const key = drawFn(ctx, state, true);
      if (key === lastKey) return;
      lastKey = key;
      ctx.clearRect(0, 0, CANVAS_W, CANVAS_H);
      ctx.fillStyle = '#0a0f14';
      ctx.fillRect(0, 0, CANVAS_W, CANVAS_H);
      drawFn(ctx, state, false);
      tex.needsUpdate = true;
    },
  };
}

function bar(ctx, x, y, w, h, frac, color, label) {
  ctx.fillStyle = '#16202a';
  ctx.fillRect(x, y, w, h);
  ctx.fillStyle = color;
  ctx.fillRect(x, y, Math.max(0, Math.min(1, frac)) * w, h);
  ctx.strokeStyle = '#3d5062';
  ctx.lineWidth = 2;
  ctx.strokeRect(x, y, w, h);
  if (label) {
    ctx.fillStyle = '#8fb4cc';
    ctx.font = 'bold 15px monospace';
    ctx.fillText(label, x, y - 6);
  }
}

// Aim a panel at the pilot's eye. Called while the mesh is still unparented, so its local
// frame is the cockpit frame the eye anchor is expressed in.
function faceAtEye(mesh, eye) {
  mesh.lookAt(eye);
}

export function createDash(profile, accent) {
  const { eye, tub } = profile;
  const { halfWidth: HW, floorY } = tub;
  const dashZ = tub.dashZ;
  const dashTopY = eye.y - 0.28;
  const dashBotY = floorY + 0.04;
  const faceY = (dashTopY + dashBotY) / 2;
  const panelH = dashTopY - dashBotY;
  // Usable panel width is only what sits BETWEEN the side consoles (each 0.16 wide, centred
  // at HW - 0.09), otherwise the outer displays end up buried inside the console boxes.
  const innerHalf = HW - 0.17;
  const totalW = innerHalf * 2;
  let mfdW = totalW * 0.46;
  const mfdH = Math.min(mfdW / 2, panelH * 0.85);
  mfdW = mfdH * 2;
  let sideW = totalW * 0.235;
  const sideH = Math.min(sideW / 1.73, panelH * 0.72);
  sideW = sideH * 1.73;
  const sideX = mfdW / 2 + sideW / 2 + totalW * 0.015;
  // Sit the displays clear of the tilted panel's top edge, which was clipping them.
  const screenZ = dashZ - 0.20;
  const group = new THREE.Group();
  group.name = 'CockpitDash';

  const accentHex = '#' + new THREE.Color(accent).getHexString();

  // --- main multifunction display: throttle / speed / hull -----------------------------
  const mfd = createScreen(mfdW, mfdH, (ctx, s, keyOnly) => {
    const spd = Math.round(s.speed);
    const thr = Math.round(s.throttle01 * 100);
    const hp = Math.round(s.hpFrac * 100);
    if (keyOnly) return `${spd}|${thr}|${hp}|${s.boosting ? 1 : 0}`;
    ctx.fillStyle = accentHex;
    ctx.font = 'bold 20px monospace';
    ctx.fillText('SPD', 10, 24);
    ctx.fillStyle = '#eaf6ff';
    ctx.font = 'bold 34px monospace';
    ctx.fillText(String(spd).padStart(3, ' '), 62, 26);
    bar(ctx, 10, 44, 236, 20, s.throttle01, s.boosting ? '#ff9d3d' : '#3ddcff', null);
    ctx.fillStyle = '#8fb4cc';
    ctx.font = 'bold 14px monospace';
    ctx.fillText(`THR ${thr}%`, 12, 60);
    bar(ctx, 10, 82, 236, 22, s.hpFrac,
      s.hpFrac > 0.5 ? '#4ade80' : s.hpFrac > 0.25 ? '#facc15' : '#ff4d4d', null);
    ctx.fillStyle = '#0a0f14';
    ctx.font = 'bold 15px monospace';
    ctx.fillText(`HULL ${hp}%`, 14, 99);
    return null;
  });
  mfd.mesh.position.set(0, faceY, screenZ);
  faceAtEye(mfd.mesh, eye);
  group.add(mfd.mesh);

  // --- left screen: weapons ------------------------------------------------------------
  const wep = createScreen(sideW, sideH, (ctx, s, keyOnly) => {
    if (keyOnly) return `${s.missiles}|${s.flares}|${Math.round(s.heat01 * 20)}|${s.gunMode}`;
    ctx.fillStyle = accentHex;
    ctx.font = 'bold 17px monospace';
    ctx.fillText(s.gunMode === 'beam' ? 'BEAM' : 'GUN', 10, 22);
    bar(ctx, 74, 6, 172, 18, s.heat01, s.heat01 > 0.2 ? '#ff8a3d' : '#ff3d3d', null);
    ctx.fillStyle = '#8fb4cc';
    ctx.font = 'bold 16px monospace';
    ctx.fillText('MSL', 10, 56);
    for (let i = 0; i < 4; i++) {
      ctx.fillStyle = i < s.missiles ? '#ff9d3d' : '#22303c';
      ctx.fillRect(62 + i * 30, 40, 22, 18);
    }
    ctx.fillStyle = '#8fb4cc';
    ctx.fillText('FLR', 10, 92);
    for (let i = 0; i < 3; i++) {
      ctx.fillStyle = i < s.flares ? '#ffe23d' : '#22303c';
      ctx.fillRect(62 + i * 30, 76, 22, 18);
    }
    return null;
  });
  wep.mesh.position.set(-sideX, faceY, screenZ);
  faceAtEye(wep.mesh, eye);
  group.add(wep.mesh);

  // --- right screen: boost / drift charge ----------------------------------------------
  const eng = createScreen(sideW, sideH, (ctx, s, keyOnly) => {
    if (keyOnly) return `${Math.round(s.boost01 * 20)}|${Math.round(s.charge01 * 20)}`;
    ctx.fillStyle = '#8fb4cc';
    ctx.font = 'bold 16px monospace';
    ctx.fillText('BOOST', 10, 24);
    bar(ctx, 10, 32, 236, 22, s.boost01, '#3d9dff', null);
    ctx.fillStyle = '#8fb4cc';
    ctx.fillText('CHARGE', 10, 82);
    bar(ctx, 10, 90, 236, 22, s.charge01, s.charge01 >= 1 ? '#ff4d4d' : '#c084fc', null);
    return null;
  });
  eng.mesh.position.set(sideX, faceY, screenZ);
  faceAtEye(eng.mesh, eye);
  group.add(eng.mesh);

  // --- annunciator lights on the glareshield ------------------------------------------
  // Two distinct states the game already tracks:
  //   TGT  — your reticle is aligned on an enemy      (main.js bestAlignment < 22)
  //   MSL  — an enemy missile is locking YOU          (missileSystem.isTargetingLocal)
  const lampGeo = new THREE.BoxGeometry(HW * 0.34, 0.075, 0.02);
  const mkLamp = (x, onColor) => {
    const mat = new THREE.MeshBasicMaterial({ color: 0x14181d, toneMapped: false });
    const m = new THREE.Mesh(lampGeo, mat);
    m.position.set(x, dashTopY + 0.125, dashZ - 0.225);
    faceAtEye(m, eye);
    group.add(m);
    return { mesh: m, mat, onColor: new THREE.Color(onColor), off: new THREE.Color(0x14181d) };
  };
  const tgtLamp = mkLamp(HW * 0.42, 0x38ff9b);
  const mslLamp = mkLamp(-HW * 0.42, 0xff3b30);

  // Labels above the lamps so they read as instruments, not just glowing blocks.
  const labels = createScreen(HW * 1.0, HW * 0.18, (ctx, s, keyOnly) => {
    if (keyOnly) return 'static';
    ctx.fillStyle = '#7f97a8';
    ctx.font = 'bold 22px monospace';
    ctx.fillText('TGT LOCK', 8, 84);
    ctx.fillText('MSL WARN', 140, 84);
    return null;
  });
  labels.mesh.position.set(0, dashTopY + 0.205, dashZ - 0.245);
  faceAtEye(labels.mesh, eye);
  group.add(labels.mesh);
  labels.redraw({});

  let blink = 0;

  return {
    group,
    update(dt, s) {
      mfd.redraw(s);
      wep.redraw(s);
      eng.redraw(s);

      blink += dt;
      // Missile warning blinks fast and urgent; target lock pulses slower and steadier.
      const mslOn = s.missileLock && (blink % 0.34) < 0.17;
      const tgtOn = s.targetLock && (blink % 0.60) < 0.42;
      tgtLamp.mat.color.copy(tgtOn ? tgtLamp.onColor : tgtLamp.off);
      mslLamp.mat.color.copy(mslOn ? mslLamp.onColor : mslLamp.off);
    },
  };
}
