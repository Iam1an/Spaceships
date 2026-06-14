import * as THREE from 'three';
export function createBeams() {
  const group = new THREE.Group();
  group.name = 'Beams';
  const LIFE = 0.18;
  const FACTION_COLOR = {
    self: 0x88ffd6,
    ally: 0x4aa3ff,
    enemy: 0xff5566,
  };
  const RADIUS = 0.5;
  const upAxis = new THREE.Vector3(0, 1, 0);
  const beams = [];
  function fire(origin, end, faction = 'self') {
    const length = origin.distanceTo(end);
    if (length <= 0.001) return;
    const geo = new THREE.CylinderGeometry(RADIUS, RADIUS, length, 8, 1, true);
    const mat = new THREE.MeshBasicMaterial({
      color: FACTION_COLOR[faction] ?? FACTION_COLOR.enemy,
      transparent: true,
      opacity: 0.9,
      blending: THREE.AdditiveBlending,
      depthWrite: false,
      side: THREE.DoubleSide,
    });
    const mesh = new THREE.Mesh(geo, mat);
    mesh.position.copy(origin).add(end).multiplyScalar(0.5);
    const dir = new THREE.Vector3().subVectors(end, origin).normalize();
    mesh.quaternion.setFromUnitVectors(upAxis, dir);
    group.add(mesh);
    beams.push({ mesh, mat, geo, age: 0 });
  }
  function update(dt) {
    for (let i = beams.length - 1; i >= 0; i--) {
      const b = beams[i];
      b.age += dt;
      const t = b.age / LIFE;
      if (t >= 1) {
        group.remove(b.mesh);
        b.geo.dispose();
        b.mat.dispose();
        beams.splice(i, 1);
      } else {
        b.mat.opacity = (1 - t) * 0.9;
      }
    }
  }
  return { group, fire, update };
}
