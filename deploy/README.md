# Deployment

Production is `gheat@100.81.137.100`, a git checkout at
`/var/www/Gheat.net/spaceships` on `main`.

## How requests reach the game

Caddy (`/etc/caddy/Caddyfile`) — nginx is installed but inactive.

```
redir /spaceships /spaceships/ 301
handle_path /spaceships/*  ->  reverse_proxy 127.0.0.1:4000
handle      /ws            ->  reverse_proxy 127.0.0.1:4000
```

**`handle_path` strips the matched prefix.** So `/spaceships/api/login` arrives
at the server as `/api/login`. The client calling `/spaceships/api/*` and the
server registering `/api/*` are *both* correct — they only appear to disagree on
a local machine, where there is no proxy in front. Nothing to fix.

Note the WebSocket is a separate `handle /ws`, **not** under `/spaceships/`, so
it is *not* prefix-stripped. A client deriving its socket URL from the page has
to account for that asymmetry.

**Anything serving this game must bind `127.0.0.1:4000`.**

## Process management: pm2, not systemd

The server runs under **pm2** as `spaceships` (id 0), and `pm2-root.service` is
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
