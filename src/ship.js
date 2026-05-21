import * as THREE from 'three';
import { GLTFLoader } from 'three/addons/loaders/GLTFLoader.js';

// Loaded once per URL, then cloned for every player/remote ship. Cloning
// the cached scene + per-mesh materials keeps tint changes and emissive
// hit-flashes from bleeding across ships.
const modelCache = new Map();   // url → THREE.Group
const loadPromises = new Map(); // url → Promise<THREE.Group>

export function loadShipModel(url = 'public/spaceship.glb') {
  if (modelCache.has(url)) return Promise.resolve(modelCache.get(url));
  if (loadPromises.has(url)) return loadPromises.get(url);
  const loader = new GLTFLoader();
  const promise = new Promise((resolve, reject) => {
    loader.load(url, (gltf) => {
      modelCache.set(url, gltf.scene);
      resolve(gltf.scene);
    }, undefined, reject);
  });
  loadPromises.set(url, promise);
  return promise;
}

export function applyColorsToShip(ship, hullColor, accentColor) {
  ship.traverse((o) => {
    if (!o.isMesh || !o.material?.color) return;
    if (isAccentMesh(o)) {
      if (accentColor !== undefined) o.material.color.setHex(accentColor);
    } else {
      if (hullColor !== undefined) o.material.color.setHex(hullColor);
    }
  });
}

// Classify a mesh as hull or accent using the same logic as customization.js.
function isAccentMesh(o) {
  const n = o.name.toLowerCase();
  if (n.includes('cockpit') || n.includes('engine') || n.includes('window') || n.includes('glass')) return true;
  const c = o.material.color;
  return 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b < 0.35;
}

export function createShip({ tint, hullColor, accentColor, modelUrl = 'public/spaceship.glb', doubleSided = false } = {}) {
  const ship = new THREE.Group();
  ship.name = 'Ship';

  const cachedModel = modelCache.get(modelUrl) ?? modelCache.get('public/spaceship.glb');
  if (cachedModel) {
    const model = cachedModel.clone(true);
    model.rotation.y = -Math.PI / 2;
    model.traverse((o) => {
      if (o.isMesh && o.material) {
        o.material = o.material.clone();
        if (doubleSided) o.material.side = THREE.DoubleSide;
        if (!o.material.color) return;
        // hullColor/accentColor take precedence; fall back to legacy tint.
        if (hullColor !== undefined || accentColor !== undefined) {
          if (isAccentMesh(o)) {
            if (accentColor !== undefined) o.material.color.setHex(accentColor);
          } else {
            if (hullColor !== undefined) o.material.color.setHex(hullColor);
          }
        } else if (tint !== undefined) {
          o.material.color.setHex(tint);
        }
      }
    });
    ship.add(model);
    return ship;
  }

  // Fallback primitives if the GLB hasn't loaded yet — keeps the game
  // playable rather than spawning invisible ships.
  const resolvedHull   = hullColor   ?? tint ?? 0x9fb6cc;
  const resolvedAccent = accentColor ?? 0x2a3340;
  const glowColor = hullColor ?? tint ?? 0x66ccff;
  const hullMat = new THREE.MeshStandardMaterial({ color: resolvedHull, metalness: 0.6, roughness: 0.35 });
  const accentMat = new THREE.MeshStandardMaterial({ color: resolvedAccent, metalness: 0.7, roughness: 0.3 });
  const glowMat = new THREE.MeshBasicMaterial({ color: glowColor });

  const body = new THREE.Mesh(new THREE.ConeGeometry(0.8, 3.2, 16), hullMat);
  body.rotation.x = Math.PI / 2;
  ship.add(body);

  const cockpit = new THREE.Mesh(new THREE.SphereGeometry(0.55, 16, 12, 0, Math.PI * 2, 0, Math.PI / 2), accentMat);
  cockpit.position.set(0, 0.35, 0.2);
  ship.add(cockpit);

  const wing = new THREE.Mesh(new THREE.BoxGeometry(2.6, 0.12, 0.9), hullMat);
  wing.position.set(0, -0.05, -0.3);
  ship.add(wing);

  for (const x of [-1.0, 1.0]) {
    const engine = new THREE.Mesh(new THREE.CylinderGeometry(0.18, 0.22, 0.6, 12), accentMat);
    engine.rotation.x = Math.PI / 2;
    engine.position.set(x, -0.05, -0.7);
    ship.add(engine);

    const glow = new THREE.Mesh(new THREE.CircleGeometry(0.16, 16), glowMat);
    glow.position.set(x, -0.05, -1.0);
    glow.rotation.y = Math.PI;
    ship.add(glow);
  }

  return ship;
}
