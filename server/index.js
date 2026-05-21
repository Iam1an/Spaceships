import express from 'express';
import http from 'node:http';
import path from 'node:path';
import crypto from 'node:crypto';
import { fileURLToPath } from 'node:url';
import {
  registerPilot, loginPilot, verifyToken,
  recordGamePlayed, recordHighScore, savePilotColors,
} from './db.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const root = path.join(__dirname, '..');

const app = express();
app.use(express.json());

// ── Auth API ─────────────────────────────────────────────────────────────────

app.post('/api/register', async (req, res) => {
  try {
    const { username, password } = req.body ?? {};
    const pilot = await registerPilot(username, password);
    res.status(201).json({ ok: true, username: pilot.username });
  } catch (e) {
    res.status(e.status ?? 500).json({ ok: false, error: e.message });
  }
});

app.post('/api/login', async (req, res) => {
  try {
    const { username, password } = req.body ?? {};
    const result = await loginPilot(username, password);
    res.json({ ok: true, ...result });
  } catch (e) {
    res.status(e.status ?? 500).json({ ok: false, error: e.message });
  }
});

app.put('/api/colors', (req, res) => {
  try {
    const auth = req.headers.authorization || '';
    const token = auth.startsWith('Bearer ') ? auth.slice(7) : '';
    if (!token) return res.status(401).json({ ok: false, error: 'Not authenticated' });
    const payload = verifyToken(token);
    const { shipColor, accentColor } = req.body ?? {};
    savePilotColors(payload.id, shipColor, accentColor);
    res.json({ ok: true });
  } catch (e) {
    res.status(e.status ?? 401).json({ ok: false, error: e.message });
  }
});

// ─────────────────────────────────────────────────────────────────────────────

// During dev iteration we want every reload to fetch fresh assets so HTML/CSS
// edits show up without needing a hard refresh.
app.use(express.static(root, {
  etag: false,
  lastModified: false,
  setHeaders: (res) => res.set('Cache-Control', 'no-store'),
}));

const server = http.createServer(app);

// --- Minimal RFC 6455 WebSocket server -------------------------------------
// Replaces the `ws` library because under Node 25 it mis-parses every
// incoming text frame as compressed (RSV1) and crashes the connection.
// Implements only the subset we need: unfragmented text/close/ping frames.
const WS_GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';

class WSConn {
  constructor(socket) {
    this.socket = socket;
    this.buffer = Buffer.alloc(0);
    this.listeners = { message: [], close: [], error: [] };
    this.open = true;

    // Node 25 TCP/upgrade regression: the same client frame bytes get
    // re-delivered on later `data` events, often with the original HTTP
    // request prepended and a mix of cumulative + partial replays. We can't
    // reason about chunk lengths reliably, so we dedup at the frame level
    // using each frame's random 4-byte mask.
    socket.on('data', (chunk) => {
      this.buffer = Buffer.concat([this.buffer, chunk]);
      try { this.parse(); } catch (e) { this.fail(e); }
    });
    // Two-tier rotating set — rotates after ~20k frames to bound memory
    // while still covering long sessions of dedup history.
    this._masksHot = new Set();
    this._masksCold = new Set();
    this._masksRotateAt = 20000;
    socket.on('close', () => { this.open = false; this.emit('close'); });
    socket.on('error', (e) => this.emit('error', e));
  }

  on(event, fn) {
    if (this.listeners[event]) this.listeners[event].push(fn);
  }
  emit(event, ...args) {
    for (const fn of this.listeners[event] || []) fn(...args);
  }

