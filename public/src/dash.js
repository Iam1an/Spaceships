import * as THREE from 'three';

// Live cockpit instruments. Everything here is driven from state main.js already tracks, and
// is deliberately chunky and high-contrast: the default pixel filter (main.js PIXEL_SCALE = 3)
// renders the scene at a third of resolution, so fine detail turns to mush.
//
// Two large displays rather than three: a centre stick sits between the pilot's knees and
// would bisect a centre screen, so the panel centre is left to physical detail instead.

const CW = 256;
const CH = 128;

// A canvas-backed screen that redraws only when its rendered values actually change.
function createScreen(w, h, drawFn) {
  const canvas = document.createElement('canvas');
  canvas.width = CW;
  canvas.height = CH;
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
      ctx.fillStyle = '#05080b';
      ctx.fillRect(0, 0, CW, CH);
      drawFn(ctx, state, false);
      tex.needsUpdate = true;
    },
  };
}

function bar(ctx, x, y, w, h, frac, color) {
  ctx.fillStyle = '#0d151c';
  ctx.fillRect(x, y, w, h);
  ctx.fillStyle = color;
  ctx.fillRect(x, y, Math.max(0, Math.min(1, frac)) * w, h);
  ctx.strokeStyle = '#2b3a47';
  ctx.lineWidth = 2;
  ctx.strokeRect(x, y, w, h);
}

function label(ctx, text, x, y, color = '#7fa6c0') {
  ctx.fillStyle = color;
  ctx.font = 'bold 15px monospace';
  ctx.fillText(text, x, y);
}

// Aim a panel at the pilot's eye. Called while the mesh is still unparented, so its local
// frame is the cockpit frame the eye anchor is expressed in.
function faceAtEye(mesh, eye) {
  mesh.lookAt(eye);
}

