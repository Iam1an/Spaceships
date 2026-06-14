import * as THREE from 'three';
const CLUSTER_COUNT = 26;
const MIN_ALT = 280;
const MAX_ALT = 520;
const DRIFT_SPEED = 0.8;
export function createClouds(scene) {
  const mat = new THREE.MeshStandardMaterial({
    color: 0xffffff,
    transparent: true,
    opacity: 0.72,
    roughness: 1.0,
    metalness: 0.0,
    depthWrite: false,
  });
  const clusters = [];
  const spread = 1700;
  for (let c = 0; c < CLUSTER_COUNT; c++) {
    const cx = (Math.random() * 2 - 1) * spread;
    const cy = MIN_ALT + Math.random() * (MAX_ALT - MIN_ALT);
    const cz = (Math.random() * 2 - 1) * spread;
    const scale = 0.6 + Math.random() * 0.9;
    const driftDir = (Math.random() > 0.5 ? 1 : -1) * (0.4 + Math.random() * 0.6);
    const group = new THREE.Group();
    group.position.set(cx, cy, cz);
    const sphereCount = 6 + Math.floor(Math.random() * 4);
    for (let s = 0; s < sphereCount; s++) {
      const r = (18 + Math.random() * 28) * scale;
      const sx = (Math.random() - 0.5) * 60 * scale;
      const sy = (Math.random() - 0.5) * 14 * scale;
      const sz = (Math.random() - 0.5) * 50 * scale;
      const sphere = new THREE.Mesh(new THREE.SphereGeometry(r, 7, 5), mat.clone());
      sphere.position.set(sx, sy, sz);
      group.add(sphere);
    }
    scene.add(group);
    clusters.push({ group, driftDir });
  }
  function update(dt) {
    for (const { group, driftDir } of clusters) {
      group.position.x += driftDir * DRIFT_SPEED * dt;
      if (group.position.x > spread + 200) group.position.x = -spread - 200;
      if (group.position.x < -spread - 200) group.position.x = spread + 200;
    }
  }
  return { clusters, update };
}