import * as THREE from 'three';

// Procedural starfield skybox. Generates a 6-face cube texture in canvases
// so we don't depend on external image assets. Swap to CubeTextureLoader
// once you have real space textures.
export function createSkybox() {
  const size = 1024;
  const faces = [];
  for (let i = 0; i < 6; i++) {
    faces.push(makeStarFace(size, i));
  }
  const tex = new THREE.CubeTexture(faces);
  tex.needsUpdate = true;
  tex.colorSpace = THREE.SRGBColorSpace;
  return tex;
}

function makeStarFace(size, faceIndex) {
  const c = document.createElement('canvas');
  c.width = c.height = size;
  const ctx = c.getContext('2d');

  // Deep-space gradient with a faint nebula tint that varies per face
  // so the cube doesn't look uniformly flat.
  const tints = [
    [10, 12, 30],
    [8, 6, 24],
    [14, 10, 28],
    [6, 8, 22],
    [12, 14, 32],
    [8, 10, 26],
  ];
  const [r, g, b] = tints[faceIndex];
  const grad = ctx.createRadialGradient(size / 2, size / 2, size * 0.1, size / 2, size / 2, size * 0.7);
  grad.addColorStop(0, `rgb(${r + 6},${g + 6},${b + 10})`);
  grad.addColorStop(1, `rgb(${r},${g},${b})`);
  ctx.fillStyle = grad;
  ctx.fillRect(0, 0, size, size);

  // Soft nebula blobs.
  for (let i = 0; i < 4; i++) {
    const x = Math.random() * size;
    const y = Math.random() * size;
    const rad = 80 + Math.random() * 220;
    const ng = ctx.createRadialGradient(x, y, 0, x, y, rad);
    const hue = 200 + Math.random() * 80;
    ng.addColorStop(0, `hsla(${hue}, 60%, 40%, 0.18)`);
    ng.addColorStop(1, 'hsla(0, 0%, 0%, 0)');
    ctx.fillStyle = ng;
    ctx.fillRect(0, 0, size, size);
  }

  // Stars.
  const starCount = 500;
  for (let i = 0; i < starCount; i++) {
    const x = Math.random() * size;
    const y = Math.random() * size;
    const a = Math.pow(Math.random(), 2);
    const sz = a * 1.8 + 0.2;
    ctx.fillStyle = `rgba(255, 255, 255, ${0.3 + a * 0.7})`;
    ctx.beginPath();
    ctx.arc(x, y, sz, 0, Math.PI * 2);
    ctx.fill();
  }
  // A few brighter colored stars.
  for (let i = 0; i < 12; i++) {
    const x = Math.random() * size;
    const y = Math.random() * size;
    const hue = Math.random() * 360;
    ctx.fillStyle = `hsla(${hue}, 80%, 80%, 0.9)`;
    ctx.beginPath();
    ctx.arc(x, y, 1.5 + Math.random() * 1.5, 0, Math.PI * 2);
    ctx.fill();
  }
  return c;
}
