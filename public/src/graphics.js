// Ultra Graphics — an opt-in, reload-to-apply renderer overhaul.
//
// Off by default. When enabled it swaps the plain forward render for a full
// HDR pipeline: image-based lighting from a procedural nebula cubemap, ACES
// tone mapping, bloom, and a single combined grade/CA/vignette/grain pass.
// Everything here is inert unless ULTRA is true, so the default path keeps
// the exact renderer it had before.
import * as THREE from 'three';
import { EffectComposer } from 'three/addons/postprocessing/EffectComposer.js';
import { RenderPass } from 'three/addons/postprocessing/RenderPass.js';
import { UnrealBloomPass } from 'three/addons/postprocessing/UnrealBloomPass.js';
import { ShaderPass } from 'three/addons/postprocessing/ShaderPass.js';

const ULTRA_KEY = 'spaceships:ultraGraphics';

// Read once at module load. Changing the setting mid-session does nothing until
// reload, which is exactly what the settings hint promises.
export const ULTRA = (() => {
  try { return localStorage.getItem(ULTRA_KEY) === '1'; } catch { return false; }
})();

// ── Quality budget ──────────────────────────────────────────────────────────
//
// The composer keeps two ping-pong HalfFloat targets, and a multisampled one
// costs roughly (8 + samples * 12) bytes per pixel. At device pixel ratio 2 on
// a 1080p window that is over half a gigabyte of render target, which an
// integrated GPU cannot allocate — it drops the WebGL context and takes the
// tab with it. So pick the pixel count and sample count from a memory budget
// instead of always asking for the maximum.
//
// This is deliberately a *budget*, not a benchmark: capable machines still get
// the full-resolution buffer, and only the sample count moves, which is
// invisible at high pixel ratios because the extra pixels already antialias.

function targetBytesPerPixel(samples) {
  // Two targets; each carries a resolve texture plus, when multisampled, a
  // colour and depth buffer per sample.
  return 2 * (8 + (samples > 1 ? samples * 12 : 0));
}

// Backing-store pixels the composer is willing to allocate. Without
// multisampling the two HalfFloat targets cost ~16 bytes per pixel, so a 4K
// backing store is about 133 MB — affordable, and high enough that ordinary
// laptops keep their full native pixel ratio rather than being softened.
// Denser panels than this get scaled down, where the loss is imperceptible.
const MAX_BACKING_PIXELS = 8.4e6;

// Absolute ceiling on the composer's own targets. Integrated GPUs share system
// memory and start dropping the context well before a discrete card would.
const MAX_TARGET_BYTES = 160 * 1048576;

let _quality = null;

function getQuality() {
  if (_quality) return _quality;

  const cssPixels = window.innerWidth * window.innerHeight;
  const requested = Math.min(window.devicePixelRatio || 1, 2);

  // Clamp total backing-store size rather than trusting the pixel ratio. A 4K
  // panel at ratio 2 would otherwise ask for 32M pixels, which is half a
  // gigabyte of target before a single sample is added.
  let dpr = requested;
  if (cssPixels * dpr * dpr > MAX_BACKING_PIXELS) {
    dpr = Math.max(1, Math.sqrt(MAX_BACKING_PIXELS / cssPixels));
  }

  // Multisampling is mostly redundant once the backing store is denser than
  // the display: rendering at ratio 2 and letting the compositor downscale is
  // supersampling, which resolves edges at least as well as 2x MSAA. So only
  // pay for samples when the pixel ratio is too low to carry the load itself,
  // and stop at 2 — a low-density screen is the likeliest sign of a weak GPU,
  // which is the last machine that should be handed the heaviest buffer.
  let samples = dpr >= 1.75 ? 0 : 2;

  // Hard ceiling as a backstop for shapes the rules above do not anticipate,
  // such as a large low-density panel. Give up samples first, then pixels.
  const est = (d, s) => cssPixels * d * d * targetBytesPerPixel(s);
  if (est(dpr, samples) > MAX_TARGET_BYTES) {
    samples = 0;
    while (est(dpr, samples) > MAX_TARGET_BYTES && dpr > 1) {
      dpr = Math.max(1, dpr - 0.25);
    }
  }

  _quality = {
    dpr,
    samples,
    bloomScale: 0.5,
  };
  return _quality;
}

