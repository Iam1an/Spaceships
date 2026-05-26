import { startGame } from './main.js';
import { requireAuth, getToken, clearToken } from './auth.js';
import { getSavedShipColor, getSavedAccentColor, getSavedTrailColor, getSavedTrailShape, hslToHex, hexToHsl, initCustomizationScene } from './customization.js';
import { containsProfanity } from './filter.js';

// Lobby state machine: main → find → room. Talks to the server over a single
// long-lived WebSocket. Once the host hits Start (or a non-host receives the
// `start` broadcast), we hide the overlay and hand the socket to startGame()
// — gameplay sync messages will reuse it next iteration.

const lobbyEl = document.getElementById('lobby');
const screens = {
  main: document.getElementById('lobby-main'),
  multi: document.getElementById('lobby-multi'),
  create: document.getElementById('lobby-create'),
  find: document.getElementById('lobby-find'),
  room: document.getElementById('lobby-room'),
  single: document.getElementById('lobby-single'),
  tutorial: document.getElementById('lobby-tutorial'),
  trials: document.getElementById('lobby-trials'),
};

const roomCodeEl = document.getElementById('roomCode');
const playersEl = document.getElementById('players');
const startBtn = document.getElementById('btnStart');
const waitingEl = document.getElementById('waitingForHost');
const errorEl = document.getElementById('lobby-error');
const codeInput = document.getElementById('codeInput');

let ws = null;
let myId = null;
let isHost = false;
let mySpawn = null;
let myAsteroids = null;
let lastPlayers = [];

const nameInput = document.getElementById('nameInput');
const SAVED_NAME_KEY = 'spaceships:pilotName';
nameInput.value = localStorage.getItem(SAVED_NAME_KEY) || '';
nameInput.addEventListener('input', () => {
  const cleaned = nameInput.value.replace(/[^A-Za-z0-9 _\-]/g, '').slice(0, 16);
  if (cleaned !== nameInput.value) nameInput.value = cleaned;
  if (containsProfanity(cleaned)) {
    nameInput.style.borderColor = '#ff5566';
    nameInput.title = 'That name is not allowed';
    return;
  }
  nameInput.style.borderColor = '';
  nameInput.title = '';
  localStorage.setItem(SAVED_NAME_KEY, cleaned);
});

// Control scheme: one of 'mouse_keys' | 'keyboard' | 'mobile'.
// Replaces the older spaceships:noMouse boolean. We migrate any legacy value
// on first read so saved preferences carry over. Default for fresh installs
// is mobile-aware: touchscreens get 'mobile', everything else 'mouse_keys'.
const SCHEMES = ['mouse_keys', 'keyboard', 'mobile'];
const SAVED_SCHEME_KEY = 'spaceships:controlScheme';
const SAVED_NO_MOUSE_KEY = 'spaceships:noMouse';
function detectDefaultScheme() {
  const touch = 'ontouchstart' in window || navigator.maxTouchPoints > 0;
  // Heuristic: real phones/tablets are both touch and lack hover.
  const coarse = window.matchMedia?.('(pointer: coarse)')?.matches;
  return touch && coarse ? 'mobile' : 'mouse_keys';
}
let selectedScheme = localStorage.getItem(SAVED_SCHEME_KEY);
if (!SCHEMES.includes(selectedScheme)) {
  // Migrate from the old checkbox: noMouse=1 → keyboard, else default.
  selectedScheme = localStorage.getItem(SAVED_NO_MOUSE_KEY) === '1'
    ? 'keyboard'
    : detectDefaultScheme();
  localStorage.setItem(SAVED_SCHEME_KEY, selectedScheme);
}
function setScheme(s) {
  if (!SCHEMES.includes(s)) return;
  selectedScheme = s;
  localStorage.setItem(SAVED_SCHEME_KEY, s);
  for (const picker of document.querySelectorAll('[data-scheme-picker]')) {
    for (const btn of picker.querySelectorAll('button[data-scheme]')) {
      btn.classList.toggle('selected', btn.dataset.scheme === s);
    }
  }
}
for (const picker of document.querySelectorAll('[data-scheme-picker]')) {
  picker.addEventListener('click', (e) => {
    const btn = e.target.closest('button[data-scheme]');
    if (btn) setScheme(btn.dataset.scheme);
  });
}
setScheme(selectedScheme);

