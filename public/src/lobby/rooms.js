// Multiplayer: creating a room, browsing open rooms, joining by code, and the
// room screen itself. Also owns the lobby-side WebSocket message handlers.
import { el, esc, onClick } from './dom.js';
import { showScreen, setError, wireScreenNav } from './screens.js';
import { connect, send, disconnect, onLobbyMessage } from './net.js';
import { pilotName } from './pilot.js';
import { enterMultiplayerGame } from './launch.js';

const roomCodeEl = el('roomCode');
const playersEl = el('players');
const startBtn = el('btnStart');
const waitingEl = el('waitingForHost');
const codeInput = el('codeInput');

// Who we are in the room we are currently sitting in.
let myId = null;
let isHost = false;
let lastPlayers = [];

// ── Room browser ────────────────────────────────────────────────────────────
function renderRoomList(rooms) {
  const list = el('roomList');
  const empty = el('roomListEmpty');
  for (const node of list.querySelectorAll('.room-entry')) node.remove();
  empty.style.display = rooms.length === 0 ? '' : 'none';
  for (const room of rooms) {
    const entry = document.createElement('div');
    entry.className = 'room-entry';
    entry.innerHTML = `
        <div class="room-entry-info">
          <div class="room-entry-code">${esc(room.code)}</div>
          <div class="room-entry-meta">${esc(room.hostName)} · ${room.playerCount} player${room.playerCount !== 1 ? 's' : ''}</div>
        </div>
        <button class="btn-join-room" data-code="${esc(room.code)}">Join</button>
      `;
    entry.querySelector('.btn-join-room').addEventListener('click', () => {
      joinRoom(room.code);
    });
    list.appendChild(entry);
  }
}

async function joinRoom(code) {
  setError('Connecting…');
  try { await connect(); } catch (e) { setError(e.message); return; }
  send({ type: 'name', name: pilotName() });
  send({ type: 'join', code });
}

// ── Create-room options ─────────────────────────────────────────────────────
function isPrivateRoom() {
  return el('privacyPrivate')?.checked ?? false;
}

function selectedMap() {
  return el('mapTerrain')?.checked ? 'terrain' : 'space';
}

function autoBotEnabled() {
  return el('autoBotInput')?.checked ?? true;
}

// ── Incoming server messages ────────────────────────────────────────────────
onLobbyMessage('room', (msg) => {
  myId = msg.you;
  isHost = !!msg.host;
  roomCodeEl.textContent = msg.code;
  el('roomPrivacyBadge').textContent = msg.private ? 'PRIVATE' : 'OPEN';
  startBtn.classList.toggle('hidden', !isHost);
  waitingEl.classList.toggle('hidden', isHost);
  showScreen('room');
  setError('');
});

onLobbyMessage('players', (msg) => {
  lastPlayers = msg.players;
  playersEl.innerHTML = '';
  for (const p of msg.players) {
    const li = document.createElement('li');
    li.textContent = p.name + (p.id === myId ? ' (you)' : '') + (p.host ? ' — host' : '');
    playersEl.appendChild(li);
  }
});

onLobbyMessage('start', (msg) => {
  enterMultiplayerGame({
    you: myId,
    host: isHost,
    spawn: msg.spawns?.[myId] || null,
    asteroids: msg.asteroids || null,
    players: lastPlayers,
    map: msg.map || 'space',
    // Only the host runs the bots.
    botAssignments: isHost ? (msg.botAssignments || []) : [],
  });
});

onLobbyMessage('rooms-list', (msg) => renderRoomList(msg.rooms || []));

onLobbyMessage('error', (msg) => setError(msg.message || 'Error'));

// ── Screen wiring ───────────────────────────────────────────────────────────
wireScreenNav({
  btnMulti: 'multi',
  btnBackMulti: 'main',
  btnCreateMenu: 'create',
  btnBackCreate: 'multi',
  btnBackFind: 'multi',
});

onClick('btnPlay', async () => {
  setError('Connecting…');
  try { await connect(); } catch (e) { setError(e.message); return; }
  send({ type: 'name', name: pilotName() });
  send({ type: 'create', private: isPrivateRoom(), map: selectedMap(), allowBot: autoBotEnabled() });
});

onClick('btnFind', async () => {
  setError('');
  showScreen('find');
  codeInput.value = '';
  renderRoomList([]);
  try {
    await connect();
    send({ type: 'name', name: pilotName() });
    send({ type: 'list-rooms' });
  } catch (e) {
    setError(e.message);
  }
});

onClick('btnRefreshRooms', async () => {
  try {
    await connect();
    send({ type: 'list-rooms' });
  } catch (e) {
    setError(e.message);
  }
});

onClick('btnJoin', async () => {
  const code = codeInput.value.trim().toUpperCase();
  if (code.length !== 4) {
    setError('Code must be 4 letters');
    return;
  }
  await joinRoom(code);
});

codeInput.addEventListener('input', (e) => {
  e.target.value = e.target.value.toUpperCase().replace(/[^A-Z]/g, '').slice(0, 4);
});
codeInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') el('btnJoin').click();
});

onClick('btnStart', () => send({ type: 'start' }));

onClick('btnLeave', () => {
  send({ type: 'leave' });
  disconnect();
  showScreen('main');
});