// ── Renderer ────────────────────────────────────────────────────────────────

export function configureRenderer(renderer) {
  if (!ULTRA) return renderer;
  // Tone mapping is done by hand in the final pass so it cannot be applied
  // twice; leave the renderer's own operator off.
  renderer.toneMapping = THREE.NoToneMapping;
  renderer.outputColorSpace = THREE.SRGBColorSpace;
  renderer.shadowMap.enabled = true;
  renderer.shadowMap.type = THREE.PCFSoftShadowMap;
  renderer.setPixelRatio(getQuality().dpr);
  return renderer;
}

export function rendererParams(base = {}) {
  if (!ULTRA) return base;
  // MSAA comes from the composer's multisampled target instead.
  return { ...base, antialias: false, powerPreference: 'high-performance', stencil: false };
}

// If the GPU still cannot hold the buffers, the driver drops the context and
// the canvas goes black with no explanation. Catch it, stop the default
// "reload the page into the same crash" cycle, and tell the player how to get
// back to a build their machine can run.
export function installContextLossGuard(renderer, onLost) {
  const canvas = renderer.domElement;
  canvas.addEventListener('webglcontextlost', (e) => {
    // Preventing the default keeps the context restorable rather than dead.
    e.preventDefault();
    console.error('[graphics] WebGL context lost.'
      + (ULTRA ? ' Ultra Graphics is likely too heavy for this GPU.' : ''));
    if (ULTRA) {
      try { localStorage.setItem(ULTRA_KEY, '0'); } catch { /* private mode */ }
    }
    onLost?.();
  }, false);
}

// Shown when the context is lost, so a crash turns into an explanation. Ultra
// has already been switched off by the handler above, so reloading recovers.
export function contextLossMessage() {
  const el = document.createElement('div');
  el.style.cssText = 'position:fixed;inset:0;z-index:9999;display:flex;flex-direction:column;'
    + 'align-items:center;justify-content:center;gap:18px;background:rgba(4,8,16,0.94);'
    + "color:#e8f4ff;font-family:'Orbitron',sans-serif;text-align:center;padding:32px;";
  el.innerHTML = '<div style="font-size:18px;letter-spacing:4px;color:#ffd97a;font-weight:800;">'
    + 'GRAPHICS DRIVER GAVE UP</div>'
    + '<div style="font-size:13px;line-height:1.7;max-width:460px;color:#9fb4d0;">'
    + 'Ultra Graphics was too much for this GPU, so it has been switched back off.<br>Reload to keep playing.</div>'
    + '<button style="font-family:inherit;font-size:13px;font-weight:800;letter-spacing:3px;'
    + 'padding:14px 28px;color:#040810;background:#ffd97a;border:0;border-radius:8px;cursor:pointer;">'
    + 'RELOAD</button>';
  el.querySelector('button').addEventListener('click', () => window.location.reload());
  return el;
}

// ── Procedural nebula cubemap ───────────────────────────────────────────────
// The stock skybox is 500 flat stars on near-black. This one layers fBm dust,
// coloured nebula bloom, a dense star field with size/temperature variation,
// and a handful of bright cores that the bloom pass turns into real glow.

// Integer bit-mix rather than the usual sin-based hash: same character of
// noise, but this runs on the integer unit and the field is evaluated a few
// million times at startup.
function hash(x, y, seed) {
  let h = (x | 0) * 374761393 + (y | 0) * 668265263 + (seed | 0) * 1442695041;
  h = (h ^ (h >>> 13)) * 1274126177;
  return ((h ^ (h >>> 16)) >>> 0) / 4294967296;
}

