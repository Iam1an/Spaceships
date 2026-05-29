import express from 'express';
import http from 'node:http';
import path from 'node:path';
import crypto from 'node:crypto';
import { fileURLToPath } from 'node:url';
import {
  registerPilot, loginPilot, verifyToken,
  recordGamePlayed, recordHighScore, savePilotColors,
  recordMatchResult, recordTrialTime, recordCampaignResult, getPilotProfile, getLeaderboard,
  getCredits, awardCredits, spendCredits, getCreditHistory,
  getCustomizationUnlocks, purchaseUnlock, UNLOCK_COSTS,
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

app.get('/api/profile/:username', (req, res) => {
  try {
    const profile = getPilotProfile(req.params.username);
    if (!profile) return res.status(404).json({ ok: false, error: 'Pilot not found' });
    res.json({ ok: true, profile });
  } catch (e) {
    res.status(500).json({ ok: false, error: e.message });
  }
});

app.get('/api/leaderboard', (_req, res) => {
  try {
    res.json({ ok: true, leaderboard: getLeaderboard() });
  } catch (e) {
    res.status(500).json({ ok: false, error: e.message });
  }
});

function extractPilotId(req) {
  const auth = req.headers.authorization || '';
  const token = auth.startsWith('Bearer ') ? auth.slice(7) : '';
  if (!token) throw Object.assign(new Error('Not authenticated'), { status: 401 });
  return verifyToken(token).id;
}

app.get('/api/unlocks', (req, res) => {
  try {
    const id = extractPilotId(req);
    res.json({ ok: true, costs: UNLOCK_COSTS, ...getCustomizationUnlocks(id) });
  } catch (e) {
    res.status(e.status ?? 401).json({ ok: false, error: e.message });
  }
});

app.post('/api/unlock/:feature', (req, res) => {
  try {
    const id     = extractPilotId(req);
    const result = purchaseUnlock(id, req.params.feature);
    if (!result.ok) return res.status(result.balance !== undefined ? 402 : 400).json({ ok: false, error: result.error, balance: result.balance });
    res.json({ ok: true, alreadyOwned: result.alreadyOwned, balance: result.balance, newAchievements: result.newAchievements ?? [] });
  } catch (e) {
    res.status(e.status ?? 401).json({ ok: false, error: e.message });
  }
});

app.get('/api/credits', (req, res) => {
  try {
    const id = extractPilotId(req);
    res.json({ ok: true, credits: getCredits(id) });
  } catch (e) {
    res.status(e.status ?? 401).json({ ok: false, error: e.message });
  }
});

app.get('/api/credits/history', (req, res) => {
  try {
    const id    = extractPilotId(req);
    const limit = Math.min(100, Math.max(1, Number(req.query.limit) || 50));
    res.json({ ok: true, history: getCreditHistory(id, limit) });
  } catch (e) {
    res.status(e.status ?? 401).json({ ok: false, error: e.message });
  }
});

app.post('/api/credits/spend', (req, res) => {
  try {
    const id     = extractPilotId(req);
    const amount = Number(req.body?.amount) || 0;
    const reason = String(req.body?.reason || 'purchase').slice(0, 120);
    if (amount < 1) return res.status(400).json({ ok: false, error: 'Invalid amount' });
    const result = spendCredits(id, amount, reason);
    if (!result.ok) return res.status(402).json({ ok: false, error: 'Insufficient credits', balance: result.balance });
    res.json({ ok: true, balance: result.balance });
  } catch (e) {
    res.status(e.status ?? 401).json({ ok: false, error: e.message });
  }
});

app.post('/api/solo-result', (req, res) => {
  try {
    const pilotId = extractPilotId(req);
    const { kills = 0, deaths = 0, won = null, botsKilled = 0 } = req.body ?? {};
    const result = recordMatchResult(pilotId, {
      kills:      Math.max(0, Number(kills)      || 0),
      deaths:     Math.max(0, Number(deaths)     || 0),
      won:        won === true || won === false ? won : null,
      botsKilled: Math.max(0, Number(botsKilled) || 0),
    });
    res.json({ ok: true, newAchievements: result.newAchievements, creditsEarned: result.creditsEarned, totalCredits: getCredits(pilotId) });
  } catch (e) {
    res.status(e.status ?? 500).json({ ok: false, error: e.message });
  }
});

app.post('/api/trial-result', (req, res) => {
  try {
    const pilotId = extractPilotId(req);
    const { trialNum, time } = req.body ?? {};
    const num = Number(trialNum);
    const t   = Number(time);
    if (![1, 2, 3, 4].includes(num) || !Number.isFinite(t) || t <= 0) {
      return res.status(400).json({ ok: false, error: 'Invalid trial data' });
    }
    const result = recordTrialTime(pilotId, num, t);
    res.json({ ok: true, newAchievements: result.newAchievements, creditsEarned: result.creditsEarned, totalCredits: getCredits(pilotId) });
  } catch (e) {
    res.status(e.status ?? 500).json({ ok: false, error: e.message });
  }
});

app.post('/api/campaign-result', (req, res) => {
  try {
    const pilotId = extractPilotId(req);
    const { missionNum, livesRemaining } = req.body ?? {};
    const num   = Number(missionNum);
    const lives = Number(livesRemaining);
    if (![1, 2, 3].includes(num) || !Number.isFinite(lives) || lives < 0 || lives > 3) {
      return res.status(400).json({ ok: false, error: 'Invalid campaign data' });
    }
    const result = recordCampaignResult(pilotId, num, lives);
    res.json({ ok: true, newAchievements: result.newAchievements, creditsEarned: result.creditsEarned, totalCredits: getCredits(pilotId) });
  } catch (e) {
    res.status(e.status ?? 500).json({ ok: false, error: e.message });
  }
});

// ─────────────────────────────────────────────────────────────────────────────

// During dev iteration we want every reload to fetch fresh assets so HTML/CSS
// edits show up without needing a hard refresh.
app.use(express.static(path.join(root, 'public'), {
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

      // FIX: Prevent OOM DoS by enforcing a maximum payload size (1MB)
      if (len > 1048576) {
        this.fail(new Error('Payload too large'));
        return;
      }

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
    const kills  = player.kills  || 0;
    const deaths = player.deaths || 0;
    let won = null;
    if (winner !== -1) won = player.team === winner;
    try {
      const result = recordMatchResult(ws.pilotId, { kills, deaths, won, botsKilled: 0 });
      if (ws.open) {
        const payload = { type: 'match-credits', creditsEarned: result.creditsEarned, totalCredits: getCredits(ws.pilotId) };
        if (result.newAchievements.length > 0) payload.earned = result.newAchievements;
        ws.send(JSON.stringify(payload));
      }
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

// Terrain map: airfields at z=±1500, spawn above runway (y=40).
const TERRAIN_TEAM_SPAWNS = [
  { z: -1400, y: 40, quat: [0, 0, 0, 1] },
  { z:  1400, y: 40, quat: [0, 1, 0, 0] },
];

function teamForId(id) {
  return id % 2;
}

function spawnForTeam(team, map = 'space') {
  if (map === 'terrain') {
    const t = TERRAIN_TEAM_SPAWNS[team];
    return {
      pos: [
        (Math.random() - 0.5) * 60,
        t.y + (Math.random() - 0.5) * 10,
        t.z + (Math.random() - 0.5) * 40,
      ],
      quat: t.quat,
    };
  }
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
  // If the bot host leaves, evict their bots — nobody drives them anymore.
  if (room.botHostId === ws.id) {
    for (const [id, p] of [...room.players]) {
      if (p.isBot) room.players.delete(id);
    }
    room.botHostId = null;
  }
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
      const isPrivate = !!msg.private;
      const map = msg.map === 'terrain' ? 'terrain' : 'space';
      const allowBot = msg.allowBot !== false; // default on
      const room = {
        code,
        hostId: ws.id,
        isPrivate,
        map,
        allowBot,
        sockets: new Set([ws]),
        players: new Map([[ws.id, { name: ws.name, hp: SHIP_MAX_HP, alive: true, kills: 0, deaths: 0 }]]),
        started: false,
      };
      rooms.set(code, room);
      ws.room = room;
      ws.send(JSON.stringify({ type: 'room', code, host: true, you: ws.id, private: isPrivate }));
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
      ws.send(JSON.stringify({ type: 'room', code: room.code, host: false, you: ws.id, private: room.isPrivate }));
      broadcastRoom(room);
      return;
    }

    if (msg.type === 'start') {
      const room = ws.room;
      if (!room || room.hostId !== ws.id) return;

      room.started = true;
      const spawns = {};
      let humanIdx = 0;
      for (const [id, p] of room.players) {
        p.hp = SHIP_MAX_HP;
        p.alive = true;
        if (p.team === undefined) {
          p.team = humanIdx++ % 2;
        } else {
          humanIdx++;
        }
        p.kills = 0;
        p.deaths = 0;
        p.invulnUntil = Date.now() + 2000;
        spawns[id] = { team: p.team, ...spawnForTeam(p.team, room.map) };
      }
      // Balance teams: add a hard-mode bot to the smaller team if uneven.
      const team0Count = [...room.players.values()].filter(p => p.team === 0).length;
      const team1Count = [...room.players.values()].filter(p => p.team === 1).length;
      const botAssignments = [];
      if (room.allowBot && team0Count !== team1Count) {
        const smallerTeam = team0Count < team1Count ? 0 : 1;
        const botId = -(nextId++);
        const sp = spawnForTeam(smallerTeam, room.map);
        room.players.set(botId, {
          name: 'Bot [Hard]',
          hp: SHIP_MAX_HP,
          alive: true,
          kills: 0,
          deaths: 0,
          team: smallerTeam,
          isBot: true,
          invulnUntil: Date.now() + 2000,
        });
        spawns[botId] = { team: smallerTeam, ...sp };
        room.botHostId = ws.id;
        botAssignments.push({ id: botId, team: smallerTeam, pos: sp.pos, quat: sp.quat });
      }
      room.asteroids = room.map === 'terrain' ? [] : generateAsteroidField(60, 400);
      room.matchOver = false;
      room.teamKills = [0, 0];
      room.matchEnd = Date.now() + MATCH_DURATION_MS;
      room.matchEndTimer = setTimeout(() => endMatch(room), MATCH_DURATION_MS);
      room.matchTickInterval = setInterval(() => {
        if (!room.started || room.matchOver) return;
        const remaining = Math.max(0, (room.matchEnd - Date.now()) / 1000);
        broadcast(room, { type: 'match-state', timer: remaining, teamKills: room.teamKills });
      }, 1000);
      broadcast(room, { type: 'start', spawns, asteroids: room.asteroids, map: room.map, botAssignments });
      broadcastRoom(room);
      return;
    }

    if (msg.type === 'bot-state') {
      const room = ws.room;
      if (!room || !room.started) return;
      if (ws.id !== room.botHostId) return;
      const bot = room.players.get(msg.botId);
      if (!bot || !bot.isBot) return;
      const out = JSON.stringify({
        type: 'state',
        id: msg.botId,
        pos: msg.pos,
        quat: msg.quat,
        boost: false,
      });
      for (const c of room.sockets) {
        if (c !== ws && c.open) c.send(out);
      }
      return;
    }

    if (msg.type === 'bot-fire') {
      const room = ws.room;
      if (!room || !room.started) return;
      if (ws.id !== room.botHostId) return;
      const bot = room.players.get(msg.botId);
      if (!bot || !bot.isBot || !bot.alive) return;
      const out = JSON.stringify({ type: 'fire', id: msg.botId, kind: 'bullet', shots: msg.shots });
      for (const c of room.sockets) {
        if (c !== ws && c.open) c.send(out);
      }
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
      // Prevent IP leaks by rejecting external URLs
      if (!modelUrl || modelUrl.startsWith('http://') || modelUrl.startsWith('https://') || modelUrl.includes('//')) return;
      
      // Optionally, to prevent admin ship unlock bypass, we could check getCustomizationUnlocks(ws.pilotId)
      // but restricting external URLs fixes the malicious IP leak.
      
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

    if (msg.type === 'flare') {
      const room = ws.room;
      if (!room || !room.started) return;
      const shooter = room.players.get(ws.id);
      if (!shooter || !shooter.alive) return;
      const out = JSON.stringify({ type: 'flare', id: ws.id, pos: msg.pos, quat: msg.quat });
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
          const sp = spawnForTeam(stillIn.team ?? 0, room.map);
          broadcast(room, { type: 'respawn', id: sid, pos: sp.pos, quat: sp.quat });
        }, RESPAWN_DELAY_MS);
      }
      return;
    }

    if (msg.type === 'hit') {
      const room = ws.room;
      if (!room || !room.started) return;
      const target = room.players.get(msg.targetId);
      if (!target || !target.alive) return;
      // Determine the effective shooter: either the sender or a bot they drive.
      let effectiveShooterId = ws.id;
      if (msg.fromBotId !== undefined && msg.fromBotId !== null) {
        if (ws.id !== room.botHostId) return; // only bot host may report bot hits
        const botShooter = room.players.get(msg.fromBotId);
        if (!botShooter || !botShooter.isBot || !botShooter.alive) return;
        effectiveShooterId = msg.fromBotId;
      }
      const shooter = room.players.get(effectiveShooterId);
      if (!shooter) return;
      // Guns (bullet/beam) require the shooter to be alive — you can't fire
      // after death. Missiles are already in-flight when they hit, so a
      // shooter who died mid-flight still gets credit and the hit lands.
      if (!shooter.alive && msg.kind !== 'missile') return;
      // No self-damage, but a bot driving the host can hit the host's ship.
      if (msg.targetId === ws.id && !msg.fromBotId) return;
      if (target.team !== undefined && target.team === shooter.team) return; // no friendly fire
      if (target.invulnUntil && Date.now() < target.invulnUntil) return; // spawn protection
      // Per-weapon damage. Buffed bullets to match beam at 10 per hit so
      // arrow-key pilots aren't punished for the beam's easier upkeep.
      const dmg = msg.kind === 'missile' ? 50 : 10;
      target.hp = Math.max(0, target.hp - dmg);
      broadcast(room, { type: 'hp', id: msg.targetId, hp: target.hp });
      if (target.hp === 0) {
        target.alive = false;
        target.deaths = (target.deaths || 0) + 1;
        shooter.kills = (shooter.kills || 0) + 1;
        if (room.teamKills && shooter.team !== undefined) {
          room.teamKills[shooter.team] = (room.teamKills[shooter.team] || 0) + 1;
        }
        broadcast(room, { type: 'death', id: msg.targetId, killerId: effectiveShooterId });
        broadcastRoom(room);
        const targetId = msg.targetId;
        setTimeout(() => {
          const stillIn = room.players.get(targetId);
          if (!stillIn || !room.started) return;
          stillIn.hp = SHIP_MAX_HP;
          stillIn.alive = true;
          stillIn.invulnUntil = Date.now() + 2000;
          const sp = spawnForTeam(stillIn.team ?? 0, room.map);
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