  parse() {
    // Node 25 regression: the http.Server `upgrade` event sometimes hands
    // us a `head` buffer that still contains the original HTTP request
    // bytes. If the buffer looks like the start of an ASCII request
    // method, skip past the end-of-headers marker so we only parse real
    // WebSocket frames after it.
    if (this.buffer.length >= 4) {
      const c = this.buffer[0];
      if (c >= 0x41 && c <= 0x5a) {
        const eoh = this.buffer.indexOf('\r\n\r\n');
        if (eoh >= 0) this.buffer = this.buffer.subarray(eoh + 4);
        else return;
      }
    }
    while (this.buffer.length >= 2) {
      const b0 = this.buffer[0];
      const b1 = this.buffer[1];
      const opcode = b0 & 0x0f;
      const masked = (b1 & 0x80) !== 0;
      let len = b1 & 0x7f;
      let off = 2;
      if (len === 126) {
        if (this.buffer.length < 4) return;
        len = this.buffer.readUInt16BE(2);
        off = 4;
      } else if (len === 127) {
        if (this.buffer.length < 10) return;
        const hi = this.buffer.readUInt32BE(2);
        const lo = this.buffer.readUInt32BE(6);
        len = hi * 0x100000000 + lo;
        off = 10;
      }
      let mask;
      let maskKey = null;
      if (masked) {
        if (this.buffer.length < off + 4) return;
        mask = this.buffer.subarray(off, off + 4);
        maskKey = mask.readUInt32BE(0);
        off += 4;
      }
      if (this.buffer.length < off + len) return;
      let payload = this.buffer.subarray(off, off + len);
      if (masked) {
        const out = Buffer.allocUnsafe(payload.length);
        for (let i = 0; i < payload.length; i++) out[i] = payload[i] ^ mask[i & 3];
        payload = out;
      }
      this.buffer = this.buffer.subarray(off + len);

      if (maskKey !== null) {
        const sig = maskKey * 0x10000 + (len & 0xffff);
        if (this._masksHot.has(sig) || this._masksCold.has(sig)) {
          continue;
        }
        this._masksHot.add(sig);
        if (this._masksHot.size >= this._masksRotateAt) {
          this._masksCold = this._masksHot;
          this._masksHot = new Set();
        }
      }

      if (opcode === 0x1 || opcode === 0x2) {
        this.emit('message', payload.toString('utf8'));
      } else if (opcode === 0x8) {
        this.close();
        return;
      } else if (opcode === 0x9) {
        this.sendFrame(payload, 0xa);
      }
    }
  }

  send(text) {
    this.sendFrame(Buffer.from(String(text), 'utf8'), 0x1);
  }

  sendFrame(payload, opcode) {
    if (!this.open) return;
    const len = payload.length;
    let header;
    if (len < 126) {
      header = Buffer.from([0x80 | opcode, len]);
    } else if (len < 0x10000) {
      header = Buffer.allocUnsafe(4);
      header[0] = 0x80 | opcode;
      header[1] = 126;
      header.writeUInt16BE(len, 2);
    } else {
      header = Buffer.allocUnsafe(10);
      header[0] = 0x80 | opcode;
      header[1] = 127;
      header.writeUInt32BE(0, 2);
      header.writeUInt32BE(len, 6);
    }
    try {
      this.socket.write(Buffer.concat([header, payload]));
    } catch {
      this.close();
    }
  }

  close() {
    if (!this.open) return;
    this.open = false;
    try { this.socket.end(); } catch {}
  }

  fail(err) {
    this.emit('error', err);
    this.close();
  }
}