function valueNoise(x, y, seed) {
  const xi = Math.floor(x), yi = Math.floor(y);
  const xf = x - xi, yf = y - yi;
  const u = xf * xf * (3 - 2 * xf);
  const v = yf * yf * (3 - 2 * yf);
  const a = hash(xi, yi, seed);
  const b = hash(xi + 1, yi, seed);
  const c = hash(xi, yi + 1, seed);
  const d = hash(xi + 1, yi + 1, seed);
  return a * (1 - u) * (1 - v) + b * u * (1 - v) + c * (1 - u) * v + d * u * v;
}

function fbm(x, y, seed, octaves = 5) {
  let sum = 0, amp = 0.5, freq = 1, norm = 0;
  for (let i = 0; i < octaves; i++) {
    sum += amp * valueNoise(x * freq, y * freq, seed + i * 17);
    norm += amp;
    amp *= 0.5;
    freq *= 2.06;
  }
  return sum / norm;
}

// Direction vector for a texel on cube face `face`, in [-1,1] face coords.
function cubeDir(face, s, t) {
  switch (face) {
    case 0: return [1, -t, -s];   // +X
    case 1: return [-1, -t, s];   // -X
    case 2: return [s, 1, t];     // +Y
    case 3: return [s, -1, -t];   // -Y
    case 4: return [s, -t, 1];    // +Z
    default: return [-s, -t, -1]; // -Z
  }
}

// A few fixed nebula lobes in world space so the clouds line up across seams.
// `spread` is the half-angle of the lobe as a fraction: only directions with
// dot > 1 - spread get any contribution, so keeping these well under 1 leaves
// most of the sky genuinely black.
const NEBULAE = [
  { dir: [0.62, 0.28, -0.73], col: [0.30, 0.52, 1.0], spread: 0.85, gain: 0.80 },
  { dir: [-0.78, -0.12, -0.61], col: [0.95, 0.30, 0.55], spread: 0.68, gain: 0.52 },
  { dir: [-0.30, 0.70, 0.65], col: [0.35, 0.85, 0.95], spread: 0.60, gain: 0.38 },
  { dir: [0.20, -0.75, 0.63], col: [0.55, 0.40, 1.00], spread: 0.55, gain: 0.34 },
];

// Linear resolution divisor for the fBm field. The dust and nebula lobes are
// smooth by construction, so evaluating them at a quarter of the face size and
// letting drawImage interpolate is indistinguishable from the full-rate
// version — and it is 16x less work. Stars are drawn afterwards at full
// resolution, so nothing that needs to be sharp goes through this.
const FIELD_DIV = 4;