// Settings gear (top-right): opens a panel with the Secret Hard Mode
// toggle. Persisted in localStorage so it survives reloads. Closes on
// outside click. The gear is hidden once the HUD shows so it doesn't
// float over gameplay.
const settingsBtn = document.getElementById('settingsBtn');
const settingsPanel = document.getElementById('settingsPanel');
const hardModeInput = document.getElementById('hardModeInput');
const SAVED_HARD_MODE_KEY = 'spaceships:hardMode';
hardModeInput.checked = localStorage.getItem(SAVED_HARD_MODE_KEY) === '1';
hardModeInput.addEventListener('change', () => {
  localStorage.setItem(SAVED_HARD_MODE_KEY, hardModeInput.checked ? '1' : '0');
});

// Retro pixel filter: default on (the user opted into the look). Stored
// as the inverse — '0' for off — so a fresh install gets the filter.
const pixelFilterInput = document.getElementById('pixelFilterInput');
const SAVED_PIXEL_KEY = 'spaceships:pixelFilter';
pixelFilterInput.checked = localStorage.getItem(SAVED_PIXEL_KEY) !== '0';
pixelFilterInput.addEventListener('change', () => {
  localStorage.setItem(SAVED_PIXEL_KEY, pixelFilterInput.checked ? '1' : '0');
});

// Enemy trails: default on. Stored inverted so '0' means off and fresh
// installs render trails for all ships.
const enemyTrailsInput = document.getElementById('enemyTrailsInput');
const SAVED_ENEMY_TRAILS = 'spaceships:enemyTrails';
enemyTrailsInput.checked = localStorage.getItem(SAVED_ENEMY_TRAILS) !== '0';
enemyTrailsInput.addEventListener('change', () => {
  localStorage.setItem(SAVED_ENEMY_TRAILS, enemyTrailsInput.checked ? '1' : '0');
});

// Volume sliders. Values stored as 0–1 in localStorage; UI is 0–100.
// Changes apply live to the running game via `window.__shipAudio` (set
// by main.js on startGame) so the gear panel works mid-match.
const musicSlider = document.getElementById('musicVolumeInput');
const sfxSlider = document.getElementById('sfxVolumeInput');
const musicVal = document.getElementById('musicVolumeVal');
const sfxVal = document.getElementById('sfxVolumeVal');
const SAVED_MUSIC_VOL = 'spaceships:musicVolume';
const SAVED_SFX_VOL = 'spaceships:sfxVolume';
function loadVol(key, fallback) {
  const v = parseFloat(localStorage.getItem(key));
  return Number.isFinite(v) ? Math.max(0, Math.min(1, v)) : fallback;
}
const initialMusic = loadVol(SAVED_MUSIC_VOL, 0.6);
const initialSfx = loadVol(SAVED_SFX_VOL, 1.0);
musicSlider.value = Math.round(initialMusic * 100);
sfxSlider.value = Math.round(initialSfx * 100);
musicVal.textContent = musicSlider.value;
sfxVal.textContent = sfxSlider.value;
function applyMusic() {
  const v = musicSlider.value / 100;
  musicVal.textContent = musicSlider.value;
  localStorage.setItem(SAVED_MUSIC_VOL, v.toFixed(3));
  if (window.__shipAudio?.setMusicVolume) window.__shipAudio.setMusicVolume(v);
}
function applySfx() {
  const v = sfxSlider.value / 100;
  sfxVal.textContent = sfxSlider.value;
  localStorage.setItem(SAVED_SFX_VOL, v.toFixed(3));
  if (window.__shipAudio?.setSfxVolume) window.__shipAudio.setSfxVolume(v);
}
musicSlider.addEventListener('input', applyMusic);
sfxSlider.addEventListener('input', applySfx);
settingsBtn.addEventListener('click', (e) => {
  e.stopPropagation();
  settingsPanel.classList.toggle('hidden');
});
document.addEventListener('click', (e) => {
  if (settingsPanel.classList.contains('hidden')) return;
  if (settingsPanel.contains(e.target) || settingsBtn.contains(e.target)) return;
  settingsPanel.classList.add('hidden');
});

function pilotName() {
  const n = (nameInput.value || '').trim();
  if (!n || containsProfanity(n)) return localStorage.getItem(SAVED_NAME_KEY) || 'Pilot';
  return n;
}
function controlScheme() {
  return selectedScheme;
}
// Back-compat shim for the existing main.js branch — keyboard is the only
// scheme that fully suppresses the mouse.
function noMouse() {
  return selectedScheme === 'keyboard';
}

function showScreen(name) {
  for (const k of Object.keys(screens)) {
    screens[k].classList.toggle('hidden', k !== name);
  }
}

function setError(text) {
  errorEl.textContent = text || '';
}

function send(obj) {
  if (ws && ws.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify(obj));
  }
}

