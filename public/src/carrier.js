import * as THREE from 'three';

// Aircraft carrier: long flat deck with an island superstructure on the
// starboard (right) side. Deck sits at y=0 so ships spawn just above it.
export const CARRIER_HALF = new THREE.Vector3(100, 10, 30);

export function createCarrier(team = 0) {
  const group = new THREE.Group();
  group.name = 'Carrier';

  const hullColor  = team === 0 ? 0x4a7080 : 0x7a5040;
  const deckColor  = team === 0 ? 0x5a8090 : 0x8a6050;
  const accentColor = 0x2a2a2a;

  const hullMat   = new THREE.MeshStandardMaterial({ color: hullColor,  metalness: 0.4, roughness: 0.7 });
  const deckMat   = new THREE.MeshStandardMaterial({ color: deckColor,  metalness: 0.2, roughness: 0.9 });
  const accentMat = new THREE.MeshStandardMaterial({ color: accentColor, metalness: 0.6, roughness: 0.5 });

  // Main hull — wide flat box, deck at y=0
  const DECK_W = 200, DECK_H = 8, DECK_L = 60;
  const hull = new THREE.Mesh(new THREE.BoxGeometry(DECK_W, DECK_H, DECK_L), hullMat);
  hull.position.y = -DECK_H / 2;  // top face at y=0
  group.add(hull);

  // Flight deck surface (thin slab on top)
  const deck = new THREE.Mesh(new THREE.BoxGeometry(DECK_W * 0.98, 1.2, DECK_L * 0.94), deckMat);
  deck.position.y = 0.6;
  group.add(deck);

  // Island superstructure — starboard side, forward third
  const islandMat = new THREE.MeshStandardMaterial({ color: hullColor, metalness: 0.5, roughness: 0.6 });
  const island = new THREE.Mesh(new THREE.BoxGeometry(18, 24, 14), islandMat);
  island.position.set(DECK_W * 0.25, 12, -DECK_L * 0.28);
  group.add(island);

  // Radar mast on top of island
  const mast = new THREE.Mesh(new THREE.CylinderGeometry(0.5, 0.5, 14, 6), accentMat);
  mast.position.set(DECK_W * 0.25, 28, -DECK_L * 0.28);
  group.add(mast);

  // Angled flight deck extension at bow
  const bowExt = new THREE.Mesh(new THREE.BoxGeometry(30, 1.2, DECK_L * 0.7), deckMat);
  bowExt.position.set(-DECK_W * 0.5 - 12, 0.6, DECK_L * 0.1);
  bowExt.rotation.y = 0.22; // angled ~12.5°
  group.add(bowExt);

  // Hull waterline stripes
  for (const z of [-DECK_L * 0.45, DECK_L * 0.45]) {
    const stripe = new THREE.Mesh(new THREE.BoxGeometry(DECK_W * 1.01, 1.0, 0.5), accentMat);
    stripe.position.set(0, -2, z);
    group.add(stripe);
  }

  // Deck centre-line markings (simple lighter box stripe)
  const lineMat = new THREE.MeshBasicMaterial({ color: 0xddddcc });
  const centreLine = new THREE.Mesh(new THREE.BoxGeometry(DECK_W * 0.9, 0.05, 1.2), lineMat);
  centreLine.position.y = 1.25;
  group.add(centreLine);

  // Running lights
  const redMat  = new THREE.MeshBasicMaterial({ color: 0xff2200 });
  const greenMat = new THREE.MeshBasicMaterial({ color: 0x00ff44 });
  const portLight = new THREE.Mesh(new THREE.SphereGeometry(0.6, 6, 4), greenMat);
  portLight.position.set(-DECK_W * 0.5, 1, 0);
  group.add(portLight);
  const stbdLight = new THREE.Mesh(new THREE.SphereGeometry(0.6, 6, 4), redMat);
  stbdLight.position.set(DECK_W * 0.5, 1, 0);
  group.add(stbdLight);

  // Hangar bay light so nearby ships get a coloured rim (matches mothership pattern)
  const bayColor = team === 0 ? 0x66ccff : 0xff8844;
  const light = new THREE.PointLight(bayColor, 1.4, 120);
  light.position.set(0, 4, 0);
  group.add(light);

  return group;
}
