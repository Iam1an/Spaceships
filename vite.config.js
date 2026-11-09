import { defineConfig } from 'vite';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.dirname(fileURLToPath(import.meta.url));
const appRoot = path.join(repoRoot, 'public');

// The Node/Express game server (npm start) — Vite's dev server proxies to it.
const API_TARGET = 'http://localhost:4000';

// ── Static asset passthrough ─────────────────────────────────────────────────
// The game loads its models/textures/sounds with *runtime relative URL strings*
// (e.g. `loader.load('spaceship.glb')`, `new Audio('sounds/shoot.mp3')`), not
// with `import`/`new URL(..., import.meta.url)`. Rollup can't see those, so they
// have to land in dist/ at exactly the same relative paths they have in public/.
//
// We can't use Vite's normal `publicDir` for this because the app root *is*
// public/ (that's where index.html lives), and publicDir may not be the root.
// So we copy public/ into dist/ verbatim, minus the two things Vite itself owns:
// index.html (built + hashed) and src/ (bundled).
const COPY_EXCLUDE = new Set(['index.html', 'src']);

function copyStaticAssets(outDir) {
  return {
    name: 'spaceships-copy-static-assets',
    apply: 'build',
    // `writeBundle` runs after Rollup has written the bundle, so nothing we copy
    // gets clobbered by the build output.
    writeBundle() {
      let copied = 0;
      for (const entry of fs.readdirSync(appRoot, { withFileTypes: true })) {
        if (COPY_EXCLUDE.has(entry.name)) continue;
        const from = path.join(appRoot, entry.name);
        const to = path.join(outDir, entry.name);
        fs.cpSync(from, to, { recursive: true });
        copied += entry.isDirectory()
          ? fs.readdirSync(from, { recursive: true }).length
          : 1;
      }
      this.info(`copied ${copied} static asset(s) from public/ into ${path.relative(repoRoot, outDir)}/`);
    },
  };
}

const outDir = path.join(repoRoot, 'dist');

const proxy = {
  '/api': { target: API_TARGET, changeOrigin: true },
  // Client code calls `/spaceships/api/...` because production sits behind a
  // reverse proxy that mounts the game at /spaceships and strips the prefix.
  // Reproduce that here so those calls reach the real `/api/...` routes.
  '/spaceships': {
    target: API_TARGET,
    changeOrigin: true,
    rewrite: (p) => p.replace(/^\/spaceships/, ''),
  },
  // WebSocket upgrade for multiplayer (server/index.js only accepts /ws).
  '/ws': { target: API_TARGET, ws: true, changeOrigin: true },
};

export default defineConfig({
  // index.html lives in public/, so that directory is the app root.
  root: appRoot,

  // Disabled deliberately — see copyStaticAssets() above. Vite refuses a
  // publicDir that is the same as (or inside) root, and ours would be both.
  publicDir: false,

  // Relative asset URLs in the built HTML, so dist/ works whether it's served
  // from `/` (npm start) or behind the `/spaceships` path prefix used in prod.
  base: './',

  build: {
    outDir,
    // outDir is outside root, so Vite won't clear it unless we say so.
    emptyOutDir: true,
    target: 'es2022',
    sourcemap: true,
    // three.js is ~600 kB minified; that's expected, not a regression.
    chunkSizeWarningLimit: 1200,
  },

  // The game server owns the API, the WebSocket, and (in prod) the
  // `/spaceships` path prefix. Everything else is served/HMR'd by Vite.
  server: { port: 5173, strictPort: true, proxy },
  // `vite preview` serves dist/, so it needs the same backend wiring.
  // (`npm start` is usually the better way to exercise a build — it serves
  // dist/ from the real server, no proxy involved.)
  preview: { port: 4173, strictPort: true, proxy },

  // ── WASM readiness (Rust port, spaceships-rs/) ─────────────────────────────
  // No wasm-pack output exists yet. When one is added, this config needs:
  //
  //  1. `optimizeDeps.exclude: ['<pkg-name>']` — the wasm-pack JS shim must not
  //     be pre-bundled by esbuild; esbuild can't handle its `new URL(..., import.meta.url)`
  //     .wasm reference and dev will 404 on the binary.
  //  2. `assetsInclude: ['**/*.wasm']` if the .wasm is fetched by URL rather than
  //     imported, so Rollup emits it instead of trying to parse it.
  //  3. `build.target` stays at es2022 or higher — wasm-pack's ESM output uses
  //     top-level await; anything below es2022 fails to build.
  //  4. If the crate is built with threads (wasm-bindgen-rayon / SharedArrayBuffer),
  //     add COOP/COEP headers to BOTH `server.headers` and `preview.headers`:
  //       'Cross-Origin-Opener-Policy': 'same-origin'
  //       'Cross-Origin-Embedder-Policy': 'require-corp'
  //     and the same headers must be added to the Express/Rust server for prod.
  //  5. A `wasm-pack build --target web --out-dir ...` step goes in front of
  //     `npm run build` (and a watch of it in front of `npm run dev`).
  //
  // Nothing below is needed until then; `target: 'es2022'` above already covers (3).
  plugins: [copyStaticAssets(outDir)],
});