function connect() {
  if (ws && ws.readyState === WebSocket.OPEN) return Promise.resolve();
  if (ws) ws.close();

  // Loading the file directly via file:// produces an empty location.host —
  // catch that early with a useful message instead of a generic disconnect.
  if (!location.host || location.protocol === 'file:') {
    return Promise.reject(new Error('Open via http://localhost:4000 (run `npm start` first)'));
  }

  return new Promise((resolve, reject) => {
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const token = getToken();
    const wsUrl = `${proto}//${location.host}/ws${token ? '?token=' + encodeURIComponent(token) : ''}`;
    ws = new WebSocket(wsUrl);
    let openedOnce = false;
    let settled = false;
    ws.addEventListener('open', () => {
      openedOnce = true;
      settled = true;
      resolve();
    }, { once: true });
    ws.addEventListener('error', () => {
      if (!settled) {
        settled = true;
        reject(new Error('Could not reach server — is `npm start` running?'));
      }
    }, { once: true });
    ws.addEventListener('message', (e) => {
      let msg;
      try { msg = JSON.parse(e.data); } catch { return; }
      handle(msg);
    });
    ws.addEventListener('close', () => {
      // Only surface "Disconnected" if we were actually connected; during a
      // failed initial handshake the explicit reject() already set a clearer
      // error message and we don't want to clobber it.
      if (openedOnce && !lobbyEl.classList.contains('hidden')) {
        setError('Disconnected from server');
      }
    });
  });
}

function renderRoomList(rooms) {
  const list = document.getElementById('roomList');
  const empty = document.getElementById('roomListEmpty');
  for (const el of list.querySelectorAll('.room-entry')) el.remove();
  if (rooms.length === 0) {
    empty.style.display = '';
  } else {
    empty.style.display = 'none';
    for (const room of rooms) {
      const entry = document.createElement('div');
      entry.className = 'room-entry';
      entry.innerHTML = `
        <div class="room-entry-info">
          <div class="room-entry-code">${room.code}</div>
          <div class="room-entry-meta">${room.hostName} · ${room.playerCount} player${room.playerCount !== 1 ? 's' : ''}</div>
        </div>
        <button class="btn-join-room" data-code="${room.code}">Join</button>
      `;
      entry.querySelector('.btn-join-room').addEventListener('click', () => {
        joinRoom(room.code);
      });
      list.appendChild(entry);
    }
  }
}

async function joinRoom(code) {
  setError('Connecting…');
  try { await connect(); } catch (e) { setError(e.message); return; }
  send({ type: 'name', name: pilotName() });
  send({ type: 'join', code });
}

function isPrivateRoom() {
  return document.getElementById('privacyPrivate')?.checked ?? false;
}

function handle(msg) {
  switch (msg.type) {
    case 'room':
      myId = msg.you;
      isHost = !!msg.host;
      roomCodeEl.textContent = msg.code;
      document.getElementById('roomPrivacyBadge').textContent = msg.private ? 'PRIVATE' : 'OPEN';
      startBtn.classList.toggle('hidden', !isHost);
      waitingEl.classList.toggle('hidden', isHost);
      showScreen('room');
      setError('');
      break;
    case 'players':
      lastPlayers = msg.players;
      playersEl.innerHTML = '';
      for (const p of msg.players) {
        const li = document.createElement('li');
        li.textContent = p.name + (p.id === myId ? ' (you)' : '') + (p.host ? ' — host' : '');
        playersEl.appendChild(li);
      }
      break;
    case 'start':
      mySpawn = msg.spawns?.[myId] || null;
      myAsteroids = msg.asteroids || null;
      enterGame();
      break;
    case 'rooms-list':
      renderRoomList(msg.rooms || []);
      break;
    case 'error':
      setError(msg.message || 'Error');
      break;
  }
}

function showHud() {
  lobbyEl.classList.add('hidden');
  // Drop the in-lobby flag so the settings panel's "Back to Menu" button
  // becomes visible (it's hidden while the lobby is on screen).
  document.body.classList.remove('in-lobby');
  if (localStorage.getItem('spaceships:showStats') !== '0') {
    document.getElementById('hud-stats').style.display = '';
  }
  document.getElementById('reticle').style.display = '';
  document.getElementById('healthbar').style.display = '';
  document.getElementById('chargebar').style.display = '';
  document.getElementById('boostbar').style.display = '';
  document.getElementById('heatbar').style.display = '';
  // Settings gear stays visible in-game so the volume sliders work
  // during a match. Panel collapses so it doesn't float over the HUD.
  settingsPanel.classList.add('hidden');
}
// Lobby is the initial state; flag it so the Back-to-Menu button stays
// hidden until showHud() flips us into a running game.
document.body.classList.add('in-lobby');

