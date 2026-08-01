import * as THREE from 'three';
import { createDash } from './dash.js';

// Cockpit profiles are authored in SHIP-LOCAL units, before SHIP_SCALE (1.5) is applied
// by main.js. Both GLB models face +X and are rotated -PI/2 about Y in ship.js, which maps
// model (x, y, z) -> ship (-z, y, x). That is why ship forward is +Z everywhere in main.js.
//
// Anchors below were measured from the GLB node hierarchy with node transforms applied:
//
//   spaceship.glb       Cockpit node   ship-local x[-0.5..0.5] y[-0.1..1.2] z[0.1..2.6]
//   spaceshipADMIN.glb  Cylinder.002   ship-local x[-0.2..0.2] y[0.6..0.9]  z[3.2..4.6]
//                       (the 'glass' material — a small blister far forward)
//
// The admin canopy glass is far too small to sit inside literally, so its interior shell is
// built larger than the real blister. That is safe because the exterior hull and the interior
// are never visible at the same time: interior renders only in first person, and the hull is
// culled only in first person (see setExteriorVisible in main.js).

export const COCKPIT_PROFILES = {
  default: {
    id: 'default',
    // Seated eye point, upper half of the canopy volume, set back so the dash reads ahead.
    eye: new THREE.Vector3(0, 0.60, 1.15),
    fov: 82,
    tub: { halfWidth: 0.66, floorY: -0.02, ceilY: 1.28, backZ: 0.05, dashZ: 2.15 },
    accent: 0x66ddff,
  },
  admin: {
    id: 'admin',
    eye: new THREE.Vector3(0, 0.74, 3.55),
    fov: 84,
    tub: { halfWidth: 0.74, floorY: 0.08, ceilY: 1.34, backZ: 2.70, dashZ: 4.55 },
    accent: 0xffc451,
  },
};

export function getCockpitProfile(isAdmin) {
  return isAdmin ? COCKPIT_PROFILES.admin : COCKPIT_PROFILES.default;
}

