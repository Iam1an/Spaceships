import * as THREE from 'three';
export const WATER_Y = -60;
export const WATER_MESH_Y = -65;
export function createWater(scene) {
  const geo = new THREE.PlaneGeometry(8000, 8000, 1, 1);
  geo.rotateX(-Math.PI / 2);
  const mat = new THREE.MeshStandardMaterial({
    color: 0x1a6ea8,
    roughness: 0.15,
    metalness: 0.1,
    transparent: true,
    opacity: 0.88,
  });
  const mesh = new THREE.Mesh(geo, mat);
  mesh.position.y = WATER_MESH_Y;
  scene.add(mesh);
  let uvOffset = 0;
  function update(dt) {
    uvOffset += dt * 0.012;
    mat.map && (mat.map.offset.set(uvOffset % 1, uvOffset % 1));
  }
  const foamGeo = new THREE.PlaneGeometry(8000, 8000, 1, 1);
  foamGeo.rotateX(-Math.PI / 2);
  const foamMat = new THREE.MeshBasicMaterial({
    color: 0x55aadd,
    transparent: true,
    opacity: 0.18,
    blending: THREE.AdditiveBlending,
    depthWrite: false,
  });
  const foam = new THREE.Mesh(foamGeo, foamMat);
  foam.position.y = WATER_MESH_Y + 0.5;
  scene.add(foam);
  return { mesh, foam, update };
}