// "Back to Menu" inside the settings panel. Simplest, safest impl is a
// full page reload — tears down WebGL, audio nodes, WebSocket, all bot
// AI timers, and re-enters the lobby in a clean state. We confirm first
// since it discards an in-progress match.
document.getElementById('btnBackToMenu').addEventListener('click', () => {
  if (window.confirm('Leave match and return to menu?')) {
    window.location.reload();
  }
});

function hardMode() { return hardModeInput.checked; }

function enterGame() {
  showHud();
  startGame({
    ws, you: myId, host: isHost, spawn: mySpawn,
    asteroids: myAsteroids, players: lastPlayers,
    noMouse: noMouse(),
    controlScheme: controlScheme(),
    hardMode: hardMode(),
    pilotName: pilotName(),
  });
}

function enterSoloGame(mode, opts = {}) {
  showHud();
  // Tutorial buttons can force a scheme (keyboard-only vs mouse+keys) for
  // their lesson; everything else takes the lobby's saved choice.
  const scheme = opts.controlScheme ?? controlScheme();
  startGame({
    solo: true, you: 0, pilotName: pilotName(), mode,
    noMouse: opts.noMouse ?? (scheme === 'keyboard'),
    controlScheme: scheme,
    hardMode: hardMode(),
  });
}

document.getElementById('btnMulti').addEventListener('click', () => {
  setError('');
  showScreen('multi');
});

document.getElementById('btnBackMulti').addEventListener('click', () => {
  setError('');
  showScreen('main');
});

document.getElementById('btnCreateMenu').addEventListener('click', () => {
  setError('');
  showScreen('create');
});

document.getElementById('btnBackCreate').addEventListener('click', () => {
  setError('');
  showScreen('multi');
});

document.getElementById('btnPlay').addEventListener('click', async () => {
  setError('Connecting…');
  try { await connect(); } catch (e) { setError(e.message); return; }
  send({ type: 'name', name: pilotName() });
  send({ type: 'create', private: isPrivateRoom() });
});


document.getElementById('btnSingle').addEventListener('click', () => {
  setError('');
  showScreen('single');
});

document.getElementById('btnBackSingle').addEventListener('click', () => {
  setError('');
  showScreen('main');
});

document.getElementById('btnTrain').addEventListener('click', () => {
  enterSoloGame('train');
});

document.getElementById('btnSkirmish').addEventListener('click', () => {
  enterSoloGame('skirmish');
});

document.getElementById('btnTutorial').addEventListener('click', () => {
  setError('');
  showScreen('tutorial');
});

document.getElementById('btnBackTutorial').addEventListener('click', () => {
  setError('');
  showScreen('single');
});

document.getElementById('btnTutorialKeys').addEventListener('click', () => {
  enterSoloGame('tutorial', { noMouse: true, controlScheme: 'keyboard' });
});

document.getElementById('btnTutorialMouse').addEventListener('click', () => {
  enterSoloGame('tutorial', { noMouse: false, controlScheme: 'mouse_keys' });
});

document.getElementById('btnTrials').addEventListener('click', () => {
  setError('');
  refreshTrialButtons();
  showScreen('trials');
});

document.getElementById('btnBackTrials').addEventListener('click', () => {
  setError('');
  showScreen('single');
});

function refreshTrialButtons() {
  const defs = [
    { id: 'btnTrial2', label: 'Trial 2', mode: 'trials2', reqKey: 'spaceships:trial1Best' },
    { id: 'btnTrial3', label: 'Trial 3', mode: 'trials3', reqKey: 'spaceships:trial2Best' },
    { id: 'btnTrial4', label: 'Trial 4', mode: 'trials4', reqKey: 'spaceships:trial3Best' },
  ];
  for (const def of defs) {
    const el = document.getElementById(def.id);
    if (!el) continue;
    const unlocked = localStorage.getItem(def.reqKey) !== null;
    el.classList.toggle('locked', !unlocked);
    el.textContent = unlocked ? def.label : `[LOCKED]  ${def.label}`;
  }
}

document.getElementById('btnTrial1').addEventListener('click', () => {
  enterSoloGame('trials');
});

document.getElementById('btnTrial2').addEventListener('click', () => {
  if (document.getElementById('btnTrial2').classList.contains('locked')) return;
  enterSoloGame('trials2');
});

document.getElementById('btnTrial3').addEventListener('click', () => {
  if (document.getElementById('btnTrial3').classList.contains('locked')) return;
  enterSoloGame('trials3');
});

document.getElementById('btnTrial4').addEventListener('click', () => {
  if (document.getElementById('btnTrial4').classList.contains('locked')) return;
  enterSoloGame('trials4');
});

