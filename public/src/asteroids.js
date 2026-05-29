import * as THREE from 'three';
import { mergeVertices } from 'three/addons/utils/BufferGeometryUtils.js';

// Shared rocky texture for all asteroid materials. Tileable so the
// icosahedron's default-but-quirky UVs still read as continuous rock.
// Loaded once at module scope (TextureLoader caches automatically) and
// applied to every cloned material.
const ASTEROID_TEX = new THREE.TextureLoader().load('sounds/asteroid.jpg');
ASTEROID_TEX.wrapS = THREE.RepeatWrapping;
ASTEROID_TEX.wrapT = THREE.RepeatWrapping;
ASTEROID_TEX.repeat.set(1.5, 1.5);
ASTEROID_TEX.anisotropy = 4;
ASTEROID_TEX.colorSpace = THREE.SRGBColorSpace;

// Spawns a field of asteroids inside a sphere of `radius`. Each asteroid
// stores a collision radius for the simple sphere/sphere check in main.js.
//
// IcosahedronGeometry is returned non-indexed (each face owns its three
// verts), so displacing positions per-vertex tears the mesh open at face
// borders. We weld with mergeVertices() first so each shared corner is
// displaced once — the polyhedron stays watertight.
// Three tiers of asteroid: small/medium/big with different size ranges and
// HP. Spawn weights bias the field toward smaller, easier rocks so the
// occasional big one feels distinct.
const TIERS = [
  { name: 'small',  minSize: 5,  maxSize: 7,  hp: 5,  weight: 0.45 },
  { name: 'medium', minSize: 9,  maxSize: 15, hp: 10, weight: 0.30 },
  { name: 'big',    minSize: 18, maxSize: 30, hp: 30, weight: 0.18 },
  { name: 'huge',   minSize: 38, maxSize: 55, hp: 50, weight: 0.07 },
];

function pickTier() {
  const r = Math.random();
  let acc = 0;
  for (const t of TIERS) {
    acc += t.weight;
    if (r < acc) return t;
  }
  return TIERS[TIERS.length - 1];
}

// Builds the geometry variants (welded, displaced icosahedrons) used as
// shape pool for both the data-driven and random paths.
function buildVariants() {
  const variants = [];
  for (let v = 0; v < 6; v++) {
    let g = new THREE.IcosahedronGeometry(1, 2);
    g = mergeVertices(g, 1e-4);
    const pos = g.attributes.position;
    for (let i = 0; i < pos.count; i++) {
      const x = pos.getX(i), y = pos.getY(i), z = pos.getZ(i);
      const lobe = 0.78 + 0.34 * pseudoNoise(x * 1.3, y * 1.3, z * 1.3, v);
      const bump = 0.94 + 0.12 * pseudoNoise(x * 4.1, y * 4.1, z * 4.1, v + 7);
      const n = lobe * bump;
      pos.setXYZ(i, x * n, y * n, z * n);
    }
    g.computeVertexNormals();
    variants.push(g);
  }
  return variants;
}

const BASE_MAT = new THREE.MeshStandardMaterial({
  map: ASTEROID_TEX,
  color: 0xffffff,
  roughness: 0.95,
  metalness: 0.05,
  flatShading: true,
});

function spinScaleFor(tier) {
  return tier === 'huge' ? 0.10 : tier === 'big' ? 0.18 : tier === 'medium' ? 0.32 : 0.5;
}

// Build the live field from server-authoritative data. Each entry has an
// id used by `setHp`/`destroy` when server messages arrive.
export function createAsteroidFieldFromData(data = []) {
  const group = new THREE.Group();
  group.name = 'Asteroids';
  const list = [];
  const variants = buildVariants();

  for (const d of data) {
    const geo = variants[(d.variant ?? 0) % variants.length];
    const mesh = new THREE.Mesh(geo, BASE_MAT.clone());
    mesh.scale.setScalar(d.size);
    mesh.position.set(d.pos[0], d.pos[1], d.pos[2]);
    mesh.rotation.set(d.rot[0], d.rot[1], d.rot[2]);
    const ss = spinScaleFor(d.tier);
    const spin = new THREE.Vector3(d.spin[0] * ss, d.spin[1] * ss, d.spin[2] * ss);
    group.add(mesh);
    list.push({ id: d.id, mesh, radius: d.size * 0.95, spin, hp: d.hp, hitFlash: 0, tier: d.tier });
  }

  function update(dt) {
    for (const a of list) {
      a.mesh.rotation.x += a.spin.x * dt;
      a.mesh.rotation.y += a.spin.y * dt;
      a.mesh.rotation.z += a.spin.z * dt;
      if (a.hitFlash > 0) {
        a.hitFlash = Math.max(0, a.hitFlash - dt * 4);
        const f = a.hitFlash;
        a.mesh.material.emissive?.setRGB(f, f * 0.6, f * 0.3);
      }
    }
  }

  function destroy(id) {
    const idx = list.findIndex((a) => a.id === id);
    if (idx < 0) return null;
    const a = list[idx];
    group.remove(a.mesh);
    a.mesh.material.dispose();
    list.splice(idx, 1);
    return a;
  }

  function setHp(id, hp) {
    const a = list.find((x) => x.id === id);
    if (a) {
      a.hp = hp;
      a.hitFlash = 1;
    }
  }

  return { group, list, update, destroy, setHp };
}