export function createDash(profile, accent) {
  const { eye, tub } = profile;
  const { halfWidth: HW, floorY, dashZ } = tub;
  const dashTopY = eye.y - 0.22;
  const dashBotY = floorY + 0.04;
  const panelH = dashTopY - dashBotY;
  const accentHex = '#' + new THREE.Color(accent).getHexString();

  const group = new THREE.Group();
  group.name = 'CockpitDash';

  // Usable panel width is only what sits BETWEEN the side consoles (0.17 wide, centred at
  // HW - 0.10), otherwise the outer displays end up buried inside the console boxes.
  const innerHalf = HW - 0.20;
  let scrW = innerHalf * 0.88;
  const scrH = Math.min(scrW / 2, panelH * 0.82);
  scrW = scrH * 2;
  const scrX = scrW / 2 + 0.035;
  const screenZ = dashZ - 0.20;
  const screenY = dashTopY - scrH / 2 - 0.03;

  // --- flight display (pilot's left; +X renders on screen-left) --------------------------
  const flight = createScreen(scrW, scrH, (ctx, s, keyOnly) => {
    const spd = Math.round(s.speed);
    const thr = Math.round(s.throttle01 * 100);
    const hp = Math.round(s.hpFrac * 100);
    if (keyOnly) return `${spd}|${thr}|${hp}|${Math.round(s.boost01 * 20)}|${s.boosting ? 1 : 0}`;
    label(ctx, 'SPD', 10, 26, accentHex);
    ctx.fillStyle = '#eaf6ff';
    ctx.font = 'bold 40px monospace';
    ctx.fillText(String(spd).padStart(3, ' '), 56, 30);
    bar(ctx, 10, 44, 236, 18, s.throttle01, s.boosting ? '#ff9d3d' : '#3ddcff');
    label(ctx, `THR ${thr}%`, 14, 58, '#04202c');
    bar(ctx, 10, 70, 236, 20, s.hpFrac,
      s.hpFrac > 0.5 ? '#4ade80' : s.hpFrac > 0.25 ? '#facc15' : '#ff4d4d');
    label(ctx, `HULL ${hp}%`, 14, 85, '#04202c');
    bar(ctx, 10, 98, 236, 18, s.boost01, '#3d9dff');
    label(ctx, 'BOOST', 14, 112, '#04202c');
    return null;
  });
  flight.mesh.position.set(scrX, screenY, screenZ);
  faceAtEye(flight.mesh, eye);
  group.add(flight.mesh);

  // --- weapons display (pilot's right) ----------------------------------------------------
  const weapons = createScreen(scrW, scrH, (ctx, s, keyOnly) => {
    if (keyOnly) {
      return `${s.missiles}|${s.flares}|${Math.round(s.heat01 * 20)}|${s.gunMode}`
        + `|${Math.round(s.charge01 * 20)}`;
    }
    label(ctx, s.gunMode === 'beam' ? 'BEAM' : 'GUN', 10, 24, accentHex);
    bar(ctx, 74, 8, 172, 18, s.heat01, s.heat01 > 0.2 ? '#ff8a3d' : '#ff3d3d');
    label(ctx, 'MSL', 10, 56);
    for (let i = 0; i < 4; i++) {
      ctx.fillStyle = i < s.missiles ? '#ff9d3d' : '#16222c';
      ctx.fillRect(62 + i * 32, 40, 24, 20);
    }
    label(ctx, 'FLR', 10, 90);
    for (let i = 0; i < 3; i++) {
      ctx.fillStyle = i < s.flares ? '#ffe23d' : '#16222c';
      ctx.fillRect(62 + i * 32, 74, 24, 20);
    }
    bar(ctx, 10, 100, 236, 16, s.charge01, s.charge01 >= 1 ? '#ff4d4d' : '#c084fc');
    label(ctx, 'CHARGE', 14, 113, '#04202c');
    return null;
  });
  weapons.mesh.position.set(-scrX, screenY, screenZ);
  faceAtEye(weapons.mesh, eye);
  group.add(weapons.mesh);

  // Emissive bezels so the displays read as lit panels set into the dash.
  for (const x of [scrX, -scrX]) {
    const bez = new THREE.Mesh(
      new THREE.PlaneGeometry(scrW + 0.016, scrH + 0.016),
      new THREE.MeshBasicMaterial({ color: accent, toneMapped: false }),
    );
    bez.position.set(x, screenY, screenZ + 0.004);
    faceAtEye(bez, eye);
    group.add(bez);
  }

  // --- annunciators on the glareshield -----------------------------------------------------
  //   TGT  — your reticle is aligned on an enemy   (main.js bestAlignment < 22)
  //   MSL  — an enemy missile is locking YOU       (missileSystem.isTargetingLocal)
  const lampGeo = new THREE.BoxGeometry(HW * 0.26, 0.048, 0.02);
  const mkLamp = (x, onColor) => {
    const mat = new THREE.MeshBasicMaterial({ color: 0x0b0f13, toneMapped: false });
    const m = new THREE.Mesh(lampGeo, mat);
    m.position.set(x, dashTopY + 0.052, dashZ - 0.255);
    faceAtEye(m, eye);
    group.add(m);
    return { mat, on: new THREE.Color(onColor), off: new THREE.Color(0x0b0f13) };
  };
  // lookAt spins these planes 180 degrees about Y, mirroring texture space relative to world
  // space, so the lamp X is flipped to line up under its label.
  const tgtLamp = mkLamp(HW * 0.34, 0x38ff9b);
  const mslLamp = mkLamp(-HW * 0.34, 0xff3b30);

  const labels = createScreen(HW * 0.78, HW * 0.14, (ctx, s, keyOnly) => {
    if (keyOnly) return 'static';
    ctx.fillStyle = '#6d8698';
    ctx.font = 'bold 23px monospace';
    ctx.fillText('TGT LOCK', 8, 84);
    ctx.fillText('MSL WARN', 138, 84);
    return null;
  });
  labels.mesh.position.set(0, dashTopY + 0.098, dashZ - 0.295);
  faceAtEye(labels.mesh, eye);
  group.add(labels.mesh);
  labels.redraw({});

  let blink = 0;

  return {
    group,
    update(dt, s) {
      flight.redraw(s);
      weapons.redraw(s);
      blink += dt;
      // Missile warning blinks fast and urgent; target lock pulses slower and steadier.
      const mslOn = s.missileLock && (blink % 0.34) < 0.17;
      const tgtOn = s.targetLock && (blink % 0.60) < 0.42;
      tgtLamp.mat.color.copy(tgtOn ? tgtLamp.on : tgtLamp.off);
      mslLamp.mat.color.copy(mslOn ? mslLamp.on : mslLamp.off);
    },
  };
}
