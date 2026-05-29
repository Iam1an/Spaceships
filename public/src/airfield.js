import * as THREE from 'three';

// Airfield half-extents for AABB collision (same pattern as mothership)
export const AIRFIELD_HALF = new THREE.Vector3(280, 4, 190);

export function createAirfield(team = 0) {
  const group = new THREE.Group();
  group.name = 'Airfield';

  const tarmacColor  = team === 0 ? 0x3a3a3a : 0x3a3335;
  const buildingColor = team === 0 ? 0x7a8a6a : 0x8a7a6a;
  const accentColor  = team === 0 ? 0x4466aa : 0xaa6644;

  const tarmacMat   = new THREE.MeshStandardMaterial({ color: tarmacColor, roughness: 0.95, metalness: 0.0 });
  const buildingMat = new THREE.MeshStandardMaterial({ color: buildingColor, roughness: 0.8, metalness: 0.1 });
  const accentMat   = new THREE.MeshStandardMaterial({ color: accentColor, roughness: 0.6, metalness: 0.3 });
  const windowMat   = new THREE.MeshBasicMaterial({ color: 0xaaddff });
  const lineMat     = new THREE.MeshBasicMaterial({ color: 0xddddbb });
  const redMat      = new THREE.MeshBasicMaterial({ color: 0xff3300 });
  const greenMat    = new THREE.MeshBasicMaterial({ color: 0x33ff66 });

  // ── Tarmac base slab ──────────────────────────────────────────────────────
  const RW = 560, RD = 380, RH = 3;
  const tarmac = new THREE.Mesh(new THREE.BoxGeometry(RW, RH, RD), tarmacMat);
  tarmac.position.y = -RH / 2;   // top face at y=0
  group.add(tarmac);

  // ── Runway centreline & threshold markings ────────────────────────────────
  // Long centreline
  const cl = new THREE.Mesh(new THREE.BoxGeometry(4, 0.05, RD * 0.82), lineMat);
  group.add(cl);
  // Threshold bars (8 short dashes at each end)
  for (const endZ of [-RD * 0.38, RD * 0.38]) {
    for (let k = -3; k <= 3; k++) {
      const bar = new THREE.Mesh(new THREE.BoxGeometry(18, 0.05, 6), lineMat);
      bar.position.set(k * 28, 0, endZ);
      group.add(bar);
    }
  }
  // Taxiway yellow lines
  const taxiMat = new THREE.MeshBasicMaterial({ color: 0xddaa00 });
  for (const x of [-RW * 0.3, RW * 0.3]) {
    const taxi = new THREE.Mesh(new THREE.BoxGeometry(2, 0.05, RD * 0.6), taxiMat);
    taxi.position.x = x;
    group.add(taxi);
  }

  // ── Control tower ─────────────────────────────────────────────────────────
  const TW = 20, TH = 55, TD = 20;
  const tower = new THREE.Mesh(new THREE.BoxGeometry(TW, TH, TD), buildingMat);
  tower.position.set(-RW * 0.35, TH / 2, -RD * 0.28);
  group.add(tower);
  // Cab (glass box on top)
  const cab = new THREE.Mesh(new THREE.BoxGeometry(TW + 4, 10, TD + 4), accentMat);
  cab.position.set(-RW * 0.35, TH + 5, -RD * 0.28);
  group.add(cab);
  const cabGlass = new THREE.Mesh(new THREE.BoxGeometry(TW + 2, 8, TD + 2), windowMat);
  cabGlass.position.set(-RW * 0.35, TH + 5, -RD * 0.28);
  group.add(cabGlass);
  // Antenna mast
  const mast = new THREE.Mesh(new THREE.CylinderGeometry(0.4, 0.4, 18, 6), accentMat);
  mast.position.set(-RW * 0.35, TH + 19, -RD * 0.28);
  group.add(mast);
  // Rotating radar dish (cosmetic flat circle)
  const radar = new THREE.Mesh(new THREE.CylinderGeometry(5, 5, 0.5, 12), accentMat);
  radar.position.set(-RW * 0.35 + 6, TH + 22, -RD * 0.28);
  radar.rotation.z = Math.PI / 3;
  group.add(radar);

  // ── Hangars ───────────────────────────────────────────────────────────────
  for (const [hx, hz] of [[-RW * 0.3, RD * 0.32], [RW * 0.3, RD * 0.32]]) {
    const HW = 90, HH = 28, HD = 60;
    const hangar = new THREE.Mesh(new THREE.BoxGeometry(HW, HH, HD), buildingMat);
    hangar.position.set(hx, HH / 2, hz);
    group.add(hangar);
    // Curved roof hint (slightly taller box on top, accent colour)
    const roof = new THREE.Mesh(new THREE.BoxGeometry(HW, 6, HD * 1.02), accentMat);
    roof.position.set(hx, HH + 3, hz);
    group.add(roof);
    // Hangar door opening (dark inset)
    const doorMat = new THREE.MeshBasicMaterial({ color: 0x111111 });
    const door = new THREE.Mesh(new THREE.BoxGeometry(HW * 0.7, HH * 0.75, 1), doorMat);
    door.position.set(hx, HH * 0.375, hz - HD / 2 - 0.3);
    group.add(door);
  }

  // ── Fuel tanks ────────────────────────────────────────────────────────────
  const tankMat = new THREE.MeshStandardMaterial({ color: 0x888866, roughness: 0.7 });
  for (const [tx, tz] of [[RW * 0.42, -RD * 0.15], [RW * 0.42, RD * 0.15]]) {
    const tank = new THREE.Mesh(new THREE.CylinderGeometry(10, 10, 20, 12), tankMat);
    tank.position.set(tx, 10, tz);
    group.add(tank);
  }

  // ── Perimeter lights ──────────────────────────────────────────────────────
  const pLight = new THREE.PointLight(accentColor, 2.0, 200);
  pLight.position.set(0, 8, 0);
  group.add(pLight);

  // Threshold/approach lights
  for (let k = 0; k < 6; k++) {
    const approach = new THREE.Mesh(new THREE.SphereGeometry(1.2, 6, 4),
      k % 2 === 0 ? redMat : greenMat);
    approach.position.set(k * 14 - 35, 1, -RD * 0.45);
    group.add(approach);
  }

  // Runway edge lights
  for (let k = -4; k <= 4; k++) {
    for (const side of [-1, 1]) {
      const rl = new THREE.Mesh(new THREE.SphereGeometry(0.8, 5, 4), redMat);
      rl.position.set(side * RW * 0.24, 0.5, k * (RD * 0.09));
      group.add(rl);
    }
  }

  return group;
}
