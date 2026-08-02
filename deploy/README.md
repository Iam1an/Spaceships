# Deployment

Production is `gheat@100.81.137.100`, a git checkout at
`/var/www/Gheat.net/spaceships` on `main`.

## How requests reach the game

Caddy (`/etc/caddy/Caddyfile`) — nginx is installed but inactive.

```
redir /spaceships /spaceships/ 301
redir /spaceships/rs /spaceships/rs/ 301
handle_path /spaceships/rs/*  ->  file_server /var/www/Gheat.net/spaceships-rs-web
handle_path /spaceships/*     ->  reverse_proxy 127.0.0.1:4000
handle      /ws               ->  reverse_proxy 127.0.0.1:4000
```

**`handle_path` strips the matched prefix.** So `/spaceships/api/login` arrives
at the server as `/api/login`. The client calling `/spaceships/api/*` and the
server registering `/api/*` are *both* correct — they only appear to disagree on
a local machine, where there is no proxy in front. Nothing to fix.

Note the WebSocket is a separate `handle /ws`, **not** under `/spaceships/`, so
it is *not* prefix-stripped. A client deriving its socket URL from the page has
to account for that asymmetry. Both clients do: the Bevy one builds
`{scheme}//{host}/ws` from `window.location`, ignoring the page's path entirely,
which is why it works unchanged from a `/spaceships/rs/` subpath.

`/spaceships/rs/*` and `/spaceships/*` overlap, and **the order they appear in
the file is not what decides it.** Caddy sorts `handle`/`handle_path` blocks by
path specificity, so the longer prefix wins wherever it is written. That was
verified rather than assumed, by diffing the adapted JSON:

```bash
caddy adapt --config /etc/caddy/Caddyfile --adapter caddyfile --pretty
```

which lists `/spaceships/rs/*` at index 3 and `/spaceships/*` at index 6 in
`apps.http.servers.srv0`. Worth re-running after any edit near these two.

**Anything serving this game must bind `127.0.0.1:4000`.**

## Process management: pm2, not systemd

The server runs under **pm2** as `spaceships` (id 5 as of 2026-08-01; the id is
not stable across `pm2 delete`/`start`, so match on the name), and
`pm2-root.service` is
enabled, so it restarts on crash *and* resurrects on boot. There is deliberately
no systemd unit for it — one was written and installed here, then removed, on
finding pm2 already owned the job: both would have raced to bind `:4000` at
boot.

```bash
pm2 list
pm2 logs spaceships --lines 50
pm2 restart spaceships
pm2 describe spaceships
```

Other pm2 apps on the same host: `gheat-net`, `gheat-next`, `gianniandson-api`.

Historical note: `~/.pm2/logs/spaceships-error.log` carries 32
`Cannot find module '/var/www/Gheat.net/spaceships'` crashes — a pm2 entry that
pointed at the directory instead of a script. Last written 27 June; the entry
has been correct since, and the restart counter is not reset by fixing it.

## Deploying a change

```bash
ssh gheat@100.81.137.100
cd /var/www/Gheat.net/spaceships
git pull
npm ci                      # three and vite are runtime deps, not dev deps
npm run build               # writes dist/; the server prefers it over public/
pm2 restart spaceships
```

pm2 runs `server/index.js` directly, so `prestart` never fires — the build has
to be an explicit deploy step.

## The Rust/Bevy client on the web — `gheat.net/spaceships/rs/`

Live since 2026-08-01. **Static files only.** It is a second, independent client
served beside the Three.js one; nothing about `/spaceships/` was changed, the
Node server was not touched, and the two share the same `/ws` and the same
`pilots.db` through it.

```bash
spaceships-rs/crates/client/build-wasm.sh   # build
deploy/deploy-rs-web.sh                     # rsync to /var/www/Gheat.net/spaceships-rs-web
```

Two tools the repo does not vendor, neither of them optional:

```bash
rustup target add wasm32-unknown-unknown
brew install binaryen        # wasm-opt: 23.6 MB -> 20.5 MB
# wasm-bindgen's CLI version must EQUAL the crate version in Cargo.lock; the
# script checks and refuses otherwise, because a mismatched schema hash fails
# later with a message that never mentions the version. Prebuilt binary:
curl -sSL https://github.com/wasm-bindgen/wasm-bindgen/releases/download/0.2.126/wasm-bindgen-0.2.126-aarch64-apple-darwin.tar.gz \
  | tar xz -C /tmp && cp /tmp/wasm-bindgen-*/wasm-bindgen ~/.local/bin/
```