function makeNebulaFace(size, face) {
  const c = document.createElement('canvas');
  c.width = c.height = size;
  const ctx = c.getContext('2d', { willReadFrequently: false });

  const fieldSize = Math.max(4, Math.floor(size / FIELD_DIV));
  const field = document.createElement('canvas');
  field.width = field.height = fieldSize;
  const fctx = field.getContext('2d', { willReadFrequently: false });
  const img = fctx.createImageData(fieldSize, fieldSize);
  const data = img.data;

  for (let py = 0; py < fieldSize; py++) {
    const t = (py + 0.5) / fieldSize * 2 - 1;
    for (let px = 0; px < fieldSize; px++) {
      const s = (px + 0.5) / fieldSize * 2 - 1;
      const d = cubeDir(face, s, t);
      const len = Math.hypot(d[0], d[1], d[2]);
      const nx = d[0] / len, ny = d[1] / len, nz = d[2] / len;

      // Deep space base — a very dark blue, not pure black, so the hull has
      // something to reflect.
      let r = 0.012, g = 0.016, b = 0.032;

      // Direction-driven fBm so the dust is continuous across cube seams.
      const warp = fbm(nx * 2.4 + 8, ny * 2.4 + nz * 1.3, 3, 4);
      const dust = fbm(nx * 3.1 + warp * 0.9, ny * 3.1 + nz * 2.0 + warp * 0.9, 91, 6);

      for (const n of NEBULAE) {
        const dot = nx * n.dir[0] + ny * n.dir[1] + nz * n.dir[2];
        // Falls off away from the lobe centre.
        let m = Math.max(0, (dot - (1 - n.spread)) / n.spread);
        m = Math.pow(m, 2.6);
        if (m <= 0.0005) continue;
        const cloud = Math.pow(Math.max(0, dust - 0.34) / 0.66, 1.9);
        const a = m * cloud * n.gain;
        r += n.col[0] * a * 0.85;
        g += n.col[1] * a * 0.85;
        b += n.col[2] * a * 0.85;
      }

      // Faint cold haze everywhere so empty sky is not dead flat.
      const haze = Math.pow(Math.max(0, dust - 0.62) / 0.38, 2.4) * 0.10;
      r += haze * 0.30; g += haze * 0.45; b += haze * 0.85;

      const o = (py * fieldSize + px) * 4;
      data[o] = Math.min(255, r * 255);
      data[o + 1] = Math.min(255, g * 255);
      data[o + 2] = Math.min(255, b * 255);
      data[o + 3] = 255;
    }
  }
  fctx.putImageData(img, 0, 0);
  ctx.imageSmoothingEnabled = true;
  ctx.imageSmoothingQuality = 'high';
  ctx.drawImage(field, 0, 0, fieldSize, fieldSize, 0, 0, size, size);

  // Stars on top, drawn with the 2D API so they get proper antialiasing.
  const starCount = Math.floor(size * size / 620);
  for (let i = 0; i < starCount; i++) {
    const x = Math.random() * size;
    const y = Math.random() * size;
    const a = Math.pow(Math.random(), 2.6);
    const rad = a * 1.5 + 0.25;
    // Blue-white through amber, weighted toward white.
    const temp = Math.random();
    const cr = temp < 0.7 ? 255 : 255;
    const cg = temp < 0.7 ? 250 : 225 - temp * 40;
    const cb = temp < 0.7 ? 255 : 190 - temp * 60;
    ctx.fillStyle = `rgba(${cr|0}, ${cg|0}, ${cb|0}, ${0.25 + a * 0.75})`;
    ctx.beginPath();
    ctx.arc(x, y, rad, 0, Math.PI * 2);
    ctx.fill();
  }

  // Bright cores with a halo — these push past the bloom threshold and bleed.
  for (let i = 0; i < 10; i++) {
    const x = Math.random() * size;
    const y = Math.random() * size;
    const hue = 190 + Math.random() * 90;
    const halo = ctx.createRadialGradient(x, y, 0, x, y, 9 + Math.random() * 14);
    halo.addColorStop(0, `hsla(${hue}, 90%, 92%, 0.85)`);
    halo.addColorStop(0.35, `hsla(${hue}, 90%, 70%, 0.22)`);
    halo.addColorStop(1, 'hsla(0,0%,0%,0)');
    ctx.fillStyle = halo;
    ctx.fillRect(x - 26, y - 26, 52, 52);
    ctx.fillStyle = '#ffffff';
    ctx.beginPath();
    ctx.arc(x, y, 1.3 + Math.random(), 0, Math.PI * 2);
    ctx.fill();
  }
  return c;
}

export function createUltraSkybox() {
  const size = 1024;
  const faces = [];
  for (let i = 0; i < 6; i++) faces.push(makeNebulaFace(size, i));
  const tex = new THREE.CubeTexture(faces);
  tex.needsUpdate = true;
  tex.colorSpace = THREE.SRGBColorSpace;
  return tex;
}

// ── Image-based lighting ────────────────────────────────────────────────────

// Prefilter a cubemap into a radiance map and hang it on scene.environment so
// every MeshStandardMaterial in the game picks up real reflections.
export function applyEnvironment(scene, renderer, cubeTexture) {
  if (!ULTRA || !cubeTexture) return null;
  const pmrem = new THREE.PMREMGenerator(renderer);
  pmrem.compileCubemapShader();
  const rt = pmrem.fromCubemap(cubeTexture);
  scene.environment = rt.texture;
  pmrem.dispose();
  return rt.texture;
}