document.getElementById('btnFind').addEventListener('click', async () => {
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

document.getElementById('btnRefreshRooms').addEventListener('click', async () => {
  try {
    await connect();
    send({ type: 'list-rooms' });
  } catch (e) {
    setError(e.message);
  }
});

document.getElementById('btnBackFind').addEventListener('click', () => {
  showScreen('multi');
  setError('');
});

document.getElementById('btnJoin').addEventListener('click', async () => {
  const code = codeInput.value.trim().toUpperCase();
  if (code.length !== 4) {
    setError('Code must be 4 letters');
    return;
  }
  await joinRoom(code);
});

startBtn.addEventListener('click', () => send({ type: 'start' }));

document.getElementById('btnLeave').addEventListener('click', () => {
  send({ type: 'leave' });
  if (ws) ws.close();
  ws = null;
  showScreen('main');
});

// Sanitize the code input to uppercase A–Z only.
codeInput.addEventListener('input', (e) => {
  e.target.value = e.target.value.toUpperCase().replace(/[^A-Z]/g, '').slice(0, 4);
});
codeInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') document.getElementById('btnJoin').click();
});

// ── Ship Customization ────────────────────────────────────────────────────────

const custPanel       = document.getElementById('customization');
const colorWheelEl    = document.getElementById('colorWheel');
const brightnessEl    = document.getElementById('brightnessSlider');
const zoomEl          = document.getElementById('zoomSlider');
const colorPreviewEl  = document.getElementById('colorPreview');
const wheelCtx        = colorWheelEl.getContext('2d');
const WHEEL_SIZE      = colorWheelEl.width; // 200
const WHEEL_R         = WHEEL_SIZE / 2;

let custScene = null;

// Hull color state
const _initHull = hexToHsl(getSavedShipColor());
let hullH = _initHull.h, hullS = _initHull.s;
let hullL = Math.max(0.05, Math.min(0.88, _initHull.l));

// Accent color state
const _initAccent = hexToHsl(getSavedAccentColor());
let accentH = _initAccent.h, accentS = _initAccent.s;
let accentL = Math.max(0.05, Math.min(0.88, _initAccent.l));

// Trail color state
const _initTrail = hexToHsl(getSavedTrailColor());
let trailH = _initTrail.h, trailS = _initTrail.s;
let trailL = Math.max(0.05, Math.min(0.88, _initTrail.l));

// Active picker mirrors whichever target is selected
let colorTarget = 'hull';
let pickH = hullH, pickS = hullS, pickL = hullL;
brightnessEl.value = Math.round(hullL * 100);

function hslToRgbInt(h, s, l) {
  const a = s * Math.min(l, 1 - l);
  const f = n => {
    const k = (n + h / 30) % 12;
    return Math.max(0, Math.min(255, Math.round((l - a * Math.max(Math.min(k - 3, 9 - k, 1), -1)) * 255)));
  };
  return [f(0), f(8), f(4)];
}

function drawWheel() {
  const img = wheelCtx.createImageData(WHEEL_SIZE, WHEEL_SIZE);
  const d = img.data;
  for (let py = 0; py < WHEEL_SIZE; py++) {
    for (let px = 0; px < WHEEL_SIZE; px++) {
      const dx = px - WHEEL_R, dy = py - WHEEL_R;
      const dist = Math.sqrt(dx * dx + dy * dy);
      if (dist > WHEEL_R) continue;
      const h = ((Math.atan2(dy, dx) * 180 / Math.PI) + 360) % 360;
      const s = dist / WHEEL_R;
      const [r, g, b] = hslToRgbInt(h, s, pickL);
      const i = (py * WHEEL_SIZE + px) * 4;
      d[i] = r; d[i + 1] = g; d[i + 2] = b; d[i + 3] = 255;
    }
  }
  wheelCtx.putImageData(img, 0, 0);

  const sx = WHEEL_R + Math.cos(pickH * Math.PI / 180) * pickS * (WHEEL_R - 2);
  const sy = WHEEL_R + Math.sin(pickH * Math.PI / 180) * pickS * (WHEEL_R - 2);
  wheelCtx.beginPath();
  wheelCtx.arc(sx, sy, 7, 0, Math.PI * 2);
  wheelCtx.strokeStyle = '#fff';
  wheelCtx.lineWidth = 2.5;
  wheelCtx.stroke();
  wheelCtx.beginPath();
  wheelCtx.arc(sx, sy, 5.5, 0, Math.PI * 2);
  wheelCtx.strokeStyle = 'rgba(0,0,0,0.7)';
  wheelCtx.lineWidth = 1.5;
  wheelCtx.stroke();
}

