import * as THREE from 'three';
import { createDash } from './dash.js';

// Cockpit profiles are authored in SHIP-LOCAL units, before SHIP_SCALE (1.5) is applied
// by main.js. Both GLB models face +X and are rotated -PI/2 about Y in ship.js, which maps
// model (x, y, z) -> ship (-z, y, x). That is why ship forward is +Z everywhere in main.js.
// With forward +Z and up +Y in a right-handed frame, the pilot's RIGHT is -X.
//
// Anchors were measured from the GLB node hierarchy with node transforms applied:
//
//   spaceship.glb       Cockpit node   ship-local x[-0.5..0.5] y[-0.1..1.2] z[0.1..2.6]
//   spaceshipADMIN.glb  Cylinder.002   ship-local x[-0.2..0.2] y[0.6..0.9]  z[3.2..4.6]
//
// The interior is built larger than the real canopy glass. That is safe because the exterior
// hull and the interior are never visible at the same time (see setExteriorVisible in main.js).
//
// Layout follows a real fighter: the tub sides only rise to a canopy RAIL at roughly shoulder
// height, and everything above is open bubble canopy carried on two thin hoops. A boxed-in
// roof and full-height walls are what made the first pass feel like a grey crate.

const COCKPIT_PROFILES = {
  default: {
    id: 'default',
    // Seated above the fuselage spine but well back in the blister (ship-local z 0.1..2.6),
    // so the nose runs out ahead of the panel and the wing roots sit in peripheral view.
    // The rail is dropped further below the eye than the panel top, to open the sides up.
    eye: new THREE.Vector3(0, 1.26, 0.40),
    fov: 84,
    tub: {
      halfWidth: 0.60,
      floorY: 0.64,
      railY: 0.92,   // canopy rail: top of the solid tub sides
      backZ: -0.70,
      dashZ: 1.50,
      dashTop: 1.04,   // panel top; the canopy rail lines up with it
    },
    accent: 0x5fd8ff,
    lampColor: 0x9fd0ff,
  },
  admin: {
    id: 'admin',
    // Seat kept where it reads best. Two hull facts constrain this one hard: the spine
    // cylinder (ship-local z -1.9..6.9, top y 0.9) passes straight through the cockpit, so
    // dropping the floor to see more ship makes the spine punch through the footwell; and
    // the only structure forward of z 4.6 is that same thin spine, so there is very little
    // ship ahead of the seat to see in the first place. Raising the seat clear of the spine
    // pushed the panel far enough below the eye that the instruments left the screen.
    eye: new THREE.Vector3(0, 1.16, 4.15),
    fov: 86,
    tub: {
      halfWidth: 0.74,
      floorY: 0.54,
      railY: 0.86,
      backZ: 3.05,
      dashZ: 5.15,
      dashTop: 0.94,
    },
    accent: 0xffc451,
    lampColor: 0xffd39a,
  },
};

// Cockpit lamps and interior meshes share this layer so the lamps cannot spill onto the
// exterior hull. Interior meshes stay on layer 0 as well, so the camera still renders them.
const COCKPIT_LAYER = 1;

export function getCockpitProfile(isAdmin) {
  return isAdmin ? COCKPIT_PROFILES.admin : COCKPIT_PROFILES.default;
}