server.on('upgrade', (req, socket, head) => {
  // Strip query string for path matching.
  const urlPath = req.url.split('?')[0];
  if (urlPath !== '/ws') {
    socket.destroy();
    return;
  }
  const key = req.headers['sec-websocket-key'];
  if (!key) {
    socket.destroy();
    return;
  }

  // Optional JWT auth — clients append ?token=<jwt> to the WS URL.
  // Guests (no token) are still allowed; ws.pilotId will be null for them.
  let pilotId = null;
  let pilotUsername = null;
  try {
    const qs = new URL(req.url, 'http://localhost').searchParams;
    const token = qs.get('token');
    if (token) {
      const payload = verifyToken(token);
      pilotId = payload.id;
      pilotUsername = payload.username;
    }
  } catch {
    // Expired or tampered token — treat as guest.
  }

  const accept = crypto.createHash('sha1').update(key + WS_GUID).digest('base64');
  // Disable Nagle so 20Hz state updates aren't coalesced into 100ms bursts.
  if (socket.setNoDelay) socket.setNoDelay(true);
  socket.write(
    'HTTP/1.1 101 Switching Protocols\r\n' +
    'Upgrade: websocket\r\n' +
    'Connection: Upgrade\r\n' +
    `Sec-WebSocket-Accept: ${accept}\r\n\r\n`
  );
  const ws = new WSConn(socket);
  ws.pilotId = pilotId;
  ws.pilotUsername = pilotUsername;
  if (head && head.length) {
    ws.buffer = head;
    try { ws.parse(); } catch (e) { ws.fail(e); }
  }
  handleConnection(ws);
});

// --- Lobby state -----------------------------------------------------------
const rooms = new Map();
let nextId = 1;

function genCode() {
  let code;
  do {
    code = '';
    for (let i = 0; i < 4; i++) {
      code += String.fromCharCode(65 + Math.floor(Math.random() * 26));
    }
  } while (rooms.has(code));
  return code;
}

const SHIP_MAX_HP = 100;
const RESPAWN_DELAY_MS = 2000;

function roomSnapshot(room) {
  return [...room.players.entries()].map(([id, p]) => ({
    id, name: p.name, host: id === room.hostId,
    team: p.team ?? null,
    isBot: !!p.isBot,
    kills: p.kills || 0, deaths: p.deaths || 0,
  }));
}

const MATCH_DURATION_MS = 300 * 1000;

function endMatch(room) {
  if (!room || room.matchOver) return;
  room.matchOver = true;
  if (room.matchTickInterval) {
    clearInterval(room.matchTickInterval);
    room.matchTickInterval = null;
  }
  if (room.matchEndTimer) {
    clearTimeout(room.matchEndTimer);
    room.matchEndTimer = null;
  }
  const [k0, k1] = room.teamKills || [0, 0];
  let winner = -1;
  if (k0 > k1) winner = 0;
  else if (k1 > k0) winner = 1;
  broadcast(room, { type: 'match-end', winner, teamKills: room.teamKills });

  // Persist stats for every authenticated pilot in this room.
  for (const ws of room.sockets) {
    if (!ws.pilotId) continue;
    const player = room.players.get(ws.id);
    if (!player || player.isBot) continue;
    const kills = player.kills || 0;
    try {
      recordGamePlayed(ws.pilotId);
      recordHighScore(ws.pilotId, kills);
    } catch (e) {
      console.warn(`stats save failed for pilot ${ws.pilotId}:`, e.message);
    }
  }
}

// Two motherships face each other across the field. Team 0 home at z=-540
// (mothership at -600, hangar mouth ~-565). Team 1 mirror at z=+540.
const TEAM_SPAWNS = [
  { z: -540, quat: [0, 0, 0, 1] },          // team 0: facing +Z
  { z: +540, quat: [0, 1, 0, 0] },          // team 1: 180° around Y, facing -Z
];

function teamForId(id) {
  return id % 2;
}

function spawnForTeam(team) {
  const t = TEAM_SPAWNS[team];
  return {
    pos: [
      (Math.random() - 0.5) * 8,
      (Math.random() - 0.5) * 4,
      t.z + (Math.random() - 0.5) * 6,
    ],
    quat: t.quat,
  };
}

// Bots spawn with much wider jitter than humans so a full squad doesn't
// clump on top of itself at the mothership — matches the solo skirmish
// spread (±40 / ±15 / ±40). Humans still spawn tightly at the hangar.
function spawnForBot(team) {
  const t = TEAM_SPAWNS[team];
  return {
    pos: [
      (Math.random() - 0.5) * 80,
      (Math.random() - 0.5) * 30,
      t.z + (Math.random() - 0.5) * 80,
    ],
    quat: t.quat,
  };
}

