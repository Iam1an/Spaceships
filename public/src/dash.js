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
function createScreen(w, h, drawFn, cw = CW, ch = CH) {
  const canvas = document.createElement('canvas');
  canvas.width = cw;
  canvas.height = ch;
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
      ctx.fillRect(0, 0, cw, ch);
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
  const dashTopY = tub.dashTop;
  // Panel depth is fixed rather than measured down to the floor: on the admin hull the
  // floor sits high (clear of the spine), which would otherwise leave a 0.13-deep panel.
  // The panel simply hangs below the floor's front edge there.
  const dashBotY = dashTopY - 0.36;
  const panelH = dashTopY - dashBotY;
  const accentHex = '#' + new THREE.Color(accent).getHexString();

  const group = new THREE.Group();
  group.name = 'CockpitDash';

  // Usable panel width is only what sits BETWEEN the side consoles (0.17 wide, centred at
  // HW - 0.10), otherwise the outer displays end up buried inside the console boxes.
  const innerHalf = HW - 0.20;
  const totalW = innerHalf * 2;
  const gap = totalW * 0.022;
  const radarS = Math.min(totalW * 0.26, panelH * 0.80);
  let scrW = (totalW - radarS - gap * 2) / 2;
  const scrH = Math.min(scrW / 2, panelH * 0.80);
  scrW = scrH * 2;
  const scrX = radarS / 2 + gap + scrW / 2;
  const screenZ = dashZ - 0.20;
  const screenY = dashTopY - Math.max(scrH, radarS) / 2 - 0.03;

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

  // --- radar scope, panel centre ----------------------------------------------------------
  // Heading-up: contacts arrive already rotated into the ship's frame by main.js. lookAt
  // mirrors texture space against world space, so canvas-right corresponds to ship -X.
  const RS = 160;
  let sweep = 0;
  const radar = createScreen(radarS, radarS, (ctx, s, keyOnly) => {
    // Key changes every frame so the memoisation in createScreen never skips the sweep.
    // Returning null here would match the initial lastKey and the scope would never draw.
    if (keyOnly) return `${sweep.toFixed(3)}:${(s.contacts ?? []).length}`;
    const c = RS / 2;
    ctx.strokeStyle = '#1d3a4a';
    ctx.lineWidth = 3;
    for (const r of [0.33, 0.66, 1.0]) {
      ctx.beginPath();
      ctx.arc(c, c, c * r * 0.92, 0, Math.PI * 2);
      ctx.stroke();
    }
    ctx.beginPath();
    ctx.moveTo(c, c - c * 0.92); ctx.lineTo(c, c + c * 0.92);
    ctx.moveTo(c - c * 0.92, c); ctx.lineTo(c + c * 0.92, c);
    ctx.stroke();
    // sweep arm
    ctx.strokeStyle = accentHex;
    ctx.globalAlpha = 0.75;
    ctx.beginPath();
    ctx.moveTo(c, c);
    ctx.lineTo(c + Math.sin(sweep) * c * 0.92, c - Math.cos(sweep) * c * 0.92);
    ctx.stroke();
    ctx.globalAlpha = 1;
    for (const ct of s.contacts ?? []) {
      const px = c - ct.x * c * 0.92;
      const py = c - ct.z * c * 0.92;
      ctx.fillStyle = ct.hostile ? '#ff4d4d' : '#46ff9b';
      ctx.fillRect(px - 5, py - 5, 10, 10);
    }
    // own ship
    ctx.fillStyle = '#eaf6ff';
    ctx.fillRect(c - 3, c - 3, 6, 6);
    return null;
  }, RS, RS);
  radar.mesh.position.set(0, screenY, screenZ);
  faceAtEye(radar.mesh, eye);
  group.add(radar.mesh);

  // Emissive bezels so the displays read as lit panels set into the dash.
  for (const [x, w, h] of [[scrX, scrW, scrH], [-scrX, scrW, scrH], [0, radarS, radarS]]) {
    const bez = new THREE.Mesh(
      new THREE.PlaneGeometry(w + 0.016, h + 0.016),
      new THREE.MeshBasicMaterial({ color: accent, toneMapped: false }),
    );
    bez.position.set(x, screenY, screenZ + 0.004);
    faceAtEye(bez, eye);
    group.add(bez);
  }

  // --- annunciators on the glareshield -----------------------------------------------------
  //   TGT  — your reticle is aligned on an enemy   (main.js bestAlignment < 22)
  //   MSL  — an enemy missile is locking YOU       (missileSystem.isTargetingLocal)
  // Split outboard rather than sitting as one centred plate: on a long-nosed hull the centre
  // of the glareshield is exactly the sightline to your own nose, and the plate covered it.
  const lampGeo = new THREE.BoxGeometry(HW * 0.28, 0.050, 0.02);
  const mkLamp = (x, onColor, text) => {
    const mat = new THREE.MeshBasicMaterial({ color: 0x0b0f13, toneMapped: false });
    const m = new THREE.Mesh(lampGeo, mat);
    m.position.set(x, dashTopY + 0.048, dashZ - 0.255);
    faceAtEye(m, eye);
    group.add(m);
    const cap = createScreen(HW * 0.34, HW * 0.10, (ctx, s, keyOnly) => {
      if (keyOnly) return 'static';
      ctx.fillStyle = '#6d8698';
      // 40px-tall canvas: a 54px baseline drew the caption off the bottom edge.
      ctx.font = 'bold 26px monospace';
      ctx.fillText(text, 8, 31);
      return null;
    }, 128, 40);
    cap.mesh.position.set(x, dashTopY + 0.098, dashZ - 0.285);
    faceAtEye(cap.mesh, eye);
    group.add(cap.mesh);
    cap.redraw({});
    return { mat, on: new THREE.Color(onColor), off: new THREE.Color(0x0b0f13) };
  };
  // lookAt spins these planes 180 degrees about Y, mirroring texture space relative to world
  // space, so the lamp X is flipped to line up under its label.
  const tgtLamp = mkLamp(HW * 0.66, 0x38ff9b, 'TGT');
  const mslLamp = mkLamp(-HW * 0.66, 0xff3b30, 'MSL');

  let blink = 0;

  return {
    group,
    update(dt, s) {
      flight.redraw(s);
      weapons.redraw(s);
      sweep = (sweep + dt * 2.2) % (Math.PI * 2);
      radar.redraw(s);
      blink += dt;
      // Missile warning blinks fast and urgent; target lock pulses slower and steadier.
      const mslOn = s.missileLock && (blink % 0.34) < 0.17;
      const tgtOn = s.targetLock && (blink % 0.60) < 0.42;
      tgtLamp.mat.color.copy(tgtOn ? tgtLamp.on : tgtLamp.off);
      mslLamp.mat.color.copy(mslOn ? mslLamp.on : mslLamp.off);
    },
  };
}
