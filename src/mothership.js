import * as THREE from 'three';

// Big slab-shaped capital ship with a glowing hangar bay on the +Z face.
// Players spawn just in front of the hangar door (at z = HANGAR_OFFSET in
// mothership-local space). Position the whole group anywhere in world.
export const HANGAR_OFFSET = 40; // distance ahead of the front face to drop a ship

export function createMothership() {
  const group = new THREE.Group();
  group.name = 'Mothership';

  const hullMat = new THREE.MeshStandardMaterial({
    color: 0x4a5366, metalness: 0.55, roughness: 0.5,
  });
  const accentMat = new THREE.MeshStandardMaterial({
    color: 0x222a36, metalness: 0.45, roughness: 0.7,
  });

  const W = 90, H = 36, L = 70;
  const hull = new THREE.Mesh(new THREE.BoxGeometry(W, H, L), hullMat);
  group.add(hull);

  // Trim stripes — slightly wider than the hull so they read as panels.
  for (const y of [-10, 10]) {
    const stripe = new THREE.Mesh(
      new THREE.BoxGeometry(W * 1.005, 1.6, L * 1.005),
      accentMat,
    );
    stripe.position.y = y;
    group.add(stripe);
  }

  // Back-facing engines.
  const glowMat = new THREE.MeshBasicMaterial({ color: 0xff7733 });
  for (const x of [-22, 0, 22]) {
    const ring = new THREE.Mesh(new THREE.CylinderGeometry(3.5, 3.5, 2, 16), accentMat);
    ring.rotation.x = Math.PI / 2;
    ring.position.set(x, -2, -L / 2 - 0.5);
    group.add(ring);
    const glow = new THREE.Mesh(new THREE.CircleGeometry(3.2, 16), glowMat);
    glow.position.set(x, -2, -L / 2 - 1.6);
    glow.rotation.y = Math.PI;
    group.add(glow);
  }

  // Hangar bay on the front (+Z) face. Frame ring around a flat glowing
  // panel, plus an inset that hints at depth.
  const doorW = 32, doorH = 18;
  const frame = new THREE.Mesh(
    new THREE.BoxGeometry(doorW + 4, doorH + 4, 1.2),
    accentMat,
  );
  frame.position.z = L / 2 + 0.1;
  group.add(frame);

  const inset = new THREE.Mesh(
    new THREE.BoxGeometry(doorW, doorH, 6),
    new THREE.MeshStandardMaterial({ color: 0x0a1220, roughness: 0.9 }),
  );
  inset.position.z = L / 2 - 2;
  group.add(inset);

  const door = new THREE.Mesh(
    new THREE.PlaneGeometry(doorW - 1, doorH - 1),
    new THREE.MeshBasicMaterial({
      color: 0x66ccff,
      transparent: true,
      opacity: 0.65,
      blending: THREE.AdditiveBlending,
      depthWrite: false,
    }),
  );
  door.position.z = L / 2 + 0.55;
  group.add(door);

  // Soft point light at the hangar mouth so nearby ships get rim-lit cyan.
  const light = new THREE.PointLight(0x66ccff, 1.8, 100);
  light.position.set(0, 0, L / 2 + 6);
  group.add(light);

  // Tiny running lights along the trim stripes for liveliness.
  const runMat = new THREE.MeshBasicMaterial({ color: 0xffd97a });
  for (let i = -3; i <= 3; i++) {
    if (i === 0) continue;
    const a = new THREE.Mesh(new THREE.SphereGeometry(0.4, 6, 4), runMat);
    a.position.set((i / 3) * (W / 2 - 4), 10, L / 2 + 0.3);
    group.add(a);
    const b = a.clone();
    b.position.y = -10;
    group.add(b);
  }

  return group;
}