export function createCockpit(profile) {
  const { eye, tub, accent, lampColor } = profile;
  const { halfWidth: HW, floorY, railY, backZ, dashZ } = tub;

  const group = new THREE.Group();
  group.name = 'CockpitInterior';

  // Real cockpits are near-black. Keeping albedo very low is what lets the emissive strips
  // and instrument glow read as the actual light sources in here.
  const panelMat = new THREE.MeshStandardMaterial({ color: 0x0e1216, roughness: 0.95, metalness: 0.05 });
  const frameMat = new THREE.MeshStandardMaterial({ color: 0x1a2027, roughness: 0.6, metalness: 0.5 });
  const trimMat = new THREE.MeshStandardMaterial({ color: 0x252c34, roughness: 0.5, metalness: 0.6 });
  const rubberMat = new THREE.MeshStandardMaterial({ color: 0x080a0c, roughness: 1.0, metalness: 0.0 });
  const seatMat = new THREE.MeshStandardMaterial({ color: 0x121619, roughness: 0.98, metalness: 0.0 });
  const strapMat = new THREE.MeshStandardMaterial({ color: 0x3a3527, roughness: 0.95, metalness: 0.0 });
  const stickMat = new THREE.MeshStandardMaterial({ color: 0x0b0e11, roughness: 0.92, metalness: 0.06 });

  const glowMat = (hex) => new THREE.MeshBasicMaterial({ color: hex, toneMapped: false });
  const accentMat = glowMat(accent);

  const box = (w, h, d, mat, x, y, z) => {
    const m = new THREE.Mesh(new THREE.BoxGeometry(w, h, d), mat);
    m.position.set(x, y, z);
    group.add(m);
    return m;
  };

  const dashTopY = tub.dashTop;
  // Panel depth is fixed rather than measured down to the floor: on the admin hull the
  // floor sits high (clear of the spine), which would otherwise leave a 0.13-deep panel.
  // The panel simply hangs below the floor's front edge there.
  const dashBotY = dashTopY - 0.36;
  const railZ0 = backZ;
  const railZ1 = dashZ - 0.10;
  const railLen = railZ1 - railZ0;
  const railMidZ = (railZ0 + railZ1) / 2;

  // ---- floor: solid, with a raised footwell deck ----------------------------------------
  // A glazed floor read as a missing floor rather than a window, so the tub is closed now.
  // Downward context comes from the hull itself, which stays drawn in first person.
  box(HW * 2, 0.04, dashZ - backZ, panelMat, 0, floorY, (backZ + dashZ) / 2);
  // tread plates + a longitudinal rib, so the floor isn't a single flat slab
  for (const x of [-HW * 0.60, HW * 0.60]) {
    box(0.30, 0.022, (dashZ - eye.z) * 0.8, trimMat, x, floorY + 0.03, eye.z + (dashZ - eye.z) * 0.5);
  }
  box(0.09, 0.035, dashZ - backZ, frameMat, 0, floorY + 0.028, (backZ + dashZ) / 2);
  // footwell lighting, washing up off the deck
  for (const s of [-1, 1]) {
    box(0.016, 0.008, (dashZ - eye.z) * 0.6, glowMat(lampColor),
      s * (HW - 0.30), floorY + 0.045, eye.z + (dashZ - eye.z) * 0.55);
  }

  // ---- tub sides: only up to the canopy rail ---------------------------------------------
  for (const s of [-1, 1]) {
    box(0.04, railY - floorY, railLen, panelMat, s * HW, (floorY + railY) / 2, railMidZ);
    // rail cap plus the light strip washing down into the tub
    box(0.075, 0.05, railLen, trimMat, s * (HW - 0.02), railY + 0.02, railMidZ);
    box(0.022, 0.012, railLen * 0.86, accentMat, s * (HW - 0.055), railY - 0.012, railMidZ);
  }
  box(HW * 2, railY - floorY, 0.04, panelMat, 0, (floorY + railY) / 2, backZ);

  // ---- side consoles, kept below the rail -------------------------------------------------
  const conLen = (dashZ - 0.18) - backZ;
  const conZ = (backZ + dashZ - 0.18) / 2;
  for (const s of [-1, 1]) {
    const c = box(0.17, 0.20, conLen, frameMat, s * (HW - 0.10), floorY + 0.20, conZ);
    c.rotation.z = s * 0.10;
    // switch banks: rows of tiny lit caps, the main "cockpit full of lights" read
    for (let i = 0; i < 7; i++) {
      const z = conZ - conLen * 0.34 + i * (conLen * 0.11);
      const hue = i % 3 === 0 ? 0xff5a3c : i % 3 === 1 ? 0x46ff9b : 0xffd24a;
      box(0.055, 0.022, 0.038, trimMat, s * (HW - 0.075), floorY + 0.30, z);
      box(0.030, 0.010, 0.030, glowMat(hue), s * (HW - 0.135), floorY + 0.312, z);
    }
  }

  // ---- instrument panel + glareshield ------------------------------------------------------
  const dash = box(HW * 1.8, dashTopY - dashBotY, 0.06, panelMat,
    0, (dashTopY + dashBotY) / 2, dashZ - 0.06);
  dash.rotation.x = 0.30;
  const hood = box(HW * 1.85, 0.035, 0.20, trimMat, 0, dashTopY + 0.045, dashZ - 0.17);
  hood.rotation.x = -0.22;
  // downward wash from under the glareshield onto the panel
  box(HW * 1.5, 0.010, 0.020, glowMat(lampColor), 0, dashTopY + 0.030, dashZ - 0.33);

  // ---- canopy: two thin hoops, nothing else above the rail ---------------------------------
  const hoop = (z, radius, tube) => {
    const m = new THREE.Mesh(new THREE.TorusGeometry(radius, tube, 6, 20, Math.PI), frameMat);
    m.position.set(0, railY, z);
    group.add(m);
    return m;
  };
  const rearHoopZ = eye.z - 0.34;
  hoop(railZ1, HW, 0.017);              // windscreen bow
  hoop(rearHoopZ, HW * 0.99, 0.015);    // rear hoop, behind the head
  // slim spine linking the two, well above the sightline
  box(0.026, 0.026, railZ1 - rearHoopZ, frameMat,
    0, railY + HW - 0.02, (railZ1 + rearHoopZ) / 2);

  // ---- ejection seat -------------------------------------------------------------------------
  const seatZ = eye.z - 0.26;
  box(0.50, 0.09, 0.44, seatMat, 0, floorY + 0.17, seatZ + 0.06);
  const back = box(0.48, 0.74, 0.09, seatMat, 0, eye.y - 0.14, seatZ - 0.18);
  back.rotation.x = -0.10;
  box(0.32, 0.15, 0.10, rubberMat, 0, eye.y + 0.28, seatZ - 0.20);
  for (const s of [-1, 1]) {
    box(0.06, 0.44, 0.34, seatMat, s * 0.25, eye.y - 0.24, seatZ - 0.02);
    const strap = box(0.07, 0.46, 0.03, strapMat, s * 0.17, eye.y - 0.42, seatZ + 0.22);
    strap.rotation.x = 0.55;
  }

  // ---- centre stick, between the pilot's knees ------------------------------------------------
  const stick = new THREE.Group();
  stick.position.set(0, floorY + 0.02, eye.z + 0.30);
  group.add(stick);
  const boot = new THREE.Mesh(new THREE.CylinderGeometry(0.072, 0.095, 0.07, 10), rubberMat);
  boot.position.y = 0.03;
  stick.add(boot);
  const shaft = new THREE.Mesh(new THREE.CylinderGeometry(0.022, 0.030, 0.24, 10), stickMat);
  shaft.position.y = 0.13;
  stick.add(shaft);
  const grip = new THREE.Mesh(new THREE.BoxGeometry(0.056, 0.15, 0.078), stickMat);
  grip.position.y = 0.29;
  grip.rotation.x = -0.16;
  stick.add(grip);
  const gripTop = new THREE.Mesh(new THREE.BoxGeometry(0.060, 0.028, 0.082), trimMat);
  gripTop.position.set(0, 0.365, 0.013);
  gripTop.rotation.x = -0.16;
  stick.add(gripTop);
  const trigger = new THREE.Mesh(new THREE.BoxGeometry(0.026, 0.038, 0.018), accentMat);
  trigger.position.set(0, 0.28, 0.048);
  stick.add(trigger);
  const hat = new THREE.Mesh(new THREE.BoxGeometry(0.030, 0.014, 0.030), glowMat(0xff5a3c));
  hat.position.set(0, 0.358, -0.022);
  stick.add(hat);

  // ---- throttle lever, left console --------------------------------------------------------------
  const throttle = new THREE.Group();
  throttle.position.set(HW - 0.14, floorY + 0.34, eye.z + 0.02);
  group.add(throttle);
  const tArm = new THREE.Mesh(new THREE.BoxGeometry(0.045, 0.045, 0.26), frameMat);
  tArm.position.z = 0.12;
  throttle.add(tArm);
  const tKnob = new THREE.Mesh(new THREE.BoxGeometry(0.10, 0.085, 0.10), rubberMat);
  tKnob.position.z = 0.26;
  throttle.add(tKnob);
  const tLed = new THREE.Mesh(new THREE.BoxGeometry(0.055, 0.010, 0.022), accentMat);
  tLed.position.set(0, 0.048, 0.26);
  throttle.add(tLed);

  // ---- rudder pedals, seen through the chin glazing ------------------------------------------------
  for (const s of [-1, 1]) {
    const pedal = box(0.115, 0.135, 0.022, stickMat, s * 0.20, floorY + 0.065, eye.z + 0.72);
    pedal.rotation.x = 0.62;
    box(0.05, 0.016, 0.24, frameMat, s * 0.20, floorY + 0.018, eye.z + 0.60);
  }

  // ---- fill light ------------------------------------------------------------------------------------
  // High and behind the head. Sitting it just above the stick meant a decay-2 point light
  // was effectively inside the grip, washing the whole thing out to pale grey.
  // Scoped to COCKPIT_LAYER so they light the interior only. Now that the hull stays drawn
  // in first person, unscoped point lights this close blew its inner surfaces out to white.
  const lamp = new THREE.PointLight(lampColor, 1.3, 2.6, 2);
  lamp.position.set(0, railY + 0.38, eye.z - 0.06);
  lamp.layers.set(COCKPIT_LAYER);
  group.add(lamp);
  const panelLamp = new THREE.PointLight(accent, 0.8, 0.75, 2);
  panelLamp.position.set(0, dashTopY + 0.02, dashZ - 0.15);
  panelLamp.layers.set(COCKPIT_LAYER);
  group.add(panelLamp);

  const dash2 = createDash(profile, accent);
  group.add(dash2.group);

  // Tag everything so ship.js colour customisation and main.js exterior culling skip it.
  group.userData.isInterior = true;
  group.traverse((o) => {
    o.userData.isInterior = true;
    if (o.isMesh) {
      o.castShadow = false;
      o.receiveShadow = false;
      // Keep layer 0 (so the camera draws it) and add the cockpit lighting layer.
      o.layers.enable(COCKPIT_LAYER);
    }
  });

  return {
    group,
    update(dt, tel) {
      const sx = tel?.steerX ?? 0;
      const sy = tel?.steerY ?? 0;
      const thr = tel?.throttle01 ?? 0;
      // Stick pulls back to pitch up; steering right tilts the grip toward -X (pilot's right).
      stick.rotation.x = THREE.MathUtils.damp(stick.rotation.x, sy * 0.30, 12, dt);
      stick.rotation.z = THREE.MathUtils.damp(stick.rotation.z, sx * 0.30, 12, dt);
      throttle.rotation.x = THREE.MathUtils.damp(
        throttle.rotation.x, THREE.MathUtils.lerp(0.55, -0.55, thr), 8, dt);
      dash2.update(dt, tel);
    },
  };
}
