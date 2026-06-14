import * as THREE from 'three';
import { mergeVertices } from 'three/addons/utils/BufferGeometryUtils.js';
const MOON_TEX = new THREE.TextureLoader().load('sounds/Moon2.jpeg');
MOON_TEX.wrapS = THREE.RepeatWrapping;
MOON_TEX.wrapT = THREE.RepeatWrapping;
MOON_TEX.colorSpace = THREE.SRGBColorSpace;
function pseudoNoise(x, y, z, seed) {
  const s = Math.sin(x * 12.9898 + y * 78.233 + z * 37.719 + seed * 4.7) * 43758.5453;
  return (s - Math.floor(s)) * 2 - 1;
}
export function createMoon({ radius = 80, position = [0, 0, 0] } = {}) {
  let geo = new THREE.IcosahedronGeometry(1, 4);
  geo = mergeVertices(geo, 1e-4);
  const pos = geo.attributes.position;
  for (let i = 0; i < pos.count; i++) {
    const x = pos.getX(i), y = pos.getY(i), z = pos.getZ(i);
    const bump = 0.985 + 0.03 * pseudoNoise(x * 3.7, y * 3.7, z * 3.7, 11);
    pos.setXYZ(i, x * bump, y * bump, z * bump);
  }
  geo.computeVertexNormals();
  const mat = new THREE.MeshStandardMaterial({
    map: MOON_TEX,
    color: 0xffffff,
    roughness: 0.95,
    metalness: 0.02,
  });
  const mesh = new THREE.Mesh(geo, mat);
  mesh.scale.setScalar(radius);
  mesh.position.fromArray(position);
  mesh.name = 'Moon';
  const spin = new THREE.Vector3(0.005, 0.012, 0.003);
  function update(dt) {
    mesh.rotation.x += spin.x * dt;
    mesh.rotation.y += spin.y * dt;
    mesh.rotation.z += spin.z * dt;
  }
  return { mesh, pos: mesh.position, radius, update };
}