// A sky/ground gradient environment for the terrain map, where the background
// is a flat colour rather than a cubemap.
export function applySkyEnvironment(scene, renderer, skyColor, groundColor) {
  if (!ULTRA) return null;
  const sky = new THREE.Color(skyColor);
  const ground = new THREE.Color(groundColor);
  const size = 128;
  const c = document.createElement('canvas');
  c.width = c.height = size;
  const ctx = c.getContext('2d');
  const faces = [];
  for (let i = 0; i < 6; i++) {
    if (i === 2) ctx.fillStyle = `#${sky.getHexString()}`;
    else if (i === 3) ctx.fillStyle = `#${ground.getHexString()}`;
    else {
      const grad = ctx.createLinearGradient(0, 0, 0, size);
      grad.addColorStop(0, `#${sky.getHexString()}`);
      grad.addColorStop(0.5, `#${sky.clone().lerp(ground, 0.5).getHexString()}`);
      grad.addColorStop(1, `#${ground.getHexString()}`);
      ctx.fillStyle = grad;
    }
    ctx.fillRect(0, 0, size, size);
    const copy = document.createElement('canvas');
    copy.width = copy.height = size;
    copy.getContext('2d').drawImage(c, 0, 0);
    faces.push(copy);
  }
  const cube = new THREE.CubeTexture(faces);
  cube.needsUpdate = true;
  cube.colorSpace = THREE.SRGBColorSpace;
  return applyEnvironment(scene, renderer, cube);
}

// ── Material upgrade ────────────────────────────────────────────────────────

// Walk a subtree and make its materials respond to the new lighting: pick up
// the env map, sharpen textures, and push emissive/unlit colours into HDR so
// the bloom pass has something above 1.0 to catch.
const _upgraded = new WeakSet();

function upgradeMaterials(root, opts = {}) {
  if (!ULTRA || !root) return;
  const {
    envIntensity = 0.45,
    glowBoost = 1.7,
    shadows = true,
    anisotropy = 8,
  } = opts;
  root.traverse((obj) => {
    if (opts.skip && opts.skip(obj)) return;
    if (obj.isMesh || obj.isPoints || obj.isSprite) {
      if (shadows && obj.isMesh) {
        // Big background props stay out of the shadow pass; they are handled
        // by the caller opting out.
        obj.castShadow = obj.castShadow || !!opts.cast;
        obj.receiveShadow = obj.receiveShadow || !!opts.receive;
      }
      const mats = Array.isArray(obj.material) ? obj.material : [obj.material];
      for (const m of mats) {
        if (!m || _upgraded.has(m)) continue;
        _upgraded.add(m);
        for (const key of ['map', 'emissiveMap', 'roughnessMap', 'metalnessMap', 'normalMap']) {
          const t = m[key];
          if (t && t.isTexture) {
            t.anisotropy = Math.max(t.anisotropy || 1, anisotropy);
            t.needsUpdate = true;
          }
        }
        if (m.isMeshStandardMaterial || m.isMeshPhysicalMaterial) {
          m.envMapIntensity = envIntensity;
          // Hull panels are authored flat-matte; a little metal and a tight
          // roughness is what gives them specular highlights and reflections.
          if (opts.metalness !== undefined) m.metalness = opts.metalness;
          if (opts.roughness !== undefined) m.roughness = opts.roughness;
          if (m.emissive && m.emissiveIntensity > 0) {
            m.emissiveIntensity *= glowBoost;
          }
          m.needsUpdate = true;
        } else if (m.isMeshBasicMaterial && m.blending === THREE.AdditiveBlending) {
          // Additive unlit geometry is this codebase's glow idiom — laser
          // bolts, engine plumes, beams, explosion shells. Pushing the colour
          // past 1.0 is what makes the bloom pass actually bite.
          m.color.multiplyScalar(glowBoost);
        }
      }
    }
  });
}

