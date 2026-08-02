#!/usr/bin/env bash
# Builds the client for the browser and reports what it costs.
#
# Output lands in crates/client/web/, which is a self-contained static site:
#
#   web/
#     index.html                  (checked in)
#     spaceships-client.js        (wasm-bindgen shim)
#     spaceships-client_bg.wasm   (the payload)
#     assets/                     (copied from public/)
#
# Serve it with any static server, e.g.
#
#   python3 -m http.server -d crates/client/web 8080
#
# Folding this into the Vite build is the next step; vite.config.js already
# documents what that needs.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace="$here/../.."
repo="$workspace/.."
out="$here/web"

# ── Toolchain ────────────────────────────────────────────────────────────────
# wasm-bindgen's CLI and the `wasm-bindgen` crate in the lockfile must be the
# *same* version — the CLI reads a schema hash baked into the .wasm by the
# crate, and a mismatch fails with a message that does not obviously say so.
# Bevy does not pin the crate, so the lockfile is the source of truth.
want="$(awk '/^name = "wasm-bindgen"$/{getline; gsub(/[",]/,""); print $3; exit}' "$workspace/Cargo.lock")"
have="$(wasm-bindgen --version 2>/dev/null | awk '{print $2}' || true)"
if [ "$have" != "$want" ]; then
  echo "wasm-bindgen CLI is '${have:-missing}', lockfile wants '$want'." >&2
  echo "  cargo install wasm-bindgen-cli --version $want" >&2
  echo "  # or grab the prebuilt binary from the wasm-bindgen releases page" >&2
  exit 1
fi

# ── Compile ──────────────────────────────────────────────────────────────────
# The getrandom cfg is mandatory, not advisory: on wasm32-unknown-unknown
# getrandom 0.3 has no default backend, and without this the link fails on a
# missing `__getrandom_v03_custom` symbol that reads like a Bevy bug.
export RUSTFLAGS='--cfg getrandom_backend="wasm_js"'

# `wasm-release` rather than `release`: see the profile in the workspace
# manifest. Pass PROFILE=release to compare.
profile="${PROFILE:-wasm-release}"

cargo build \
  --manifest-path "$workspace/Cargo.toml" \
  -p spaceships-client \
  --profile "$profile" \
  --target wasm32-unknown-unknown

raw="$workspace/target/wasm32-unknown-unknown/$profile/spaceships-client.wasm"

# ── Bindings ─────────────────────────────────────────────────────────────────
mkdir -p "$out"
wasm-bindgen \
  --out-dir "$out" \
  --out-name spaceships-client \
  --target web \
  --no-typescript \
  "$raw"

bg="$out/spaceships-client_bg.wasm"
before=$(wc -c <"$bg")

# ── Size pass ────────────────────────────────────────────────────────────────
# `-Oz` rather than `-O3`: this build is bandwidth-bound, not CPU-bound. wgpu
# and naga carry a lot of code that a shipping game never reaches, and -Oz plus
# aggressive inlining removal is where most of it goes.
#
# The `--enable-*` flags are not optional. rustc emits bulk-memory,
# sign-ext, and non-trapping float casts for wasm32-unknown-unknown by
# default, but binaryen still defaults those proposals off, so without them
# wasm-opt rejects its own input with "wasm-validator error ... requires bulk
# memory" — which reads like a corrupt build and is not one.
if command -v wasm-opt >/dev/null 2>&1; then
  wasm-opt -Oz \
    --enable-bulk-memory \
    --enable-bulk-memory-opt \
    --enable-sign-ext \
    --enable-nontrapping-float-to-int \
    --enable-mutable-globals \
    --enable-multivalue \
    --enable-reference-types \
    --strip-debug --strip-producers \
    -o "$bg.opt" "$bg"
  mv "$bg.opt" "$bg"
else
  echo "note: wasm-opt not found; skipping the size pass" >&2
fi
after=$(wc -c <"$bg")

# ── Assets ───────────────────────────────────────────────────────────────────
# What the client actually loads. `spaceshipADMIN.glb` stays out: it is 4.9 MB
# for a model most players never own, and the JS loads it unconditionally on
# every session, which is a mistake worth not repeating here.
mkdir -p "$out/assets/sounds/warnings" "$out/assets/fonts"

# Models and textures.
cp "$repo/public/spaceship.glb"       "$out/assets/"
cp "$repo/public/jet.glb"             "$out/assets/"
cp "$repo/public/moon Texture.jpg"    "$out/assets/"
cp "$repo/public/sounds/asteroid.jpg" "$out/assets/sounds/"

# The tab icon. Not loaded by the engine — `index.html` links it — but it
# belongs with the rest of the copied payload rather than in a deploy step.
cp "$repo/public/favicon.png"         "$out/assets/"

# Audio. `audio.rs` loads every effect plus the fourteen voice warnings, and a
# missing file is silent rather than an error, so an incomplete copy here shows
# up as a game that simply has no sound.
cp "$repo/public/sounds/"*.mp3          "$out/assets/sounds/"
cp "$repo/public/sounds/warnings/"*.mp3 "$out/assets/sounds/warnings/"

# Orbitron, the HUD face. Without it `hud.rs` silently falls back to bevy's
# embedded FiraMono subset — no error, just the wrong typeface.
cp "$repo/public/fonts/"*.ttf "$out/assets/fonts/"

# ── Precompressed sidecars ───────────────────────────────────────────────────
# Caddy's `file_server { precompressed br gzip }` serves `foo.wasm.br` in place
# of `foo.wasm` when the client sends `Accept-Encoding: br`, and falls back to
# the plain file otherwise. Compressing here rather than per-request matters at
# this size: `encode` would spend ~a second of CPU on every cold load, and at
# `-q 11` brotli takes roughly a quarter of what gzip leaves.
#
# These were already being computed and thrown away for the size report below;
# now the report reads the files. Both are regenerated every build — a stale
# `.br` beside a fresh `.wasm` is served in preference to it, which fails as a
# wasm-bindgen schema mismatch and gives no hint that the cause is a file the
# page never names.
for f in "$bg" "$out/spaceships-client.js"; do
  gzip -9 -c "$f" >"$f.gz"
  if command -v brotli >/dev/null 2>&1; then
    brotli -q 11 -f -c "$f" >"$f.br"
  else
    rm -f "$f.br"
  fi
done

gz=$(wc -c <"$bg.gz")
br=$( ( [ -f "$bg.br" ] && wc -c <"$bg.br" ) || echo 0)

# ── Build stamp ──────────────────────────────────────────────────────────────
# `index.html` needs the *uncompressed* size to draw a progress bar, and cannot
# derive it: with the brotli sidecar in play `Content-Length` is the compressed
# size, so a bar scaled to it finishes at ~20% and stops.
cat >"$out/build.json" <<EOF
{ "wasm": $after, "wasm_br": $br, "wasm_gz": $gz, "profile": "$profile" }
EOF

# ── Report ───────────────────────────────────────────────────────────────────
fmt() { awk -v b="$1" 'BEGIN{ printf "%.1f MB (%d bytes)", b/1048576, b }'; }
echo
echo "wasm    before wasm-opt : $(fmt "$before")"
echo "        after  wasm-opt : $(fmt "$after")"
echo "        gzip -9          : $(fmt "$gz")"
[ "$br" -gt 0 ] && echo "        brotli -q 11     : $(fmt "$br")"
echo "assets                   : $(fmt "$(find "$out/assets" -type f -exec wc -c {} + | tail -1 | awk '{print $1}')")"
echo
echo "serve:  python3 -m http.server -d $out 8080"
echo "deploy: $repo/deploy/deploy-rs-web.sh"