// --- Asteroid field generation (server-authoritative) -----------------
// We generate the layout once per game so all clients agree on positions,
// sizes, and HP. HP is mutated on the server when bullets report hits.
const ASTEROID_TIERS = [
  { name: 'small',  minSize: 5,  maxSize: 7,  hp: 5,  weight: 0.45 },
  { name: 'medium', minSize: 9,  maxSize: 15, hp: 10, weight: 0.30 },
  { name: 'big',    minSize: 18, maxSize: 30, hp: 30, weight: 0.18 },
  { name: 'huge',   minSize: 38, maxSize: 55, hp: 50, weight: 0.07 },
];
const MOTHERSHIP_AVOID = [
  { pos: [0, 0, -600], halfSize: [45, 18, 35] },
  { pos: [0, 0,  600], halfSize: [45, 18, 35] },
];

function pickAsteroidTier() {
  const r = Math.random();
  let acc = 0;
  for (const t of ASTEROID_TIERS) {
    acc += t.weight;
    if (r < acc) return t;
  }
  return ASTEROID_TIERS[ASTEROID_TIERS.length - 1];
}

function clipsMothership(x, y, z, asteroidRadius) {
  for (const m of MOTHERSHIP_AVOID) {
    const margin = asteroidRadius + 6;
    if (
      Math.abs(x - m.pos[0]) < m.halfSize[0] + margin &&
      Math.abs(y - m.pos[1]) < m.halfSize[1] + margin &&
      Math.abs(z - m.pos[2]) < m.halfSize[2] + margin
    ) return true;
  }
  return false;
}

function generateAsteroidField(count = 60, radius = 400) {
  const list = [];
  for (let i = 0; i < count; i++) {
    const tier = pickAsteroidTier();
    const size = tier.minSize + Math.random() * (tier.maxSize - tier.minSize);
    let pos = null;
    for (let attempt = 0; attempt < 10 && !pos; attempt++) {
      let dx = Math.random() * 2 - 1;
      let dy = (Math.random() * 2 - 1) * 0.4;
      let dz = Math.random() * 2 - 1;
      const len = Math.sqrt(dx * dx + dy * dy + dz * dz) || 1;
      dx /= len; dy /= len; dz /= len;
      const minDist = 30 + size;
      const dist = minDist + Math.random() * (radius - minDist);
      const cx = dx * dist, cy = dy * dist, cz = dz * dist;
      if (!clipsMothership(cx, cy, cz, size)) pos = [cx, cy, cz];
    }
    if (!pos) {
      pos = [(Math.random() - 0.5) * radius, 0, (Math.random() - 0.5) * radius];
    }
    list.push({
      id: i,
      pos,
      rot: [Math.random() * Math.PI, Math.random() * Math.PI, Math.random() * Math.PI],
      size,
      hp: tier.hp,
      tier: tier.name,
      variant: Math.floor(Math.random() * 6),
      spin: [Math.random() - 0.5, Math.random() - 0.5, Math.random() - 0.5],
    });
  }
  return list;
}

function broadcast(room, payload) {
  const msg = JSON.stringify(payload);
  for (const c of room.sockets) {
    if (c.open) c.send(msg);
  }
}

function broadcastRoom(room) {
  const msg = JSON.stringify({ type: 'players', players: roomSnapshot(room) });
  for (const ws of room.sockets) {
    if (ws.open) ws.send(msg);
  }
}

function leaveRoom(ws) {
  const room = ws.room;
  if (!room) return;
  room.sockets.delete(ws);
  room.players.delete(ws.id);
  ws.room = null;
  // Count humans separately from bots — a room of only bots has nobody
  // to listen to broadcasts, so it should be torn down.
  let nextHostId = null;
  for (const [id, p] of room.players) {
    if (!p.isBot) { nextHostId = id; break; }
  }
  if (nextHostId === null) {
    if (room.matchEndTimer) clearTimeout(room.matchEndTimer);
    if (room.matchTickInterval) clearInterval(room.matchTickInterval);
    rooms.delete(room.code);
    return;
  }
  if (ws.id === room.hostId) {
    room.hostId = nextHostId;
  }
  broadcastRoom(room);
}