// One sweep over the whole scene. Ships are upgraded first with a metallic
// hull treatment; the generic pass then picks up everything else. Order
// matters because a material is only ever touched once.
export function sweepScene(scene) {
  if (!ULTRA) return;
  scene.traverse((o) => {
    if (o.name === 'Ship') {
      // The cockpit interior is a sibling child of the hull and has its own
      // authored palette — chroming it would wreck the instrument panel.
      upgradeMaterials(o, {
        metalness: 0.55,
        roughness: 0.34,
        envIntensity: 0.9,
        skip: (m) => !!m.userData?.isInterior,
      });
    }
  });
  upgradeMaterials(scene);
}

// ── Lighting ────────────────────────────────────────────────────────────────

// Replace the flat ambient + single directional with a three-point rig. Called
// instead of the default lights when ULTRA is on.
export function installSpaceLights(scene) {
  const lights = {};
  lights.key = new THREE.DirectionalLight(0xfff2dd, 2.7);
  lights.key.position.set(200, 300, 100);
  scene.add(lights.key);

  // Cool bounce from the opposite side to keep shadowed hull readable. Kept
  // gentle — a strong blue fill against the warm key turns grey rock violet.
  lights.fill = new THREE.DirectionalLight(0x7aa8e0, 0.45);
  lights.fill.position.set(-260, -120, -180);
  scene.add(lights.fill);

  // Warm rim from behind for silhouette separation against the nebula.
  lights.rim = new THREE.DirectionalLight(0xffa060, 0.30);
  lights.rim.position.set(-80, 60, -320);
  scene.add(lights.rim);

  // Sky/ground ambient — the nebula env map is too dark to carry this alone.
  // Kept near-neutral so it lifts shadows without tinting every rock.
  lights.hemi = new THREE.HemisphereLight(0x9fb4d0, 0x2b2f3a, 0.55);
  scene.add(lights.hemi);
  return lights;
}

export function upgradeTerrainSun(sun) {
  if (!ULTRA || !sun) return;
  sun.intensity = 2.2;
  sun.color.setHex(0xfff0d0);
  sun.shadow.mapSize.set(2048, 2048);
  sun.shadow.bias = -0.0005;
  sun.shadow.normalBias = 0.6;
  sun.shadow.radius = 2;
}

// ── Post-processing ─────────────────────────────────────────────────────────

// One combined final pass: ACES tone map, filmic contrast, radial chromatic
// aberration, vignette, and animated grain. Doing it in a single shader keeps
// the pass count down and avoids ambiguity about where encoding happens.
const GradeShader = {
  uniforms: {
    tDiffuse: { value: null },
    uExposure: { value: 1.10 },
    uTime: { value: 0 },
    uAberration: { value: 0.0014 },
    uVignette: { value: 0.34 },
    uGrain: { value: 0.009 },
    uSaturation: { value: 1.14 },
    uContrast: { value: 1.05 },
    uLift: { value: new THREE.Vector3(0.004, 0.006, 0.016) },
    uGain: { value: new THREE.Vector3(1.00, 0.995, 1.025) },
  },
  vertexShader: /* glsl */`
    varying vec2 vUv;
    void main() {
      vUv = uv;
      gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
    }
  `,
  fragmentShader: /* glsl */`
    uniform sampler2D tDiffuse;
    uniform float uExposure, uTime, uAberration, uVignette, uGrain, uSaturation, uContrast;
    uniform vec3 uLift, uGain;
    varying vec2 vUv;

    // ACES filmic approximation (Narkowicz).
    vec3 aces(vec3 x) {
      const float a = 2.51, b = 0.03, c = 2.43, d = 0.59, e = 0.14;
      return clamp((x * (a * x + b)) / (x * (c * x + d) + e), 0.0, 1.0);
    }

    float rand(vec2 co) {
      return fract(sin(dot(co, vec2(12.9898, 78.233))) * 43758.5453);
    }

    void main() {
      vec2 uv = vUv;
      vec2 toCenter = uv - 0.5;
      float r2 = dot(toCenter, toCenter);

      // Chromatic aberration grows toward the edges of frame.
      vec2 off = toCenter * uAberration * r2 * 4.0;
      vec3 hdr;
      hdr.r = texture2D(tDiffuse, uv + off).r;
      hdr.g = texture2D(tDiffuse, uv).g;
      hdr.b = texture2D(tDiffuse, uv - off).b;

      hdr *= uExposure;
      vec3 col = aces(hdr);

      // Grade in display-referred space.
      col = col * uGain + uLift;
      float luma = dot(col, vec3(0.2126, 0.7152, 0.0722));
      col = mix(vec3(luma), col, uSaturation);
      col = clamp((col - 0.5) * uContrast + 0.5, 0.0, 1.0);

      // Vignette.
      col *= 1.0 - uVignette * smoothstep(0.15, 0.85, r2 * 1.9);

      // Animated grain, strongest in the shadows where sensor noise lives.
      float n = rand(uv * vec2(1024.0, 1024.0) + fract(uTime) * 91.7) - 0.5;
      col += n * uGrain * (1.0 - smoothstep(0.0, 0.7, luma));

      // Manual sRGB encode — this pass writes straight to the canvas.
      col = clamp(col, 0.0, 1.0);
      vec3 srgb = mix(col * 12.92,
                      1.055 * pow(max(col, vec3(0.0031308)), vec3(1.0 / 2.4)) - 0.055,
                      step(0.0031308, col));
      gl_FragColor = vec4(srgb, 1.0);
    }
  `,
};