function applyColor() {
  const hex = hslToHex(pickH, pickS, pickL);
  if (colorTarget === 'hull') {
    colorPreviewEl.style.background = hex;
    localStorage.setItem('spaceships:shipColor', hex);
    if (custScene) custScene.setColor(hex);
  } else if (colorTarget === 'accent') {
    colorPreviewEl.style.borderColor = hex;
    localStorage.setItem('spaceships:shipAccentColor', hex);
    if (custScene) custScene.setAccentColor(hex);
  } else {
    // trail — show via the trail swatch
    const trailSwatch = document.getElementById('trailColorSwatch');
    if (trailSwatch) trailSwatch.style.background = hex;
    localStorage.setItem('spaceships:trailColor', hex);
  }
  const hullHex = hslToHex(hullH, hullS, hullL);
  colorPreviewEl.style.boxShadow = `0 0 18px ${hullHex}88`;
}

function setColorTarget(target) {
  if (colorTarget === 'hull')   { hullH   = pickH; hullS   = pickS; hullL   = pickL; }
  else if (colorTarget === 'accent') { accentH = pickH; accentS = pickS; accentL = pickL; }
  else                          { trailH  = pickH; trailS  = pickS; trailL  = pickL; }

  colorTarget = target;

  if (target === 'hull')   { pickH = hullH;   pickS = hullS;   pickL = hullL;   }
  else if (target === 'accent') { pickH = accentH; pickS = accentS; pickL = accentL; }
  else                     { pickH = trailH;  pickS = trailS;  pickL = trailL;  }

  brightnessEl.value = Math.round(pickL * 100);
  document.getElementById('tabHull').classList.toggle('active', target === 'hull');
  document.getElementById('tabAccent').classList.toggle('active', target === 'accent');
  document.getElementById('tabTrail').classList.toggle('active', target === 'trail');
  // Show trail swatch when editing trail color, hide ship color preview and vice-versa.
  const trailSwatch = document.getElementById('trailColorSwatch');
  if (trailSwatch) trailSwatch.style.display = target === 'trail' ? '' : 'none';
  colorPreviewEl.style.display = target === 'trail' ? 'none' : '';
  drawWheel();
}

function pickFromWheel(clientX, clientY) {
  const rect = colorWheelEl.getBoundingClientRect();
  const scaleX = WHEEL_SIZE / rect.width;
  const scaleY = WHEEL_SIZE / rect.height;
  const dx = (clientX - rect.left) * scaleX - WHEEL_R;
  const dy = (clientY - rect.top)  * scaleY - WHEEL_R;
  const dist = Math.sqrt(dx * dx + dy * dy);
  pickH = ((Math.atan2(dy, dx) * 180 / Math.PI) + 360) % 360;
  pickS = Math.min(1, dist / WHEEL_R);
  drawWheel();
  applyColor();
}

let wheelDragging = false;
colorWheelEl.addEventListener('mousedown', e => { wheelDragging = true; pickFromWheel(e.clientX, e.clientY); });
window.addEventListener('mousemove', e => { if (wheelDragging) pickFromWheel(e.clientX, e.clientY); });
window.addEventListener('mouseup', () => { wheelDragging = false; });
colorWheelEl.addEventListener('touchstart', e => { e.preventDefault(); pickFromWheel(e.touches[0].clientX, e.touches[0].clientY); }, { passive: false });
colorWheelEl.addEventListener('touchmove',  e => { e.preventDefault(); pickFromWheel(e.touches[0].clientX, e.touches[0].clientY); }, { passive: false });

brightnessEl.addEventListener('input', () => {
  pickL = brightnessEl.value / 100;
  drawWheel();
  applyColor();
});

zoomEl.addEventListener('input', () => {
  if (custScene) custScene.setZoom(parseFloat(zoomEl.value));
});

document.getElementById('tabHull').addEventListener('click', () => setColorTarget('hull'));
document.getElementById('tabAccent').addEventListener('click', () => setColorTarget('accent'));
document.getElementById('tabTrail').addEventListener('click', () => setColorTarget('trail'));

// Trail shape picker
const TRAIL_SHAPE_KEY = 'spaceships:trailShape';
function setTrailShape(shape) {
  localStorage.setItem(TRAIL_SHAPE_KEY, shape);
  for (const btn of document.querySelectorAll('.trail-shape-btn')) {
    btn.classList.toggle('active', btn.dataset.shape === shape);
  }
}
document.getElementById('trail-shape-picker').addEventListener('click', (e) => {
  const btn = e.target.closest('.trail-shape-btn');
  if (btn) setTrailShape(btn.dataset.shape);
});
setTrailShape(getSavedTrailShape());

// Trail color swatch initial state
const trailSwatchEl = document.getElementById('trailColorSwatch');
if (trailSwatchEl) trailSwatchEl.style.background = getSavedTrailColor();

