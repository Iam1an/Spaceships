import * as THREE from 'three';
const MAX_PARTICLES = 250;
const geoCache = {};
function buildGeometry(shape) {
  switch (shape) {
    case 'square':
      return new THREE.PlaneGeometry(1, 1);
    case 'triangle': {
      const s = new THREE.Shape();
      const r = 0.6;
      s.moveTo(0, r);
      s.lineTo(-r * Math.sin((Math.PI * 2) / 3), r * Math.cos((Math.PI * 2) / 3) * -1);
      s.lineTo(r * Math.sin((Math.PI * 2) / 3), r * Math.cos((Math.PI * 2) / 3) * -1);
      s.closePath();
      return new THREE.ShapeGeometry(s);
    }
    case 'star': {
      const s = new THREE.Shape();
      const outer = 0.5, inner = 0.21, pts = 5;
      for (let i = 0; i < pts * 2; i++) {
        const r = i % 2 === 0 ? outer : inner;
        const a = (i * Math.PI) / pts - Math.PI / 2;
        if (i === 0) s.moveTo(Math.cos(a) * r, Math.sin(a) * r);
        else s.lineTo(Math.cos(a) * r, Math.sin(a) * r);
      }
      s.closePath();
      return new THREE.ShapeGeometry(s);
    }
    case 'david': {
      const r = 0.5;
      const h = r * Math.sqrt(3) / 2;
      const geo = new THREE.BufferGeometry();
      geo.setAttribute('position', new THREE.Float32BufferAttribute([
        0, r, 0,
        -h, -r / 2, 0,
        h, -r / 2, 0,
        0, -r, 0,
        h, r / 2, 0,
        -h, r / 2, 0,
      ], 3));
      geo.setIndex([0, 1, 2, 3, 4, 5]);
      geo.computeVertexNormals();
      return geo;
    }
    default:
      return new THREE.SphereGeometry(0.5, 8, 6);
  }
}
function getGeometry(shape) {
  if (!geoCache[shape]) geoCache[shape] = buildGeometry(shape);
  return geoCache[shape];
}
const FLAT_SHAPES = new Set(['square', 'triangle', 'star', 'david']);
export function createTrails() {
  const group = new THREE.Group();
  group.name = 'Trails';
  const list = [];
  function emit(pos, scale = 1, color = 0x66ddff, life = 0.55, shape = 'circle') {
    if (list.length >= MAX_PARTICLES) {
      const oldest = list.shift();
      group.remove(oldest.mesh);
      oldest.mesh.material.dispose();
    }
    const geo = getGeometry(shape);
    const mat = new THREE.MeshBasicMaterial({
      color,
      transparent: true,
      opacity: 0.85,
      blending: THREE.AdditiveBlending,
      depthWrite: false,
      side: FLAT_SHAPES.has(shape) ? THREE.DoubleSide : THREE.FrontSide,
    });
    const mesh = new THREE.Mesh(geo, mat);
    mesh.position.copy(pos);
    mesh.scale.setScalar(scale);
    group.add(mesh);
    list.push({ mesh, age: 0, life, scale, flat: FLAT_SHAPES.has(shape) });
  }
  function update(dt, camera) {
    for (let i = list.length - 1; i >= 0; i--) {
      const p = list[i];
      p.age += dt;
      const t = p.age / p.life;
      p.mesh.scale.setScalar(p.scale * (1 - t * 0.45));
      p.mesh.material.opacity = (1 - t) * 0.85;
      if (p.flat && camera) p.mesh.quaternion.copy(camera.quaternion);
      if (t >= 1) {
        group.remove(p.mesh);
        p.mesh.material.dispose();
        list.splice(i, 1);
      }
    }
  }
  return { group, emit, update };
}