export function createAsteroidField({ count = 60, radius = 400, avoid = [] } = {}) {
  const group = new THREE.Group();
  group.name = 'Asteroids';

  const list = [];

  function clipsAvoidance(x, y, z, asteroidRadius) {
    for (const v of avoid) {
      const margin = asteroidRadius + 6;
      if (
        Math.abs(x - v.pos.x) < v.halfSize.x + margin &&
        Math.abs(y - v.pos.y) < v.halfSize.y + margin &&
        Math.abs(z - v.pos.z) < v.halfSize.z + margin
      ) return true;
    }
    return false;
  }

  // Pre-build a few welded, displaced variants. Cloning a variant for each
  // asteroid would be cheaper, but we want shape variety and rocks are few.
  const variants = [];
  for (let v = 0; v < 6; v++) {
    let g = new THREE.IcosahedronGeometry(1, 2);
    g = mergeVertices(g, 1e-4);
    const pos = g.attributes.position;
    for (let i = 0; i < pos.count; i++) {
      const x = pos.getX(i), y = pos.getY(i), z = pos.getZ(i);
      // Layered noise: large lobes + small surface bumps.
      const lobe = 0.78 + 0.34 * pseudoNoise(x * 1.3, y * 1.3, z * 1.3, v);
      const bump = 0.94 + 0.12 * pseudoNoise(x * 4.1, y * 4.1, z * 4.1, v + 7);
      const n = lobe * bump;
      pos.setXYZ(i, x * n, y * n, z * n);
    }
    g.computeVertexNormals();
    variants.push(g);
  }

  const baseMat = new THREE.MeshStandardMaterial({
    map: ASTEROID_TEX,
    color: 0xffffff,
    roughness: 0.95,
    metalness: 0.05,
    flatShading: true,
  });

  for (let i = 0; i < count; i++) {
    const tier = pickTier();
    const geo = variants[Math.floor(Math.random() * variants.length)];
    // Clone the material per asteroid so hit-flash emissive doesn't bleed
    // across the whole field.
    const mesh = new THREE.Mesh(geo, baseMat.clone());

    const size = tier.minSize + Math.random() * (tier.maxSize - tier.minSize);
    mesh.scale.setScalar(size);

    // Random point inside sphere, retrying if it clips a mothership AABB.
    let placed = false;
    for (let attempt = 0; attempt < 10 && !placed; attempt++) {
      const dir = new THREE.Vector3(
        Math.random() * 2 - 1,
        (Math.random() * 2 - 1) * 0.4,
        Math.random() * 2 - 1
      ).normalize();
      const minDist = 30 + size;
      const dist = minDist + Math.random() * (radius - minDist);
      const cand = dir.multiplyScalar(dist);
      if (!clipsAvoidance(cand.x, cand.y, cand.z, size)) {
        mesh.position.copy(cand);
        placed = true;
      }
    }
    if (!placed) {
      // Last-ditch fallback: nudge to a random direction without bound check.
      mesh.position.set((Math.random() - 0.5) * radius, 0, (Math.random() - 0.5) * radius);
    }
    mesh.rotation.set(Math.random() * Math.PI, Math.random() * Math.PI, Math.random() * Math.PI);

    // Smaller rocks tumble faster than big ones for a sense of mass.
    const spinScale =
      tier.name === 'huge'   ? 0.10 :
      tier.name === 'big'    ? 0.18 :
      tier.name === 'medium' ? 0.32 :
                               0.5;
    const spin = new THREE.Vector3(
      (Math.random() - 0.5) * spinScale,
      (Math.random() - 0.5) * spinScale,
      (Math.random() - 0.5) * spinScale
    );

    group.add(mesh);
    list.push({ mesh, radius: size * 0.95, spin, hp: tier.hp, hitFlash: 0, tier: tier.name });
  }

  function update(dt) {
    for (const a of list) {
      a.mesh.rotation.x += a.spin.x * dt;
      a.mesh.rotation.y += a.spin.y * dt;
      a.mesh.rotation.z += a.spin.z * dt;
      if (a.hitFlash > 0) {
        a.hitFlash = Math.max(0, a.hitFlash - dt * 4);
        const f = a.hitFlash;
        a.mesh.material.emissive?.setRGB(f, f * 0.6, f * 0.3);
      }
    }
  }

  return { group, list, update };
}

// Cheap deterministic-ish 3D noise. Good enough for shape variation; not
// trying to compete with simplex.
function pseudoNoise(x, y, z, seed) {
  const s = Math.sin(x * 12.9898 + y * 78.233 + z * 37.719 + seed * 4.7) * 43758.5453;
  return (s - Math.floor(s)) * 2 - 1;
}