// Shared low-poly kit, driven entirely by the profile's `tub` box + eye anchor, so both ships
// get proportionally correct interiors from the same code.
export function createCockpit(profile) {
  const { eye, tub, accent } = profile;
  const { halfWidth: HW, floorY, ceilY, backZ, dashZ } = tub;

  const group = new THREE.Group();
  group.name = 'CockpitInterior';

  const panelMat = new THREE.MeshStandardMaterial({
    color: 0x333b44, roughness: 0.9, metalness: 0.1, emissive: 0x11161d, emissiveIntensity: 1,
  });
  const frameMat = new THREE.MeshStandardMaterial({
    color: 0x515c68, roughness: 0.55, metalness: 0.45, emissive: 0x14191f, emissiveIntensity: 1,
  });
  const rubberMat = new THREE.MeshStandardMaterial({
    color: 0x1c2126, roughness: 1.0, metalness: 0.0,
  });
  const seatMat = new THREE.MeshStandardMaterial({
    color: 0x2d3238, roughness: 0.95, metalness: 0.02,
  });
  const strapMat = new THREE.MeshStandardMaterial({
    color: 0x59503a, roughness: 0.95, metalness: 0.0,
  });
  const accentMat = new THREE.MeshBasicMaterial({ color: accent });

  const box = (w, h, d, mat, x, y, z) => {
    const m = new THREE.Mesh(new THREE.BoxGeometry(w, h, d), mat);
    m.position.set(x, y, z);
    group.add(m);
    return m;
  };

  const strut = (a, b, r, mat) => {
    const from = new THREE.Vector3(...a);
    const dir = new THREE.Vector3(...b).sub(from);
    const len = dir.length();
    const m = new THREE.Mesh(new THREE.CylinderGeometry(r, r, len, 8), mat);
    m.position.copy(from).addScaledVector(dir, 0.5);
    m.quaternion.setFromUnitVectors(new THREE.Vector3(0, 1, 0), dir.normalize());
    group.add(m);
    return m;
  };

  const innerLen = dashZ - backZ;
  const dashTopY = eye.y - 0.28;
  const dashBotY = floorY + 0.04;
  // Walls and roof stop beside/behind the pilot; everything forward of this is canopy glass,
  // otherwise the shell closes in and the forward view becomes a letterbox slot.
  const wallFrontZ = eye.z + 0.30;
  const roofFrontZ = eye.z + 0.12;
  const tubLen = wallFrontZ - backZ;
  const tubMidZ = (backZ + wallFrontZ) / 2;

  // ---- shell -------------------------------------------------------------------------
  box(HW * 2, 0.05, innerLen, panelMat, 0, floorY, (backZ + dashZ) / 2);           // floor pan
  box(0.05, ceilY - floorY, tubLen, panelMat, -HW, (floorY + ceilY) / 2, tubMidZ); // left wall
  box(0.05, ceilY - floorY, tubLen, panelMat, HW, (floorY + ceilY) / 2, tubMidZ);  // right wall
  box(HW * 2, 0.05, roofFrontZ - backZ, panelMat, 0, ceilY, (backZ + roofFrontZ) / 2); // roof
  box(HW * 2, ceilY - floorY, 0.05, panelMat, 0, (floorY + ceilY) / 2, backZ);     // bulkhead

  // ---- side consoles -----------------------------------------------------------------
  for (const s of [-1, 1]) {
    const conLen = (dashZ - 0.14) - backZ;
    const conZ = (backZ + dashZ - 0.14) / 2;
    const c = box(0.16, 0.26, conLen, frameMat,
      s * (HW - 0.09), floorY + 0.28, conZ);
    c.rotation.z = s * 0.12;
    // glowing edge strip along the console top
    box(0.02, 0.012, conLen * 0.8, accentMat,
      s * (HW - 0.17), floorY + 0.425, conZ);
  }

  // ---- instrument panel + glareshield ------------------------------------------------
  const dash = box(HW * 1.85, dashTopY - dashBotY, 0.07, panelMat,
    0, (dashTopY + dashBotY) / 2, dashZ - 0.06);
  dash.rotation.x = 0.30; // top leans away from the pilot
  const hood = box(HW * 1.9, 0.045, 0.26, frameMat, 0, dashTopY + 0.05, dashZ - 0.20);
  hood.rotation.x = -0.22;

  // ---- canopy frame ------------------------------------------------------------------
  for (const s of [-1, 1]) {
    // A-pillar: dash outer corner up and back to the roof leading edge.
    strut([s * HW * 0.97, dashTopY, dashZ - 0.10],
      [s * HW * 0.93, ceilY - 0.04, roofFrontZ], 0.019, frameMat);
    // canopy rail along the top of each wall
    box(0.045, 0.045, tubLen, frameMat, s * (HW - 0.02), ceilY - 0.05, tubMidZ);
  }
  // overhead spine
  box(0.05, 0.05, roofFrontZ - backZ, frameMat, 0, ceilY - 0.04, (backZ + roofFrontZ) / 2);

  // ---- ejection seat -----------------------------------------------------------------
  const seatZ = eye.z - 0.26;
  box(0.52, 0.10, 0.44, seatMat, 0, floorY + 0.20, seatZ + 0.06);            // seat pan
  const back = box(0.50, 0.72, 0.10, seatMat, 0, eye.y - 0.12, seatZ - 0.18);
  back.rotation.x = -0.10;                                                    // seat back
  box(0.34, 0.16, 0.10, rubberMat, 0, eye.y + 0.26, seatZ - 0.20);           // headrest
  for (const s of [-1, 1]) {
    box(0.07, 0.42, 0.34, seatMat, s * 0.26, eye.y - 0.22, seatZ - 0.02);    // bolsters
    const strap = box(0.07, 0.46, 0.03, strapMat, s * 0.17, eye.y - 0.40, seatZ + 0.22);
    strap.rotation.x = 0.55;                                                  // harness
  }

  // ---- control stick (pivots at its base) --------------------------------------------
  const stick = new THREE.Group();
  stick.position.set(-(HW - 0.13), floorY + 0.40, eye.z + 0.26);
  group.add(stick);
  const shaft = new THREE.Mesh(new THREE.CylinderGeometry(0.024, 0.030, 0.20, 10), frameMat);
  shaft.position.y = 0.10;
  stick.add(shaft);
  const boot = new THREE.Mesh(new THREE.CylinderGeometry(0.060, 0.080, 0.06, 10), rubberMat);
  boot.position.y = 0.025;
  stick.add(boot);
  const grip = new THREE.Mesh(new THREE.BoxGeometry(0.070, 0.14, 0.085), rubberMat);
  grip.position.y = 0.25;
  stick.add(grip);
  const trigger = new THREE.Mesh(new THREE.BoxGeometry(0.03, 0.035, 0.02), accentMat);
  trigger.position.set(0, 0.25, 0.050);
  stick.add(trigger);

  // ---- throttle lever (pivots on the left console) -----------------------------------
  const throttle = new THREE.Group();
  throttle.position.set(HW - 0.13, floorY + 0.44, eye.z + 0.04);
  group.add(throttle);
  const tArm = new THREE.Mesh(new THREE.BoxGeometry(0.05, 0.05, 0.26), frameMat);
  tArm.position.z = 0.12;
  throttle.add(tArm);
  const tKnob = new THREE.Mesh(new THREE.BoxGeometry(0.10, 0.09, 0.09), rubberMat);
  tKnob.position.z = 0.26;
  throttle.add(tKnob);

  // ---- instruments -------------------------------------------------------------------
  const dash2 = createDash(profile, accent);
  group.add(dash2.group);

  // ---- cockpit fill light ------------------------------------------------------------
  // Keeps the interior readable on every map regardless of scene lighting.
  const lamp = new THREE.PointLight(0xbcd6ff, 3.0, 4.5, 2);
  lamp.position.set(0, ceilY - 0.14, eye.z + 0.15);
  group.add(lamp);

  // Tag everything so ship.js colour customisation and main.js exterior culling skip it.
  group.userData.isInterior = true;
  group.traverse((o) => {
    o.userData.isInterior = true;
    if (o.isMesh) { o.castShadow = false; o.receiveShadow = false; }
  });

  return {
    group,
    update(dt, tel) {
      const sx = tel?.steerX ?? 0;
      const sy = tel?.steerY ?? 0;
      const thr = tel?.throttle01 ?? 0;
      // Stick pulls back to pitch up (steerY < 0 is nose-up in main.js).
      stick.rotation.x = THREE.MathUtils.damp(stick.rotation.x, sy * 0.34, 12, dt);
      stick.rotation.z = THREE.MathUtils.damp(stick.rotation.z, sx * 0.34, 12, dt);
      // Idle fully back, firewalled fully forward.
      throttle.rotation.x = THREE.MathUtils.damp(
        throttle.rotation.x, THREE.MathUtils.lerp(0.55, -0.55, thr), 8, dt);
      dash2.update(dt, tel);
    },
  };
}