drawWheel();
applyColor();
colorPreviewEl.style.borderColor = hslToHex(accentH, accentS, accentL);

document.getElementById('btnCustomize').addEventListener('click', () => {
  if (custPanel.classList.contains('open')) {
    closeCustomization();
    return;
  }
  lobbyEl.classList.add('slide-left');
  custPanel.classList.add('open');
  if (!custScene) {
    custScene = initCustomizationScene(document.getElementById('custCanvas'));
  } else {
    custScene.resume();
  }
  custScene.setColor(getSavedShipColor());
  custScene.setAccentColor(getSavedAccentColor());
});

const saveColorsStatus = document.getElementById('saveColorsStatus');
let saveStatusTimer = null;

async function saveColorsToServer() {
  const token = getToken();
  if (!token) {
    saveColorsStatus.textContent = 'Log in to save to account';
    saveColorsStatus.style.color = '#ff8a8a';
  } else {
    saveColorsStatus.textContent = 'Saving…';
    saveColorsStatus.style.color = '#c8e0ff';
    try {
      const res = await fetch('/spaceships/api/colors', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json', 'Authorization': 'Bearer ' + token },
        body: JSON.stringify({
          shipColor: getSavedShipColor(),
          accentColor: getSavedAccentColor(),
        }),
      });
      let data;
      try { data = await res.json(); } catch { data = {}; }
      if (res.ok && data.ok) {
        saveColorsStatus.textContent = 'Saved!';
        saveColorsStatus.style.color = '#66ff88';
      } else {
        saveColorsStatus.textContent = data.error || ('Server error ' + res.status);
        saveColorsStatus.style.color = '#ff8a8a';
      }
    } catch {
      saveColorsStatus.textContent = 'No connection to server';
      saveColorsStatus.style.color = '#ff8a8a';
    }
  }
  if (saveStatusTimer) clearTimeout(saveStatusTimer);
  saveStatusTimer = setTimeout(() => { saveColorsStatus.textContent = ''; }, 3000);
}

document.getElementById('btnSaveColors').addEventListener('click', saveColorsToServer);

document.getElementById('btnResetColors').addEventListener('click', () => {
  const DEFAULT_HULL   = '#9fb6cc';
  const DEFAULT_ACCENT = '#2a3340';
  const DEFAULT_TRAIL  = '#66ddff';

  // Reset stored values
  localStorage.setItem('spaceships:shipColor',       DEFAULT_HULL);
  localStorage.setItem('spaceships:shipAccentColor', DEFAULT_ACCENT);
  localStorage.setItem('spaceships:trailColor',      DEFAULT_TRAIL);

  // Rebuild internal HSL state for all three targets
  const h = hexToHsl(DEFAULT_HULL);
  hullH = h.h; hullS = h.s; hullL = Math.max(0.05, Math.min(0.88, h.l));
  const a = hexToHsl(DEFAULT_ACCENT);
  accentH = a.h; accentS = a.s; accentL = Math.max(0.05, Math.min(0.88, a.l));
  const t = hexToHsl(DEFAULT_TRAIL);
  trailH = t.h; trailS = t.s; trailL = Math.max(0.05, Math.min(0.88, t.l));

  // Re-sync the active picker to whatever tab is currently selected
  if (colorTarget === 'hull')        { pickH = hullH;   pickS = hullS;   pickL = hullL;   }
  else if (colorTarget === 'accent') { pickH = accentH; pickS = accentS; pickL = accentL; }
  else                               { pickH = trailH;  pickS = trailS;  pickL = trailL;  }
  brightnessEl.value = Math.round(pickL * 100);

  // Apply to scene and UI
  if (custScene) {
    custScene.setColor(DEFAULT_HULL);
    custScene.setAccentColor(DEFAULT_ACCENT);
  }
  colorPreviewEl.style.background   = DEFAULT_HULL;
  colorPreviewEl.style.borderColor  = DEFAULT_ACCENT;
  colorPreviewEl.style.boxShadow    = `0 0 18px ${DEFAULT_HULL}88`;
  const trailSwatch = document.getElementById('trailColorSwatch');
  if (trailSwatch) trailSwatch.style.background = DEFAULT_TRAIL;

  drawWheel();
});

function closeCustomization() {
  custPanel.classList.remove('open');
  lobbyEl.classList.remove('slide-left');
  if (custScene) custScene.pause();
}

document.getElementById('btnSaveCustom').addEventListener('click', closeCustomization);

// ── Logout ────────────────────────────────────────────────────────────────────