export function createComposer(renderer, scene, camera, opts = {}) {
  if (!ULTRA) return null;
  const size = renderer.getDrawingBufferSize(new THREE.Vector2());

  // HalfFloat so bloom has real headroom above 1.0; samples gives us MSAA
  // inside the composer, which plain antialias:true cannot do here.
  const quality = getQuality();

  const target = new THREE.WebGLRenderTarget(size.x, size.y, {
    type: THREE.HalfFloatType,
    format: THREE.RGBAFormat,
    colorSpace: THREE.LinearSRGBColorSpace,
    samples: opts.samples ?? quality.samples,
    stencilBuffer: false,
    depthBuffer: true,
  });

  const composer = new EffectComposer(renderer, target);
  composer.addPass(new RenderPass(scene, camera));

  // Bloom is inherently low frequency, so resolving it at half resolution and
  // letting the composite upsample costs about a quarter of the fill rate and
  // a quarter of the mip-chain memory for no visible difference.
  const bloomScale = opts.bloomScale ?? quality.bloomScale;
  const bloom = new UnrealBloomPass(
    new THREE.Vector2(Math.max(1, Math.round(size.x * bloomScale)),
                      Math.max(1, Math.round(size.y * bloomScale))),
    opts.bloomStrength ?? 0.58,
    opts.bloomRadius ?? 0.62,
    opts.bloomThreshold ?? 0.92,
  );
  composer.addPass(bloom);

  const grade = new ShaderPass(GradeShader);
  grade.renderToScreen = true;
  composer.addPass(grade);

  return {
    composer,
    bloom,
    grade,
    render(dt) {
      grade.uniforms.uTime.value += dt;
      composer.render(dt);
    },
    // Callers pass CSS pixels, but EffectComposer.setSize sets render-target
    // dimensions verbatim — unlike its constructor, which uses the drawing
    // buffer size. Scale by the pixel ratio or every resize quietly drops the
    // composer to CSS resolution.
    setSize(w, h) {
      const dpr = renderer.getPixelRatio();
      const bw = Math.floor(w * dpr);
      const bh = Math.floor(h * dpr);
      composer.setSize(bw, bh);
      // composer.setSize resizes every pass, which would put bloom back to
      // full resolution — restore the half-res chain afterwards.
      bloom.setSize(Math.max(1, Math.round(bw * bloomScale)),
                    Math.max(1, Math.round(bh * bloomScale)));
    },
    dispose() {
      composer.dispose();
      target.dispose();
    },
  };
}
