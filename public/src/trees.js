import * as THREE from 'three';
import { getTerrainHeight, TERRAIN_SIZE } from './terrain.js';

const TREE_COUNT     = 340;
const MIN_HEIGHT     = 8;     // don't place in deep valleys / airfields
const MAX_HEIGHT     = 115;   // don't place above treeline
const AIRFIELD_CLEAR = 320;   // keep trees away from airfield centres

const AIRFIELD_CENTRES = [
  { x: 0, z: -1500 },
  { x: 0, z:  1500 },
];

function nearAirfield(x, z) {
  for (const af of AIRFIELD_CENTRES) {
    const dx = x - af.x, dz = z - af.z;
    if (Math.sqrt(dx * dx + dz * dz) < AIRFIELD_CLEAR) return true;
  }
  return false;
}

export function createTrees(scene) {
  // Two InstancedMesh objects: canopy cones + trunk cylinders
  const canopyGeo = new THREE.ConeGeometry(7, 22, 6);
  const trunkGeo  = new THREE.CylinderGeometry(1.2, 1.6, 10, 6);
  const canopyMat = new THREE.MeshStandardMaterial({ color: 0x2d5a1b, roughness: 0.9 });
  const trunkMat  = new THREE.MeshStandardMaterial({ color: 0x5a3a1a, roughness: 0.95 });

  const canopyIM = new THREE.InstancedMesh(canopyGeo, canopyMat, TREE_COUNT);
  const trunkIM  = new THREE.InstancedMesh(trunkGeo,  trunkMat,  TREE_COUNT);
  canopyIM.castShadow = false;
  trunkIM.castShadow  = false;

  const dummy = new THREE.Object3D();
  let placed = 0;
  let attempts = 0;
  const halfSize = TERRAIN_SIZE / 2 - 50;

  while (placed < TREE_COUNT && attempts < TREE_COUNT * 20) {
    attempts++;
    const x = (Math.random() * 2 - 1) * halfSize;
    const z = (Math.random() * 2 - 1) * halfSize;
    const h = getTerrainHeight(x, z);
    if (h < MIN_HEIGHT || h > MAX_HEIGHT) continue;
    if (nearAirfield(x, z)) continue;

    const scale = 0.7 + Math.random() * 0.8;
    const yaw   = Math.random() * Math.PI * 2;

    // Trunk
    dummy.position.set(x, h + 5 * scale, z);
    dummy.rotation.set(0, yaw, 0);
    dummy.scale.setScalar(scale);
    dummy.updateMatrix();
    trunkIM.setMatrixAt(placed, dummy.matrix);

    // Canopy sits above trunk
    dummy.position.set(x, h + 10 * scale + 11 * scale, z);
    dummy.updateMatrix();
    canopyIM.setMatrixAt(placed, dummy.matrix);

    placed++;
  }

  // If we didn't place all requested trees, shrink instance count
  canopyIM.count = placed;
  trunkIM.count  = placed;
  canopyIM.instanceMatrix.needsUpdate = true;
  trunkIM.instanceMatrix.needsUpdate  = true;

  scene.add(canopyIM);
  scene.add(trunkIM);

  return { canopyIM, trunkIM };
}