function parseJwtUsername(token) {
  try {
    return JSON.parse(atob(token.split('.')[1].replace(/-/g, '+').replace(/_/g, '/'))).username || null;
  } catch { return null; }
}

function refreshLogoutRow() {
  const token    = getToken();
  const username = token ? parseJwtUsername(token) : null;
  const row      = document.getElementById('settingsLogoutRow');
  const label    = document.getElementById('pilotLabelName');
  if (username) {
    label.textContent = username;
    row.classList.remove('hidden');
  } else {
    row.classList.add('hidden');
  }
}

document.getElementById('btnLogout').addEventListener('click', () => {
  clearToken();
  // Close the settings panel, reload so the auth overlay re-appears.
  document.getElementById('settingsPanel').classList.add('hidden');
  location.reload();
});

// Gate the lobby behind auth. If the player already has a valid JWT this
// resolves instantly (no overlay shown). Guests click through without a token.
requireAuth().then(() => {
  refreshLogoutRow();
  // Guests get a randomly-generated name they cannot change.
  if (localStorage.getItem('spaceships:isGuest') === '1') {
    const guestName = localStorage.getItem(SAVED_NAME_KEY) || 'Pilot';
    nameInput.value = guestName;
    nameInput.readOnly = true;
    nameInput.style.opacity = '0.55';
    nameInput.title = 'Guests cannot change their callsign — log in to choose a name';
  } else if (getToken()) {
    // Logged-in players always use their account username.
    const accountName = localStorage.getItem(SAVED_NAME_KEY) || 'Pilot';
    nameInput.value = accountName;
    nameInput.readOnly = true;
    nameInput.style.opacity = '0.55';
    nameInput.title = 'Your callsign is your account username and cannot be changed here';
  }
});

// ── Online count ──────────────────────────────────────────────────────────────

const onlineCountEl = document.getElementById('online-count');
async function refreshOnlineCount() {
  try {
    const res = await fetch('/spaceships/api/online');
    if (!res.ok) return;
    const data = await res.json();
    if (onlineCountEl) onlineCountEl.textContent = data.count;
  } catch { /* server unreachable — leave the dash */ }
}
refreshOnlineCount();
setInterval(refreshOnlineCount, 30_000);

// ── Controls popup ────────────────────────────────────────────────────────────

const CONTROL_GUIDES = {
  mouse_keys: [
    ['Mouse / Arrows', 'Steer'],
    ['LMB / F', 'Fire'],
    ['Scroll / W / S', 'Throttle'],
    ['Shift', 'Boost'],
    ['Space', 'Drift / Charge'],
    ['A / D', 'Roll'],
    ['RMB', 'Free-look'],
    ['P', 'Toggle gun'],
    ['C', 'Aim assist'],
    ['O', 'Grab mouse'],
    ['L', 'Fullscreen'],
  ],
  keyboard: [
    ['Arrows', 'Steer'],
    ['F', 'Fire'],
    ['W / S', 'Throttle'],
    ['Shift', 'Boost'],
    ['Space', 'Drift / Charge'],
    ['A / D', 'Roll'],
    ['P', 'Toggle gun'],
    ['C', 'Aim assist'],
    ['L', 'Fullscreen'],
  ],
  mobile: [
    ['Left thumb', 'Steer'],
    ['Right buttons', 'Fire / Boost / Drift'],
    ['⟲ ⟳', 'Roll'],
    ['Slider', 'Throttle'],
  ],
};

const btnControlsPopup = document.getElementById('btnControlsPopup');
const controlsPopup = document.getElementById('controls-popup');

btnControlsPopup.addEventListener('click', () => {
  const isHidden = controlsPopup.classList.toggle('hidden');
  btnControlsPopup.textContent = isHidden ? 'CONTROLS ▾' : 'CONTROLS ▴';
  if (!isHidden) {
    const scheme = localStorage.getItem(SAVED_SCHEME_KEY) || 'mouse_keys';
    const entries = CONTROL_GUIDES[scheme] || CONTROL_GUIDES.mouse_keys;
    controlsPopup.innerHTML = entries
      .map(([key, desc]) => `<div class="ctrl-row"><span class="ctrl-key">${key}</span><span class="ctrl-desc">${desc}</span></div>`)
      .join('');
  }
});

// ── Stats toggle ──────────────────────────────────────────────────────────────

const showStatsInput = document.getElementById('showStatsInput');
showStatsInput.checked = localStorage.getItem('spaceships:showStats') !== '0';
showStatsInput.addEventListener('change', () => {
  const show = showStatsInput.checked;
  localStorage.setItem('spaceships:showStats', show ? '1' : '0');
  const hudStats = document.getElementById('hud-stats');
  if (hudStats) hudStats.style.display = show ? '' : 'none';
});
