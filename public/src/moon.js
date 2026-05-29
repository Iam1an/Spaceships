import * as THREE from 'three';
import { mergeVertices } from 'three/addons/utils/BufferGeometryUtils.js';

// A single static obstacle parked at the origin. Big enough to break
// line of sight between the two motherships so neither team can snipe
// across the whole field. Treated as an indestructible sphere everywhere
// it matters (bullets / beams / aim-assist LOS / ship collision).
//
// Geometry: a welded, lightly-displaced icosahedron — reads as a sphere
// with subtle surface variation rather than a chunky asteroid. Surface
// look comes from a dedicated moon photo (already light gray, so we
// can use it as an albedo map without tinting the moon toward dark).
const MOON_TEX = new THREE.TextureLoader().load('sounds/Moon2.jpeg');
MOON_TEX.wrapS = THREE.RepeatWrapping;
MOON_TEX.wrapT = THREE.RepeatWrapping;
MOON_TEX.colorSpace = THREE.SRGBColorSpace;

function pseudoNoise(x, y, z, seed) {
  const s = Math.sin(x * 12.9898 + y * 78.233 + z * 37.719 + seed * 4.7) * 43758.5453;
  return (s - Math.floor(s)) * 2 - 1;
}

export function createMoon({ radius = 80, position = [0, 0, 0] } = {}) {
  // detail=4 gives ~2500 tris — plenty for a single object the player
  // orbits, cheap enough that even iGPUs don't care.
  let geo = new THREE.IcosahedronGeometry(1, 4);
  geo = mergeVertices(geo, 1e-4);
  const pos = geo.attributes.position;
  for (let i = 0; i < pos.count; i++) {
    const x = pos.getX(i), y = pos.getY(i), z = pos.getZ(i);
    // Tiny surface bumps only — the moon should read as a sphere first,
    // craters second. Lobes/large displacement removed entirely.
    const bump = 0.985 + 0.03 * pseudoNoise(x * 3.7, y * 3.7, z * 3.7, 11);
    pos.setXYZ(i, x * bump, y * bump, z * bump);
  }
  geo.computeVertexNormals();

  const mat = new THREE.MeshStandardMaterial({
    map: MOON_TEX,
    color: 0xffffff,        // let the texture set its own brightness
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