function handleConnection(ws) {
  ws.id = nextId++;
  // Use the authenticated pilot's callsign as the default name when logged in.
  ws.name = ws.pilotUsername || ('Player ' + ws.id);
  ws.room = null;

  ws.on('error', (err) => {
    // Suppress benign disconnect noise — broken pipes / dead hosts are
    // expected when a client navigates away mid-broadcast. The close
    // handler will run shortly and clean the player up.
    const transient = new Set(['EPIPE', 'EHOSTDOWN', 'ECONNRESET', 'ETIMEDOUT', 'ENETUNREACH']);
    if (transient.has(err.code)) return;
    console.warn(`ws ${ws.id} error:`, err.message);
  });

  ws.on('message', (data) => {
    let msg;
    try { msg = JSON.parse(data); } catch { return; }

    if (msg.type === 'name') {
      const raw = typeof msg.name === 'string' ? msg.name : '';
      const cleaned = raw.replace(/[^A-Za-z0-9 _\-]/g, '').trim().slice(0, 16);
      if (cleaned) {
        ws.name = cleaned;
        if (ws.room) {
          const p = ws.room.players.get(ws.id);
          if (p) p.name = cleaned;
          broadcastRoom(ws.room);
        }
      }
      return;
    }

    if (msg.type === 'list-rooms') {
      const list = [];
      for (const [code, room] of rooms) {
        if (!room.isPrivate && !room.started) {
          const humanCount = [...room.players.values()].filter(p => !p.isBot).length;
          list.push({
            code,
            mode: room.mode,
            playerCount: humanCount,
            hostName: room.players.get(room.hostId)?.name || 'Unknown',
          });
        }
      }
      ws.send(JSON.stringify({ type: 'rooms-list', rooms: list }));
      return;
    }

    if (msg.type === 'create') {
      if (ws.room) leaveRoom(ws);
      const code = genCode();
      const mode = msg.mode === '5v5' ? '5v5' : 'normal';
      const isPrivate = !!msg.private;
      const room = {
        code,
        hostId: ws.id,
        mode,
        isPrivate,
        sockets: new Set([ws]),
        players: new Map([[ws.id, { name: ws.name, hp: SHIP_MAX_HP, alive: true, kills: 0, deaths: 0 }]]),
        started: false,
      };
      rooms.set(code, room);
      ws.room = room;
      ws.send(JSON.stringify({ type: 'room', code, host: true, you: ws.id, mode, private: isPrivate }));
      broadcastRoom(room);
      return;
    }

    if (msg.type === 'join') {
      const code = String(msg.code || '').toUpperCase();
      const room = rooms.get(code);
      if (!room) {
        ws.send(JSON.stringify({ type: 'error', message: 'Room not found' }));
        return;
      }
      if (room.started) {
        ws.send(JSON.stringify({ type: 'error', message: 'Game already started' }));
        return;
      }
      if (ws.room) leaveRoom(ws);
      room.sockets.add(ws);
      room.players.set(ws.id, { name: ws.name, hp: SHIP_MAX_HP, alive: true, kills: 0, deaths: 0 });
      ws.room = room;
      ws.send(JSON.stringify({ type: 'room', code: room.code, host: false, you: ws.id, mode: room.mode || 'normal', private: room.isPrivate }));
      broadcastRoom(room);
      return;
    }

    if (msg.type === 'start') {
      const room = ws.room;
      if (!room || room.hostId !== ws.id) return;

      // 5v5 mode requires exactly 2 humans (one per team), then pads with
      // 4 bot players per team. Bots live in room.players just like
      // humans — HP, kills, friendly-fire all go through the same code.
      if (room.mode === '5v5') {
        const humans = [...room.players.keys()];
        if (humans.length < 2) {
          ws.send(JSON.stringify({ type: 'error', message: 'Need 2 players for 5v5' }));
          return;
        }
        // Assign humans to opposing teams: first joiner (host) = 0, other = 1.
        const teamAssignments = new Map();
        teamAssignments.set(humans[0], 0);
        teamAssignments.set(humans[1], 1);
        for (const [id, p] of room.players) {
          p.team = teamAssignments.get(id) ?? 0;
        }
        // Bot players. IDs in a high range so they never collide with ws.ids.
        // Names are team-stable (Red=team 0, Blue=team 1) so both clients
        // read the same identity regardless of their own team.
        room.botIds = [];
        let nextBotId = 100000;
        for (let i = 0; i < 4; i++) {
          const id = nextBotId++;
          room.players.set(id, {
            name: `Red Bot ${i + 1}`, isBot: true, team: 0,
            hp: SHIP_MAX_HP, alive: true, kills: 0, deaths: 0,
          });
          room.botIds.push(id);
        }
        for (let i = 0; i < 4; i++) {
          const id = nextBotId++;
          room.players.set(id, {
            name: `Blue Bot ${i + 1}`, isBot: true, team: 1,
            hp: SHIP_MAX_HP, alive: true, kills: 0, deaths: 0,
          });
          room.botIds.push(id);
        }
      }

      room.started = true;
      const spawns = {};
      const bots = [];
      // Track arrival order so non-5v5 rooms still get alternating
      // teams (id % 2 could put both players on the same side and
      // friendly-fire would block all damage).
      let humanIdx = 0;
      for (const [id, p] of room.players) {
        p.hp = SHIP_MAX_HP;
        p.alive = true;
        if (p.team === undefined) {
          p.team = p.isBot ? 0 : (humanIdx++ % 2);
        } else if (!p.isBot) {
          humanIdx++;
        }
        p.kills = 0;
        p.deaths = 0;
        p.invulnUntil = Date.now() + 2000;
        const sp = { team: p.team, ...(p.isBot ? spawnForBot(p.team) : spawnForTeam(p.team)) };
        spawns[id] = sp;
        if (p.isBot) bots.push({ id, name: p.name, team: p.team, spawn: sp });
      }
      room.asteroids = generateAsteroidField(60, 400);
      // 5v5 match: 2-minute timer, team with most kills wins.
      room.matchOver = false;
      room.teamKills = [0, 0];
      room.matchEnd = Date.now() + MATCH_DURATION_MS;
      room.matchEndTimer = setTimeout(() => endMatch(room), MATCH_DURATION_MS);
      room.matchTickInterval = setInterval(() => {
        if (!room.started || room.matchOver) return;
        const remaining = Math.max(0, (room.matchEnd - Date.now()) / 1000);
        broadcast(room, { type: 'match-state', timer: remaining, teamKills: room.teamKills });
      }, 1000);
      broadcast(room, { type: 'start', spawns, asteroids: room.asteroids, bots });
      broadcastRoom(room);
      return;
    }

    if (msg.type === 'asteroid-hit') {
      const room = ws.room;
      if (!room || !room.started || !room.asteroids) return;
      const a = room.asteroids.find((x) => x.id === msg.id);
      if (!a || a.hp <= 0) return;
      a.hp = Math.max(0, a.hp - 1);
      if (a.hp === 0) {
        broadcast(room, { type: 'asteroid-destroyed', id: a.id });
      } else {
        broadcast(room, { type: 'asteroid-hp', id: a.id, hp: a.hp });
      }
      return;
    }

    if (msg.type === 'colors') {
      const room = ws.room;
      if (!room) return;
      const out = JSON.stringify({ type: 'colors', id: ws.id, hullColor: msg.hullColor, accentColor: msg.accentColor });
      for (const c of room.sockets) {
        if (c !== ws && c.open) c.send(out);
      }
      return;
    }

    if (msg.type === 'ship-model') {
      const room = ws.room;
      if (!room) return;
      const modelUrl = typeof msg.modelUrl === 'string' ? msg.modelUrl : null;
      if (!modelUrl) return;
      const out = JSON.stringify({ type: 'ship-model', id: ws.id, modelUrl });
      for (const c of room.sockets) {
        if (c !== ws && c.open) c.send(out);
      }
      return;
    }

    if (msg.type === 'fire') {
      const room = ws.room;
      if (!room || !room.started) return;
      const shooter = room.players.get(ws.id);
      if (!shooter || !shooter.alive) return;
      const out = JSON.stringify({ type: 'fire', id: ws.id, kind: msg.kind, shots: msg.shots });
      for (const c of room.sockets) {
        if (c !== ws && c.open) c.send(out);
      }
      return;
    }

    if (msg.type === 'self-damage') {
      const room = ws.room;
      if (!room || !room.started) return;
      const self = room.players.get(ws.id);
      if (!self || !self.alive) return;
      const dmg = Math.max(0, Math.min(SHIP_MAX_HP, Number(msg.dmg) || 0));
      if (dmg <= 0) return;
      self.hp = Math.max(0, self.hp - dmg);
      broadcast(room, { type: 'hp', id: ws.id, hp: self.hp });
      if (self.hp === 0) {
        self.alive = false;
        self.deaths = (self.deaths || 0) + 1;
        broadcast(room, { type: 'death', id: ws.id, killerId: null });
        broadcastRoom(room);
        const sid = ws.id;
        setTimeout(() => {
          const stillIn = room.players.get(sid);
          if (!stillIn || !room.started) return;
          stillIn.hp = SHIP_MAX_HP;
          stillIn.alive = true;
          stillIn.invulnUntil = Date.now() + 2000;
          const sp = stillIn.isBot ? spawnForBot(stillIn.team ?? 0) : spawnForTeam(stillIn.team ?? 0);
          broadcast(room, { type: 'respawn', id: sid, pos: sp.pos, quat: sp.quat });
        }, RESPAWN_DELAY_MS);
      }
      return;
    }

    if (msg.type === 'hit') {
      const room = ws.room;
      if (!room || !room.started) return;
      const target = room.players.get(msg.targetId);
      const shooter = room.players.get(ws.id);
      if (!target || !target.alive) return;
      if (!shooter || !shooter.alive) return;       // dead players can't damage
      if (msg.targetId === ws.id) return;           // no self-damage
      if (target.team !== undefined && target.team === shooter.team) return; // no friendly fire
      if (target.invulnUntil && Date.now() < target.invulnUntil) return; // spawn protection
      // Per-weapon damage. Buffed bullets to match beam at 10 per hit so
      // arrow-key pilots aren't punished for the beam's easier upkeep.
      const dmg = msg.kind === 'beam' ? 10 : 10;
      target.hp = Math.max(0, target.hp - dmg);
      broadcast(room, { type: 'hp', id: msg.targetId, hp: target.hp });
      if (target.hp === 0) {
        target.alive = false;
        target.deaths = (target.deaths || 0) + 1;
        shooter.kills = (shooter.kills || 0) + 1;
        if (room.teamKills && shooter.team !== undefined) {
          room.teamKills[shooter.team] = (room.teamKills[shooter.team] || 0) + 1;
        }
        broadcast(room, { type: 'death', id: msg.targetId, killerId: ws.id });
        broadcastRoom(room);
        const targetId = msg.targetId;
        setTimeout(() => {
          const stillIn = room.players.get(targetId);
          if (!stillIn || !room.started) return;
          stillIn.hp = SHIP_MAX_HP;
          stillIn.alive = true;
          stillIn.invulnUntil = Date.now() + 2000;
          const sp = stillIn.isBot ? spawnForBot(stillIn.team ?? 0) : spawnForTeam(stillIn.team ?? 0);
          broadcast(room, { type: 'respawn', id: targetId, pos: sp.pos, quat: sp.quat });
        }, RESPAWN_DELAY_MS);
      }
      return;
    }

    if (msg.type === 'leave') {
      leaveRoom(ws);
      return;
    }

    if (msg.type === 'state') {
      const room = ws.room;
      if (!room || !room.started) return;
      const out = JSON.stringify({
        type: 'state',
        id: ws.id,
        pos: msg.pos,
        quat: msg.quat,
        boost: !!msg.boost,
      });
      for (const c of room.sockets) {
        if (c !== ws && c.open) c.send(out);
      }
      return;
    }

    // ---- 5v5 bot relays. Host owns bot AI; we just gate by host id and
    // forward to other clients (or apply damage in the bot-hit case). ----
    if (msg.type === 'bot-state') {
      const room = ws.room;
      if (!room || !room.started || room.hostId !== ws.id) return;
      // Batch payload: [{id, pos, quat, boost, alive}, ...]
      const out = JSON.stringify({ type: 'bot-state', bots: msg.bots });
      for (const c of room.sockets) {
        if (c !== ws && c.open) c.send(out);
      }
      return;
    }

    if (msg.type === 'bot-fire') {
      const room = ws.room;
      if (!room || !room.started || room.hostId !== ws.id) return;
      const out = JSON.stringify({
        type: 'bot-fire', botId: msg.botId, kind: msg.kind, shots: msg.shots,
      });
      for (const c of room.sockets) {
        if (c !== ws && c.open) c.send(out);
      }
      return;
    }

    if (msg.type === 'bot-hit') {
      const room = ws.room;
      if (!room || !room.started || room.hostId !== ws.id) return;
      const shooter = room.players.get(msg.botId);
      const target = room.players.get(msg.targetId);
      if (!shooter || !shooter.isBot) return;
      if (!target || !target.alive) return;
      if (msg.botId === msg.targetId) return;
      if (shooter.team !== undefined && shooter.team === target.team) return;
      if (target.invulnUntil && Date.now() < target.invulnUntil) return;
      const dmg = msg.kind === 'beam' ? 10 : 10;
      target.hp = Math.max(0, target.hp - dmg);
      broadcast(room, { type: 'hp', id: msg.targetId, hp: target.hp });
      if (target.hp === 0) {
        target.alive = false;
        target.deaths = (target.deaths || 0) + 1;
        shooter.kills = (shooter.kills || 0) + 1;
        if (room.teamKills && shooter.team !== undefined) {
          room.teamKills[shooter.team] = (room.teamKills[shooter.team] || 0) + 1;
        }
        broadcast(room, { type: 'death', id: msg.targetId, killerId: msg.botId });
        broadcastRoom(room);
        const targetId = msg.targetId;
        setTimeout(() => {
          const stillIn = room.players.get(targetId);
          if (!stillIn || !room.started) return;
          stillIn.hp = SHIP_MAX_HP;
          stillIn.alive = true;
          stillIn.invulnUntil = Date.now() + 2000;
          const sp = stillIn.isBot ? spawnForBot(stillIn.team ?? 0) : spawnForTeam(stillIn.team ?? 0);
          broadcast(room, { type: 'respawn', id: targetId, pos: sp.pos, quat: sp.quat });
        }, RESPAWN_DELAY_MS);
      }
      return;
    }
  });

  ws.on('close', () => {
    const room = ws.room;
    const wasStarted = room && room.started;
    const id = ws.id;
    leaveRoom(ws);
    if (wasStarted && room) {
      const out = JSON.stringify({ type: 'disconnect', id });
      for (const c of room.sockets) {
        if (c.open) c.send(out);
      }
    }
  });
}

const PORT = process.env.PORT || 4000;
server.listen(PORT, () => {
  console.log(`Spaceships server listening on port ${PORT}`);
  console.log(`Local:  http://localhost:${PORT}`);
  console.log(`LAN:    http://<your-ip>:${PORT}`);
});
