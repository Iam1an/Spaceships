import * as THREE from 'three';

// ── Terrain constants ─────────────────────────────────────────────────────────
export const TERRAIN_SIZE   = 3600;   // world units, square
export const TERRAIN_SEGS   = 96;     // segments per side
export const TERRAIN_KILL_CLEARANCE = 5; // ship dies if y < groundY + this

// Airfield flat zones — one per team
const AIRFIELDS = [
  { cx: 0, cz: -1500, hw: 280, hd: 190 },  // team 0
  { cx: 0, cz:  1500, hw: 280, hd: 190 },  // team 1
];

function smoothstep(t) {
  t = Math.min(1, Math.max(0, t));
  return t * t * (3 - 2 * t);
}

function airfieldBlend(wx, wz) {
  let maxBlend = 0;
  for (const af of AIRFIELDS) {
    const tx = Math.max(0, 1 - Math.abs(wx - af.cx) / af.hw);
    const tz = Math.max(0, 1 - Math.abs(wz - af.cz) / af.hd);
    maxBlend = Math.max(maxBlend, smoothstep(Math.min(tx, tz) * 2.5));
  }
  return maxBlend;
}

// Deterministic multi-octave terrain noise (no external library)
function rawHeight(wx, wz) {
  let h = 0;
  // Broad mountain ranges — two perpendicular ridge systems
  h += (Math.sin(wx * 0.0010 + 1.1) * 0.5 + 0.5)
     * (Math.sin(wz * 0.0012 + 2.3) * 0.5 + 0.5) * 390;
  h += (Math.sin(wx * 0.0014 + 3.4) * 0.5 + 0.5)
     * (Math.sin(wz * 0.0009 + 0.7) * 0.5 + 0.5) * 255;
  // Medium hills / canyon walls
  h += Math.max(0, Math.sin(wx * 0.0029 + 0.9) * Math.cos(wz * 0.0026 + 1.6)) * 165;
  h += Math.max(0, Math.sin(wx * 0.0039 - wz * 0.0023 + 2.2)) * 105;
  // Fine ridges
  h += Math.sin(wx * 0.0072 + 1.7) * Math.cos(wz * 0.0065 + 0.4) * 42;
  h += Math.sin(wx * 0.0117 - wz * 0.0091 + 3.1) * 24;
  // Micro detail
  h += Math.sin(wx * 0.0208 + wz * 0.0182 + 0.8) * 12;
  return Math.max(0, h);
}

export function getTerrainHeight(wx, wz) {
  // Clamp to terrain extent
  if (Math.abs(wx) > TERRAIN_SIZE / 2 || Math.abs(wz) > TERRAIN_SIZE / 2) return 0;
  const raw  = rawHeight(wx, wz);
  const flat = airfieldBlend(wx, wz);
  return raw * (1 - flat);
}

// ── Geometry & mesh ───────────────────────────────────────────────────────────
export function createTerrain() {
  const W = TERRAIN_SIZE, S = TERRAIN_SEGS;
  const geo = new THREE.PlaneGeometry(W, W, S, S);
  const pos = geo.attributes.position;

  // After PlaneGeometry (XY plane), vertex layout:
  //   original x ∈ [-W/2, W/2]  →  world x (unchanged after rotateX)
  //   original y ∈ [-W/2, W/2]  →  world z (sign flipped after rotateX(-PI/2))
  // We set position.z before rotation; after rotateX it becomes world Y.
  const count = pos.count;
  const colors = new Float32Array(count * 3);

  for (let i = 0; i < count; i++) {
    const gx = pos.getX(i);   // world X
    const gy = pos.getY(i);   // world -Z (before rotation)
    const wx = gx;
    const wz = -gy;            // world Z after rotation
    const h  = getTerrainHeight(wx, wz);
    pos.setZ(i, h);            // will become world Y after rotateX

    // Vertex colour by altitude
    let r, g, b;
    if (h < 10) {
      // Flat dirt/grass around airfields and valley floors
      r = 0.36; g = 0.50; b = 0.22;
    } else if (h < 120) {
      // Mid green grass
      r = 0.28; g = 0.48; b = 0.18;
    } else if (h < 270) {
      // Rocky slope — lerp grass→rock
      const t = (h - 120) / 150;
      r = 0.28 + t * 0.26; g = 0.48 - t * 0.18; b = 0.18 + t * 0.12;
    } else if (h < 420) {
      // Grey rock
      r = 0.54; g = 0.48; b = 0.40;
    } else {
      // Snow caps
      const t = Math.min(1, (h - 420) / 90);
      r = 0.54 + t * 0.42; g = 0.48 + t * 0.46; b = 0.40 + t * 0.55;
    }
    colors[i * 3]     = r;
    colors[i * 3 + 1] = g;
    colors[i * 3 + 2] = b;
  }

  geo.setAttribute('color', new THREE.BufferAttribute(colors, 3));
  pos.needsUpdate = true;
  geo.rotateX(-Math.PI / 2);
  geo.computeVertexNormals();

  const mat = new THREE.MeshStandardMaterial({
    vertexColors: true,
    roughness: 0.92,
    metalness: 0.0,
    polygonOffset: true,
    polygonOffsetFactor: 2,
    polygonOffsetUnits: 2,
  });

  const mesh = new THREE.Mesh(geo, mat);
  mesh.name = 'Terrain';
  return mesh;
}
