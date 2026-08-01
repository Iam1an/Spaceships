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

## Install the service

The server has been running as a bare `node server/index.js` — no unit, so a
crash or a reboot took the game down until someone noticed. `spaceships.service`
fixes that, following the same pattern as `paintballer.service`.

Needs sudo on the host:

```bash
scp deploy/spaceships.service gheat@100.81.137.100:/tmp/
ssh gheat@100.81.137.100
sudo install -m 644 /tmp/spaceships.service /etc/systemd/system/
sudo systemctl daemon-reload

# The manual process is holding :4000, so stop it before starting the unit or
# the unit will fail to bind. Find it with:
ss -lptn 'sport = :4000'
kill <pid>

sudo systemctl enable --now spaceships
systemctl status spaceships
journalctl -u spaceships -n 50 --no-pager
```

Expect a few seconds of downtime between the `kill` and `systemctl enable
--now`. There is no way to avoid it while a single port is involved.

## Deploying a change

```bash
ssh gheat@100.81.137.100
cd /var/www/Gheat.net/spaceships
git pull
npm ci                      # three and vite are runtime deps, not dev deps
npm run build               # writes dist/; the server prefers it over public/
sudo systemctl restart spaceships
```

`npm start` also runs the build via `prestart`, but under systemd the unit
invokes `node` directly, so the build has to be an explicit deploy step.

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
3. Point `ExecStart=` at the binary and drop the `node` dependency:
   ```
   ExecStart=/var/www/Gheat.net/spaceships/spaceships-server
   Environment=PILOTS_DB=/var/www/Gheat.net/spaceships/pilots.db
   ```
4. `sudo systemctl daemon-reload && sudo systemctl restart spaceships`

Rolling back is putting the old `ExecStart=` back and restarting. Both servers
read the same database file, so no migration is involved in either direction.

## Notes

- Node is **v26.1.0**. The hand-rolled `WSConn` exists because of a `ws`
  regression on Node 25 — the `ws` package is still a dependency but unused.
- `.env` holds `JWT_SECRET`. The unit loads it with `EnvironmentFile=` and no
  leading `-`, so a missing file stops the service rather than silently falling
  back to the dev default, which would invalidate every existing token.
- `spaceshipADMIN.glb` is 4.9 MB and is fetched on every session regardless of
  ownership (`main.js:100`). Worth making conditional.