`build-wasm.sh` takes about seven minutes cold, and most of that is one fat-LTO
codegen unit plus `wasm-opt -Oz` over 24 MB. **Do not edit the script while it is
running** — bash reads it incrementally and will run a mixture of both versions.

The deploy script copies and verifies and does nothing else — it never touches
Caddy, pm2, or the database. The route below is a one-time edit, already done.

### What was changed on the host, exactly

1. **`/etc/caddy/Caddyfile`** — one `redir` and one `handle_path` block added
   immediately above the existing `handle_path /spaceships/*`. No existing
   directive was edited, reordered, or removed:

   ```
   redir /spaceships/rs /spaceships/rs/ 301

   handle_path /spaceships/rs/* {
       root * /var/www/Gheat.net/spaceships-rs-web
       header Cache-Control "no-cache"
       file_server {
           precompressed br gzip
       }
   }
   ```

   Validated with `caddy validate`, then applied with `systemctl reload caddy`
   (**reload, not restart** — the unit's `ExecReload` is `caddy reload --force`,
   the main PID is unchanged, and live WebSocket matches are not dropped).

2. **`/var/www/Gheat.net/spaceships-rs-web/`** — new directory, ~40 MB, owned by
   `gheat`. Outside the `/var/www/Gheat.net/spaceships` git checkout on purpose,
   so `git pull` on the JS game can never collide with it.

Nothing else. pm2, `server/index.js`, and `pilots.db` were not touched.

### Rolling back

Backups of the pre-change Caddyfile, byte-identical, in two places:

- `/etc/caddy/Caddyfile.bak.20260801-pre-spaceships-rs`
- `~gheat/caddy-backups/Caddyfile.2026-08-01-211836.bak`

```bash
sudo install -m 644 -o root -g root \
  /etc/caddy/Caddyfile.bak.20260801-pre-spaceships-rs /etc/caddy/Caddyfile
sudo caddy validate --config /etc/caddy/Caddyfile
sudo systemctl reload caddy
```

`/spaceships/rs/*` then falls through to the Node server again and 404s, which
is what it did before. The files can stay — nothing reaches them — or go with
`rm -rf /var/www/Gheat.net/spaceships-rs-web`. **Neither step affects
`/spaceships/`**, which is the point of adding a route rather than editing one.

### The payload, honestly

A Bevy build is not small. As shipped:

| | bytes | |
|---|---|---|
| rustc output (`wasm-release`, `opt-level="z"`, fat LTO, `panic=abort`, stripped) | 24,756,804 | 23.6 MB |
| after `wasm-opt -Oz` | 21,452,201 | **20.5 MB** — what the browser compiles |
| `gzip -9` | 7,137,189 | 6.8 MB |
| `brotli -q 11` | 4,963,471 | **4.7 MB** — what actually crosses the wire |
| assets (models, 26 mp3s, 2 fonts, textures) | 6,806,773 | 6.5 MB |

Caddy serves the brotli sidecar via `precompressed br gzip` — `build-wasm.sh`
writes `.br` and `.gz` next to the `.wasm`, so this costs no per-request CPU,
and Cloudflare passes `Content-Encoding: br` through untouched. Confirmed:

```bash
curl -sI -H 'Accept-Encoding: br' https://gheat.net/spaceships/rs/spaceships-client_bg.wasm
# content-encoding: br   content-length: 4963471   content-type: application/wasm
```

`encode` is deliberately **not** in the block: compressing 20 MB per cold load
would be about a second of CPU each time, for a worse ratio than `-q 11`.

A cold fetch measured 4.96 MB in **0.9–1.2 s** through Cloudflare. First load
including every asset the engine pulls at boot is about **11.3 MB** — the wasm
plus 5.2 MB of mp3.

`Cache-Control: no-cache` on the block is deliberate and has one cost worth
knowing. It makes the browser revalidate, so a returning player gets a 304 and
re-downloads nothing, and a redeploy can never leave a stale `.wasm` beside a
fresh `.js` — the failure that would cause is a wasm-bindgen schema mismatch
naming neither file. The cost is `cf-cache-status: DYNAMIC`: Cloudflare does not
edge-cache it either, so **every** first-time visitor pulls 4.7 MB from this
box. If that ever matters, the fix is a Cloudflare Cache Rule on
`/spaceships/rs/*` plus content-hashed filenames — not dropping `no-cache`
while the filenames are still fixed.

4.7 MB is a usable first load and a noticeable one. `index.html` therefore
fetches the wasm itself through a `TransformStream` to draw a real progress bar
while `instantiateStreaming` compiles, reading the *uncompressed* size from
`build.json` — `Content-Length` is the brotli size and a bar scaled to it would
finish at 23%.

If it needs to be smaller, in rough order of return, none of them attempted here
because they are all changes to `crates/client/Cargo.toml`:

- **`reflect_auto_register`** registers every reflected type in the binary,
  including ones nothing ever queries. Bevy's own docs flag it as a size cost.
- **`bevy_post_process`** pulls the whole post-process render graph for bloom,
  chromatic aberration, and vignette. The web build already logs that depth of
  field is disabled on WebGL2 anyway.
- **wgpu + naga** are the floor, and by some distance the largest single share:
  naga's WGSL front end and GLSL back end are compiled in because every shader
  is translated at runtime for WebGL2. There is no precompiled-shader path in
  Bevy 0.19.
- **`mp3`** drags in symphonia. Re-encoding the 26 clips to a format bevy
  decodes more cheaply would trade wasm bytes for asset bytes.
- The two biggest *assets* are `dumb_Eflatmin.mp3` (2.8 MB) and `move.mp3`
  (2.0 MB) — 4.8 MB of the 6.5 MB. They are music and an engine loop; loading
  them lazily after the first frame would cut the time-to-playable more than
  any wasm work.

### Verified in a browser, not just built

Headed Chromium against `https://gheat.net/spaceships/rs/`, on an M5:

- Boots in ~1.2 s on a warm connection; renders through ANGLE's Metal backend on
  WebGL2 at a steady **144 fps** (0.7 ms of main-world CPU per frame).
- The CRT lobby, the Skirmish match, HUD, trails, asteroids, weapons and target
  boxes are pixel-equivalent to the native Metal build.
- Audio plays. This needs the `AudioContext` shim in `web/index.html`, without
  which everything initialises cleanly and there is silence — see the trace in
  `audio.rs`'s module docs.
- Pointer lock engages on click (it fails only when the window is not OS-focused,
  which is an automation artifact, not a bug).
- Guest multiplayer works end to end: `wss://gheat.net/ws` connects, a room is
  created on the live Node server, `start` comes back with server spawns and the
  asteroid seed, and `state`/`bot-state` flow at 20 Hz against a
  server-authoritative timer.

Known cosmetic non-issue: WebGL2 has no compute, so the console logs a handful
of `*Plugin not loaded` warnings (SSAO, OIT, atmosphere, motion vectors) and
falls back to CPU clustering and CPU batch preprocessing. Expected, and the
frame rate above is *with* those fallbacks.

### Getting between the two clients

The Bevy page has a "Three.js version" link to `/spaceships/` in its bottom-right
corner — one anchor, static, in `crates/client/web/index.html`.

The reverse link does not exist, because it belongs in `public/index.html`, which
is the JS client's territory. It is one line wherever the lobby footer lives:

```html
<a href="/spaceships/rs/">Try the Rust version</a>
```

## Cutting over to the Rust server

`crates/server` is a drop-in replacement: same routes, same JSON, same
`pilots.db`, and JWTs cross-verify in both directions, so tokens issued by the
Node server keep working and vice versa. Caddy needs no changes.

Verified against a snapshot of the **live** database (19 pilots, 143
achievements, 310 credit transactions) — leaderboard, rank tiers, KDR
formatting, trial bests and achievement metadata all come back correct.

1. **Back up the database first.** It holds real accounts with real bcrypt
   hashes and real credit balances.
   ```bash
   sqlite3 "file:pilots.db?mode=ro" ".backup pilots-$(date +%F).db"
   ```
2. Build the binary for the host (Arch, x86_64) and copy it over.
3. Point pm2 at the binary instead of the script:
   ```bash
   pm2 delete spaceships
   PILOTS_DB=/var/www/Gheat.net/spaceships/pilots.db PORT=4000 \
     pm2 start ./spaceships-server --name spaceships
   pm2 save
   ```
   `pm2 save` matters — without it the change is lost on the next resurrect.
4. `pm2 logs spaceships` to confirm it bound `:4000` and opened the database.

Rolling back is `pm2 delete spaceships && pm2 start server/index.js --name
spaceships && pm2 save`. Both servers
read the same database file, so no migration is involved in either direction.

## Notes

- Node is **v26.1.0**. The hand-rolled `WSConn` exists because of a `ws`
  regression on Node 25 — the `ws` package is still a dependency but unused.
- `.env` holds `JWT_SECRET`, and pm2 does **not** load it automatically — the
  running server picks it up because `server/index.js` reads it itself. If the
  Rust server replaces it, the secret has to be passed explicitly on the pm2
  command line or via `pm2 set`, or every existing token silently stops
  validating and all 19 accounts are logged out.
- `spaceshipADMIN.glb` is 4.9 MB and is fetched on every session regardless of
  ownership (`main.js:100`). Worth making conditional.
