import * as THREE from 'three';
import { loadShipModel, createShip } from './ship.js';
const SHIP_COLOR_KEY = 'spaceships:shipColor';
const ACCENT_COLOR_KEY = 'spaceships:shipAccentColor';
const TRAIL_COLOR_KEY = 'spaceships:trailColor';
const TRAIL_SHAPE_KEY = 'spaceships:trailShape';
export function getSavedShipColor() {
  return localStorage.getItem(SHIP_COLOR_KEY) || '#9fb6cc';
}
export function getSavedAccentColor() {
  return localStorage.getItem(ACCENT_COLOR_KEY) || '#2a3340';
}
export function getSavedTrailColor() {
  return localStorage.getItem(TRAIL_COLOR_KEY) || '#66ddff';
}
export function getSavedTrailShape() {
  return localStorage.getItem(TRAIL_SHAPE_KEY) || 'circle';
}
export function hslToHex(h, s, l) {
  const a = s * Math.min(l, 1 - l);
  const f = n => {
    const k = (n + h / 30) % 12;
    const c = l - a * Math.max(Math.min(k - 3, 9 - k, 1), -1);
    return Math.round(255 * Math.max(0, Math.min(1, c))).toString(16).padStart(2, '0');
  };
  return `#${f(0)}${f(8)}${f(4)}`;
}
export function hexToHsl(hex) {
  const r = parseInt(hex.slice(1, 3), 16) / 255;
  const g = parseInt(hex.slice(3, 5), 16) / 255;
  const b = parseInt(hex.slice(5, 7), 16) / 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  let h = 0, s = 0;
  const l = (max + min) / 2;
  if (max !== min) {
    const d = max - min;
    s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    if (max === r) h = ((g - b) / d + (g < b ? 6 : 0)) * 60;
    else if (max === g) h = ((b - r) / d + 2) * 60;
    else h = ((r - g) / d + 4) * 60;
  }
  return { h, s, l };
}
export function initCustomizationScene(canvasEl) {
  const renderer = new THREE.WebGLRenderer({ canvas: canvasEl, antialias: true });
  renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
  renderer.setClearColor(0x050b18, 1);
  const scene = new THREE.Scene();
  scene.fog = new THREE.FogExp2(0x050b18, 0.016);
  scene.add(new THREE.AmbientLight(0xffffff, 0.45));
  const keyLight = new THREE.DirectionalLight(0xaaddff, 2.0);
  keyLight.position.set(3, 5, 6);
  scene.add(keyLight);
  const fillLight = new THREE.DirectionalLight(0xffd97a, 0.65);
  fillLight.position.set(-5, 1, -3);
  scene.add(fillLight);
  const camera = new THREE.PerspectiveCamera(38, 1, 0.1, 200);
  camera.position.set(0, 2.5, 14);
  camera.lookAt(0, 0, 0);
  const pedestalMat = new THREE.MeshStandardMaterial({ color: 0x0d1a2e, metalness: 0.9, roughness: 0.2 });
  const plinth = new THREE.Mesh(new THREE.CylinderGeometry(1.4, 1.7, 0.45, 48), pedestalMat);
  plinth.position.y = -2.1;
  scene.add(plinth);
  const ringMat = new THREE.MeshBasicMaterial({ color: 0x66ddff });
  const ring = new THREE.Mesh(new THREE.TorusGeometry(1.4, 0.055, 12, 64), ringMat);
  ring.position.y = -1.88;
  scene.add(ring);
  const discMat = new THREE.MeshBasicMaterial({ color: 0x0a1428, transparent: true, opacity: 0.9 });
  const disc = new THREE.Mesh(new THREE.CircleGeometry(4, 48), discMat);
  disc.rotation.x = -Math.PI / 2;
  disc.position.y = -2.35;
  scene.add(disc);
  const spotMat = new THREE.MeshBasicMaterial({
    color: 0x1a3060, transparent: true, opacity: 0.18, side: THREE.BackSide,
  });
  const spotGeo = new THREE.ConeGeometry(2.5, 8, 32, 1, true);
  const spot = new THREE.Mesh(spotGeo, spotMat);
  spot.position.y = 5;
  scene.add(spot);
  const shipGroup = new THREE.Group();
  shipGroup.position.y = -0.6;
  scene.add(shipGroup);
  const hullMeshes = [];
  const accentMeshes = [];
  const currentColor = new THREE.Color(getSavedShipColor());
  const currentAccentColor = new THREE.Color(getSavedAccentColor());
  let running = false;
  let raf = null;
  let angle = 0;
  function resize() {
    const w = canvasEl.clientWidth;
    const h = canvasEl.clientHeight;
    if (w > 0 && h > 0 && (renderer.domElement.width !== w || renderer.domElement.height !== h)) {
      renderer.setSize(w, h, false);
      camera.aspect = w / h;
      camera.updateProjectionMatrix();
    }
  }
  function tick() {
    if (!running) return;
    raf = requestAnimationFrame(tick);
    resize();
    angle += 0.007;
    shipGroup.rotation.y = angle;
    ringMat.color.copy(currentColor).multiplyScalar(0.75 + 0.25 * Math.sin(Date.now() * 0.0018));
    renderer.render(scene, camera);
  }
  function setColor(hex) {
    currentColor.set(hex);
    const n = parseInt(hex.replace('#', ''), 16);
    for (const m of hullMeshes) {
      if (m.material?.color) m.material.color.setHex(n);
    }
  }
  function setAccentColor(hex) {
    currentAccentColor.set(hex);
    const n = parseInt(hex.replace('#', ''), 16);
    for (const m of accentMeshes) {
      if (m.material?.color) m.material.color.setHex(n);
    }
  }
  function setZoom(z) {
    camera.position.z = z;
    camera.updateProjectionMatrix();
  }
  function resume() {
    if (!running) { running = true; tick(); }
  }
  function pause() {
    running = false;
    if (raf) { cancelAnimationFrame(raf); raf = null; }
  }
  function destroy() {
    pause();
    renderer.dispose();
  }
  running = true;
  tick();
  loadShipModel()
    .catch(() => { })
    .finally(() => {
      const ship = createShip();
      ship.scale.setScalar(2.5);
      shipGroup.add(ship);
      scene.updateMatrixWorld(true);
      const box = new THREE.Box3().setFromObject(ship);
      const center = new THREE.Vector3();
      box.getCenter(center);
      ship.position.sub(center);
      ship.traverse(o => {
        if (!o.isMesh || !o.material?.color) return;
        const n = o.name.toLowerCase();
        const byName = n.includes('cockpit') || n.includes('engine') ||
          n.includes('window') || n.includes('glass');
        const c = o.material.color;
        const byLum = 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b < 0.35;
        if (byName || byLum) accentMeshes.push(o);
        else hullMeshes.push(o);
      });
      setColor(getSavedShipColor());
      setAccentColor(getSavedAccentColor());
    });
  return { setColor, setAccentColor, setZoom, resume, pause, destroy };
}
