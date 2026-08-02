#!/usr/bin/env bash
# Ships the Rust/Bevy web client to gheat.net/spaceships/rs/.
#
# It copies files and nothing else. It does not touch the Caddyfile, pm2, the
# Node server, or pilots.db — the route that makes this directory reachable is
# a one-time edit, recorded in deploy/README.md.
#
#   spaceships-rs/crates/client/build-wasm.sh   # build first
#   deploy/deploy-rs-web.sh                     # then this
#
# Set HOST to deploy somewhere else; DRY=1 to see what would move.

set -euo pipefail

host="${HOST:-gheat@100.81.137.100}"
dest="${DEST:-/var/www/Gheat.net/spaceships-rs-web}"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
web="$here/../spaceships-rs/crates/client/web"

# ── Preflight ────────────────────────────────────────────────────────────────
# Every one of these has a failure mode that only shows up in the browser, as a
# blank page or silence, so they are checked here instead.
need() {
  [ -e "$web/$1" ] || { echo "missing $1 — run build-wasm.sh first" >&2; exit 1; }
}
need index.html
need spaceships-client.js
need spaceships-client_bg.wasm
need build.json
need assets/spaceship.glb
need assets/jet.glb
need assets/fonts

# A `.br` older than the `.wasm` beside it is worse than none: Caddy prefers it,
# and the page then fails on a wasm-bindgen schema mismatch that names neither
# file.
for f in spaceships-client_bg.wasm spaceships-client.js; do
  for enc in br gz; do
    if [ -f "$web/$f.$enc" ] && [ "$web/$f.$enc" -ot "$web/$f" ]; then
      echo "$f.$enc is older than $f — re-run build-wasm.sh" >&2
      exit 1
    fi
  done
done

# Fourteen voice warnings plus twelve effects; `audio.rs` treats a missing file
# as silence rather than an error, so a short copy is invisible until someone
# notices the game has no sound.
sounds=$(find "$web/assets/sounds" -name '*.mp3' | wc -l | tr -d ' ')
[ "$sounds" -ge 26 ] || { echo "only $sounds mp3s under assets/sounds (expected 26)" >&2; exit 1; }

echo "→ $host:$dest"
du -sh "$web" | awk '{print "  payload: " $1}'

# ── Copy ─────────────────────────────────────────────────────────────────────
# `--delete` so a renamed asset does not linger and get served forever.
#
# No `--chmod`: macOS ships openrsync, which rejects the flag outright ("invalid
# argument"), so the modes are fixed afterwards over ssh instead. Caddy runs as
# its own user and serves nothing it cannot read, and a file arriving 600 from a
# umask is a 403 with no other symptom.
rsync -av --delete ${DRY:+--dry-run} \
  --exclude='.gitignore' \
  "$web/" "$host:$dest/"

[ -n "${DRY:-}" ] && exit 0

ssh "$host" "chmod -R a+rX '$dest'"

# ── Verify ───────────────────────────────────────────────────────────────────
# Through Caddy, not the filesystem: the things that break here are the route,
# the MIME type, and the precompressed sidecar, none of which a file listing
# would show.
base="${BASE_URL:-https://gheat.net/spaceships/rs}"
echo
for probe in "/" "/spaceships-client_bg.wasm" "/assets/spaceship.glb"; do
  printf '  %-32s ' "$probe"
  curl -sS -o /dev/null -H 'Accept-Encoding: br' \
    -w 'HTTP %{http_code}  %{content_type}  %{size_download} bytes\n' \
    "$base$probe"
done

# The JS client must be unharmed. It shares a path prefix with this one and a
# mis-ordered route would shadow it.
printf '  %-32s ' "/spaceships/ (Three.js)"
curl -sS -o /dev/null -w 'HTTP %{http_code}  %{content_type}\n' \
  "${JS_URL:-https://gheat.net/spaceships/}"
