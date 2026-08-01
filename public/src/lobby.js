import { startGame } from './main.js';
import { requireAuth, getToken, clearToken } from './auth.js';
import { getSavedShipColor, getSavedAccentColor, getSavedTrailColor, getSavedTrailShape, hslToHex, hexToHsl, initCustomizationScene } from './customization.js';
import { containsProfanity } from './filter.js';
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
  campaign: document.getElementById('lobby-campaign'),
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
let myMap = 'space';
let myBotAssignments = [];
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
const SCHEMES = ['mouse_keys', 'keyboard', 'mobile'];
const SAVED_SCHEME_KEY = 'spaceships:controlScheme';
const SAVED_NO_MOUSE_KEY = 'spaceships:noMouse';
function detectDefaultScheme() {
  const touch = 'ontouchstart' in window || navigator.maxTouchPoints > 0;
  const coarse = window.matchMedia?.('(pointer: coarse)')?.matches;
  return touch && coarse ? 'mobile' : 'mouse_keys';
}
let selectedScheme = localStorage.getItem(SAVED_SCHEME_KEY);
if (!SCHEMES.includes(selectedScheme)) {
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
const settingsBtn = document.getElementById('settingsBtn');
const settingsPanel = document.getElementById('settingsPanel');
const hardModeInput = document.getElementById('hardModeInput');
const SAVED_HARD_MODE_KEY = 'spaceships:hardMode';
hardModeInput.checked = localStorage.getItem(SAVED_HARD_MODE_KEY) === '1';
hardModeInput.addEventListener('change', () => {
  localStorage.setItem(SAVED_HARD_MODE_KEY, hardModeInput.checked ? '1' : '0');
});
const cockpitViewInput = document.getElementById('cockpitViewInput');
const SAVED_VIEW_KEY = 'spaceships:viewMode';
cockpitViewInput.checked = localStorage.getItem(SAVED_VIEW_KEY) === 'first';
cockpitViewInput.addEventListener('change', () => {
  localStorage.setItem(SAVED_VIEW_KEY, cockpitViewInput.checked ? 'first' : 'third');
});
const pixelFilterInput = document.getElementById('pixelFilterInput');
const SAVED_PIXEL_KEY = 'spaceships:pixelFilter';
pixelFilterInput.checked = localStorage.getItem(SAVED_PIXEL_KEY) !== '0';
pixelFilterInput.addEventListener('change', () => {
  localStorage.setItem(SAVED_PIXEL_KEY, pixelFilterInput.checked ? '1' : '0');
});
// Ultra graphics is read once at game start, so the toggle only takes effect on
// the next launch. Off by default.
const ultraGraphicsInput = document.getElementById('ultraGraphicsInput');
const SAVED_ULTRA_KEY = 'spaceships:ultraGraphics';
function syncPixelAvailability() {
  const on = ultraGraphicsInput.checked;
  pixelFilterInput.disabled = on;
  pixelFilterInput.closest('label').style.opacity = on ? '0.4' : '';
}
ultraGraphicsInput.checked = localStorage.getItem(SAVED_ULTRA_KEY) === '1';
syncPixelAvailability();
ultraGraphicsInput.addEventListener('change', () => {
  localStorage.setItem(SAVED_ULTRA_KEY, ultraGraphicsInput.checked ? '1' : '0');
  syncPixelAvailability();
});
const enemyTrailsInput = document.getElementById('enemyTrailsInput');
const SAVED_ENEMY_TRAILS = 'spaceships:enemyTrails';
enemyTrailsInput.checked = localStorage.getItem(SAVED_ENEMY_TRAILS) !== '0';
enemyTrailsInput.addEventListener('change', () => {
  localStorage.setItem(SAVED_ENEMY_TRAILS, enemyTrailsInput.checked ? '1' : '0');
});
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
function selectedMap() {
  return document.getElementById('mapTerrain')?.checked ? 'terrain' : 'space';
}
function autoBotEnabled() {
  return document.getElementById('autoBotInput')?.checked ?? true;
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
      myMap = msg.map || 'space';
      myBotAssignments = isHost ? (msg.botAssignments || []) : [];
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
  document.body.classList.remove('in-lobby');
  if (localStorage.getItem('spaceships:showStats') !== '0') {
    document.getElementById('hud-stats').style.display = '';
  }
  document.getElementById('reticle').style.display = '';
  document.getElementById('healthbar').style.display = '';
  document.getElementById('chargebar').style.display = '';
  document.getElementById('boostbar').style.display = '';
  document.getElementById('heatbar').style.display = '';
  document.getElementById('missilehud').style.display = '';
  document.getElementById('flarehud').style.display = '';
  settingsPanel.classList.add('hidden');
}
document.body.classList.add('in-lobby');
document.getElementById('btnBackToMenu').addEventListener('click', () => {
  if (window.confirm('Leave match and return to menu?')) {
    window.location.reload();
  }
});
function hardMode() { return hardModeInput.checked; }
function enterGame() {
  closeCustomization();
  showHud();
  startGame({
    ws, you: myId, host: isHost, spawn: mySpawn,
    asteroids: myAsteroids, players: lastPlayers,
    noMouse: noMouse(),
    controlScheme: controlScheme(),
    hardMode: hardMode(),
    pilotName: pilotName(),
    map: myMap,
    botAssignments: myBotAssignments,
  });
}
function soloSelectedMap() {
  return document.getElementById('soloMapTerrain')?.checked ? 'terrain' : 'space';
}
function enterSoloGame(mode, opts = {}) {
  closeCustomization();
  showHud();
  const scheme = opts.controlScheme ?? controlScheme();
  startGame({
    solo: true, you: 0, pilotName: pilotName(), mode,
    noMouse: opts.noMouse ?? (scheme === 'keyboard'),
    controlScheme: scheme,
    hardMode: hardMode(),
    map: opts.map ?? soloSelectedMap(),
    missionId: opts.missionId,
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
  send({ type: 'create', private: isPrivateRoom(), map: selectedMap(), allowBot: autoBotEnabled() });
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
document.getElementById('btnCampaign').addEventListener('click', () => {
  setError('');
  refreshCampaignButtons();
  showScreen('campaign');
});
document.getElementById('btnBackCampaign').addEventListener('click', () => {
  setError('');
  showScreen('main');
});
function refreshCampaignButtons() {
  const locks = [
    { id: 'btnMission2', reqKey: 'spaceships:campaign1Beat', statusId: 'mission2Status' },
    { id: 'btnMission3', reqKey: 'spaceships:campaign2Beat', statusId: 'mission3Status' },
  ];
  for (const def of locks) {
    const el = document.getElementById(def.id);
    if (!el) continue;
    const unlocked = localStorage.getItem(def.reqKey) !== null;
    el.classList.toggle('locked', !unlocked);
  }
  const statusKeys = ['spaceships:campaign1Beat', 'spaceships:campaign2Beat', 'spaceships:campaign3Beat'];
  for (let i = 1; i <= 3; i++) {
    const el = document.getElementById(`mission${i}Status`);
    if (el) el.textContent = localStorage.getItem(statusKeys[i - 1]) ? '✓ COMPLETED' : '';
  }
}
document.getElementById('btnMission1').addEventListener('click', () => {
  enterSoloGame('campaign', { map: 'space', missionId: 1 });
});
document.getElementById('btnMission2').addEventListener('click', () => {
  if (document.getElementById('btnMission2').classList.contains('locked')) return;
  enterSoloGame('campaign', { map: 'space', missionId: 2 });
});
document.getElementById('btnMission3').addEventListener('click', () => {
  if (document.getElementById('btnMission3').classList.contains('locked')) return;
  enterSoloGame('campaign', { map: 'space', missionId: 3 });
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
codeInput.addEventListener('input', (e) => {
  e.target.value = e.target.value.toUpperCase().replace(/[^A-Z]/g, '').slice(0, 4);
});
codeInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') document.getElementById('btnJoin').click();
});
const custPanel = document.getElementById('customization');
const colorWheelEl = document.getElementById('colorWheel');
const brightnessEl = document.getElementById('brightnessSlider');
const zoomEl = document.getElementById('zoomSlider');
const colorPreviewEl = document.getElementById('colorPreview');
const wheelCtx = colorWheelEl.getContext('2d');
const WHEEL_SIZE = colorWheelEl.width;
const WHEEL_R = WHEEL_SIZE / 2;
let custScene = null;
const _initHull = hexToHsl(getSavedShipColor());
let hullH = _initHull.h, hullS = _initHull.s;
let hullL = Math.max(0.05, Math.min(0.88, _initHull.l));
const _initAccent = hexToHsl(getSavedAccentColor());
let accentH = _initAccent.h, accentS = _initAccent.s;
let accentL = Math.max(0.05, Math.min(0.88, _initAccent.l));
const _initTrail = hexToHsl(getSavedTrailColor());
let trailH = _initTrail.h, trailS = _initTrail.s;
let trailL = Math.max(0.05, Math.min(0.88, _initTrail.l));
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
    const trailSwatch = document.getElementById('trailColorSwatch');
    if (trailSwatch) trailSwatch.style.background = hex;
    localStorage.setItem('spaceships:trailColor', hex);
  }
  const hullHex = hslToHex(hullH, hullS, hullL);
  colorPreviewEl.style.boxShadow = `0 0 18px ${hullHex}88`;
}
function setColorTarget(target) {
  if (colorTarget === 'hull') { hullH = pickH; hullS = pickS; hullL = pickL; }
  else if (colorTarget === 'accent') { accentH = pickH; accentS = pickS; accentL = pickL; }
  else { trailH = pickH; trailS = pickS; trailL = pickL; }
  colorTarget = target;
  if (target === 'hull') { pickH = hullH; pickS = hullS; pickL = hullL; }
  else if (target === 'accent') { pickH = accentH; pickS = accentS; pickL = accentL; }
  else { pickH = trailH; pickS = trailS; pickL = trailL; }
  brightnessEl.value = Math.round(pickL * 100);
  document.getElementById('tabHull').classList.toggle('active', target === 'hull');
  document.getElementById('tabAccent').classList.toggle('active', target === 'accent');
  document.getElementById('tabTrail').classList.toggle('active', target === 'trail');
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
  const dy = (clientY - rect.top) * scaleY - WHEEL_R;
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
colorWheelEl.addEventListener('touchmove', e => { e.preventDefault(); pickFromWheel(e.touches[0].clientX, e.touches[0].clientY); }, { passive: false });
brightnessEl.addEventListener('input', () => {
  pickL = brightnessEl.value / 100;
  drawWheel();
  applyColor();
});
zoomEl.addEventListener('input', () => {
  if (custScene) custScene.setZoom(parseFloat(zoomEl.value));
});
const UNLOCK_COSTS = { hull: 250, accent: 400, trail: 500, trail_shape: 200, admin_ship: 125000 };
const COST_SAVE_COLORS = 50;
const custCrStatusEl = document.getElementById('custCrStatus');
let custCrTimer = null;
function showCustCrStatus(msg, color = '#ff8a8a') {
  if (!custCrStatusEl) return;
  custCrStatusEl.textContent = msg;
  custCrStatusEl.style.color = color;
  if (custCrTimer) clearTimeout(custCrTimer);
  custCrTimer = setTimeout(() => { custCrStatusEl.textContent = ''; }, 3000);
}
function isUnlocked(feature) {
  return localStorage.getItem(`spaceships:unlock_${feature}`) === '1';
}
function saveUnlockLocal(feature) {
  localStorage.setItem(`spaceships:unlock_${feature}`, '1');
}
function updateCustUnlockUI() {
  const badges = {
    hull: document.getElementById('hullTabCost'),
    accent: document.getElementById('accentTabCost'),
    trail: document.getElementById('trailTabCost'),
    trail_shape: document.getElementById('trailShapeCost'),
  };
  for (const [feature, el] of Object.entries(badges)) {
    if (!el) continue;
    if (isUnlocked(feature)) {
      el.textContent = '· ✓';
      el.style.color = '#66ff88';
    } else {
      el.textContent = `· ${UNLOCK_COSTS[feature]} ⬡`;
      el.style.color = '';
    }
  }
  const adminBtn = document.getElementById('btnBuyAdminShip');
  const adminCost = document.getElementById('adminShipCost');
  if (adminBtn) {
    if (isUnlocked('admin_ship')) {
      adminBtn.textContent = '⚡ ADMIN SHIP · OWNED';
      adminBtn.style.opacity = '0.6';
      adminBtn.style.cursor = 'default';
      if (adminCost) { adminCost.textContent = '· ✓'; adminCost.style.color = '#66ff88'; }
    } else {
      adminBtn.textContent = '⚡ ADMIN SHIP · 125,000 ⬡';
      adminBtn.style.opacity = '';
      adminBtn.style.cursor = '';
      if (adminCost) { adminCost.textContent = '· 125,000 ⬡'; adminCost.style.color = ''; }
    }
  }
}
async function refreshUnlocks() {
  const token = getToken();
  if (!token) return;
  try {
    const res = await fetch('/spaceships/api/unlocks', { headers: { 'Authorization': 'Bearer ' + token } });
    const data = await res.json();
    if (!data.ok) return;
    if (data.unlockHull) saveUnlockLocal('hull');
    if (data.unlockAccent) saveUnlockLocal('accent');
    if (data.unlockTrail) saveUnlockLocal('trail');
    if (data.unlockTrailShape) saveUnlockLocal('trail_shape');
    if (data.unlockAdminShip) saveUnlockLocal('admin_ship');
    updateCustUnlockUI();
  } catch { }
}
async function tryPurchaseUnlock(feature) {
  const cost = UNLOCK_COSTS[feature];
  if (!cost) return { ok: false, msg: 'Unknown feature' };
  const token = getToken();
  if (!token) return { ok: false, msg: 'Log in to unlock this feature' };
  if (isUnlocked(feature)) return { ok: true, alreadyOwned: true };
  const cached = parseInt(localStorage.getItem('spaceships:credits') || '0', 10);
  if (cached < cost) return { ok: false, msg: `Need ${cost - cached} more ⬡ (you have ${cached} ⬡)` };
  try {
    const res = await fetch(`/spaceships/api/unlock/${feature}`, {
      method: 'POST', headers: { 'Authorization': 'Bearer ' + token },
    });
    const data = await res.json();
    if (data.ok) {
      saveUnlockLocal(feature);
      if (!data.alreadyOwned) setCreditsDisplay(data.balance);
      updateCustUnlockUI();
      if (data.newAchievements?.length) {
        try {
          const prev = JSON.parse(localStorage.getItem('spaceships:pendingAchs') || '[]');
          localStorage.setItem('spaceships:pendingAchs', JSON.stringify([...prev, ...data.newAchievements]));
        } catch { }
        checkPendingAchievements();
      }
      return { ok: true, alreadyOwned: data.alreadyOwned };
    }
    return { ok: false, msg: `Not enough ⬡ (have ${data.balance ?? cached})` };
  } catch {
    return { ok: false, msg: 'Could not reach server' };
  }
}
function makeTabHandler(id, feature) {
  document.getElementById(id).addEventListener('click', async () => {
    if (isUnlocked(feature)) { setColorTarget(feature === 'trail' ? 'trail' : feature); return; }
    const r = await tryPurchaseUnlock(feature);
    if (!r.ok) { showCustCrStatus(`${feature} unlock costs ${UNLOCK_COSTS[feature]} ⬡ · ${r.msg}`); return; }
    showCustCrStatus(r.alreadyOwned ? 'Already unlocked!' : `Unlocked! −${UNLOCK_COSTS[feature]} ⬡`, '#66ff88');
    setColorTarget(feature === 'trail' ? 'trail' : feature);
  });
}
makeTabHandler('tabHull', 'hull');
makeTabHandler('tabAccent', 'accent');
makeTabHandler('tabTrail', 'trail');
const adminShipStatusEl = document.getElementById('adminShipStatus');
let adminShipStatusTimer = null;
function showAdminShipStatus(msg, color = '#ff8a8a') {
  if (!adminShipStatusEl) return;
  adminShipStatusEl.textContent = msg;
  adminShipStatusEl.style.color = color;
  if (adminShipStatusTimer) clearTimeout(adminShipStatusTimer);
  adminShipStatusTimer = setTimeout(() => { adminShipStatusEl.textContent = ''; }, 4000);
}
document.getElementById('btnBuyAdminShip')?.addEventListener('click', async () => {
  if (isUnlocked('admin_ship')) return;
  const token = getToken();
  if (!token) { showAdminShipStatus('Log in to purchase the Admin Ship'); return; }
  const cached = parseInt(localStorage.getItem('spaceships:credits') || '0', 10);
  if (cached < UNLOCK_COSTS.admin_ship) {
    showAdminShipStatus(`Need ${(UNLOCK_COSTS.admin_ship - cached).toLocaleString()} more ⬡ (you have ${cached.toLocaleString()} ⬡)`);
    return;
  }
  showAdminShipStatus('Processing…', '#c8e0ff');
  const r = await tryPurchaseUnlock('admin_ship');
  if (r.ok) {
    showAdminShipStatus(r.alreadyOwned ? 'Already owned!' : 'Admin Ship unlocked! Active next match.', '#66ff88');
    updateCustUnlockUI();
  } else {
    showAdminShipStatus(r.msg || 'Purchase failed');
  }
});
const TRAIL_SHAPE_KEY = 'spaceships:trailShape';
function setTrailShape(shape) {
  localStorage.setItem(TRAIL_SHAPE_KEY, shape);
  for (const btn of document.querySelectorAll('.trail-shape-btn')) {
    btn.classList.toggle('active', btn.dataset.shape === shape);
  }
}
document.getElementById('trail-shape-picker').addEventListener('click', async (e) => {
  const btn = e.target.closest('.trail-shape-btn');
  if (!btn) return;
  if (isUnlocked('trail_shape')) { setTrailShape(btn.dataset.shape); return; }
  const r = await tryPurchaseUnlock('trail_shape');
  if (!r.ok) { showCustCrStatus(`Trail shapes cost ${UNLOCK_COSTS.trail_shape} ⬡ to unlock · ${r.msg}`); return; }
  showCustCrStatus(r.alreadyOwned ? 'Already unlocked!' : `Trail shapes unlocked! −${UNLOCK_COSTS.trail_shape} ⬡`, '#66ff88');
  setTrailShape(btn.dataset.shape);
});
setTrailShape(getSavedTrailShape());
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
  document.body.classList.add('customization-open');
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
    const cached = parseInt(localStorage.getItem('spaceships:credits') || '0', 10);
    if (cached < COST_SAVE_COLORS) {
      saveColorsStatus.textContent = `Need ${COST_SAVE_COLORS - cached} more ⬡ to save (${COST_SAVE_COLORS} ⬡ per save)`;
      saveColorsStatus.style.color = '#ff8a8a';
      if (saveStatusTimer) clearTimeout(saveStatusTimer);
      saveStatusTimer = setTimeout(() => { saveColorsStatus.textContent = ''; }, 3500);
      return;
    }
    saveColorsStatus.textContent = 'Saving…';
    saveColorsStatus.style.color = '#c8e0ff';
    try {
      const spendRes = await fetch('/spaceships/api/credits/spend', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'Authorization': 'Bearer ' + token },
        body: JSON.stringify({ amount: COST_SAVE_COLORS, reason: 'save_colors' }),
      });
      const spendData = await spendRes.json();
      if (!spendData.ok) {
        saveColorsStatus.textContent = spendData.error || 'Not enough ⬡';
        saveColorsStatus.style.color = '#ff8a8a';
        if (saveStatusTimer) clearTimeout(saveStatusTimer);
        saveStatusTimer = setTimeout(() => { saveColorsStatus.textContent = ''; }, 3000);
        return;
      }
      setCreditsDisplay(spendData.balance);
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
  const DEFAULT_HULL = '#9fb6cc';
  const DEFAULT_ACCENT = '#2a3340';
  const DEFAULT_TRAIL = '#66ddff';
  localStorage.setItem('spaceships:shipColor', DEFAULT_HULL);
  localStorage.setItem('spaceships:shipAccentColor', DEFAULT_ACCENT);
  localStorage.setItem('spaceships:trailColor', DEFAULT_TRAIL);
  const h = hexToHsl(DEFAULT_HULL);
  hullH = h.h; hullS = h.s; hullL = Math.max(0.05, Math.min(0.88, h.l));
  const a = hexToHsl(DEFAULT_ACCENT);
  accentH = a.h; accentS = a.s; accentL = Math.max(0.05, Math.min(0.88, a.l));
  const t = hexToHsl(DEFAULT_TRAIL);
  trailH = t.h; trailS = t.s; trailL = Math.max(0.05, Math.min(0.88, t.l));
  if (colorTarget === 'hull') { pickH = hullH; pickS = hullS; pickL = hullL; }
  else if (colorTarget === 'accent') { pickH = accentH; pickS = accentS; pickL = accentL; }
  else { pickH = trailH; pickS = trailS; pickL = trailL; }
  brightnessEl.value = Math.round(pickL * 100);
  if (custScene) {
    custScene.setColor(DEFAULT_HULL);
    custScene.setAccentColor(DEFAULT_ACCENT);
  }
  colorPreviewEl.style.background = DEFAULT_HULL;
  colorPreviewEl.style.borderColor = DEFAULT_ACCENT;
  colorPreviewEl.style.boxShadow = `0 0 18px ${DEFAULT_HULL}88`;
  const trailSwatch = document.getElementById('trailColorSwatch');
  if (trailSwatch) trailSwatch.style.background = DEFAULT_TRAIL;
  drawWheel();
});
function closeCustomization() {
  custPanel.classList.remove('open');
  document.body.classList.remove('customization-open');
  lobbyEl.classList.remove('slide-left');
  if (custScene) custScene.pause();
}
document.getElementById('btnSaveCustom').addEventListener('click', closeCustomization);
function parseJwtUsername(token) {
  try {
    return JSON.parse(atob(token.split('.')[1].replace(/-/g, '+').replace(/_/g, '/'))).username || null;
  } catch { return null; }
}
function refreshLogoutRow() {
  const token = getToken();
  const username = token ? parseJwtUsername(token) : null;
  const row = document.getElementById('settingsLogoutRow');
  const label = document.getElementById('pilotLabelName');
  if (username) {
    label.textContent = username;
    row.classList.remove('hidden');
  } else {
    row.classList.add('hidden');
  }
}
document.getElementById('btnLogout').addEventListener('click', () => {
  clearToken();
  document.getElementById('settingsPanel').classList.add('hidden');
  location.reload();
});
function checkPendingAchievements() {
  try {
    const raw = localStorage.getItem('spaceships:pendingAchs');
    if (!raw) return;
    const earned = JSON.parse(raw);
    if (!Array.isArray(earned) || !earned.length) return;
    localStorage.removeItem('spaceships:pendingAchs');
    const list = document.getElementById('hangar-ach-list');
    const overlay = document.getElementById('hangar-ach-overlay');
    if (!list || !overlay) return;
    list.innerHTML = earned.map(a =>
      `<div class="hangar-ach-row">
        <span class="ach-toast-icon">${esc(a.icon)}</span>
        <div class="ach-toast-body">
          <span class="ach-toast-title">ACHIEVEMENT UNLOCKED</span>
          <span class="ach-toast-label">${esc(a.label)}</span>
          <span class="ach-desc">${esc(a.desc)}</span>
        </div>
      </div>`
    ).join('');
    overlay.classList.remove('hidden');
    document.getElementById('btnDismissHangarAch').onclick = () => {
      overlay.classList.add('hidden');
    };
  } catch { }
}
requireAuth().then(() => {
  refreshLogoutRow();
  if (localStorage.getItem('spaceships:isGuest') === '1') {
    const guestName = localStorage.getItem(SAVED_NAME_KEY) || 'Pilot';
    nameInput.value = guestName;
    nameInput.readOnly = true;
    nameInput.style.opacity = '0.55';
    nameInput.title = 'Guests cannot change their callsign — log in to choose a name';
  } else if (getToken()) {
    const accountName = localStorage.getItem(SAVED_NAME_KEY) || 'Pilot';
    nameInput.value = accountName;
    nameInput.readOnly = true;
    nameInput.style.opacity = '0.55';
    nameInput.title = 'Your callsign is your account username and cannot be changed here';
  }
  refreshCredits();
  refreshUnlocks();
  checkPendingAchievements();
});
const creditsAmountEl = document.getElementById('creditsAmount');
function setCreditsDisplay(amount) {
  if (creditsAmountEl) {
    creditsAmountEl.textContent = Number.isFinite(amount)
      ? amount.toLocaleString()
      : '—';
  }
  localStorage.setItem('spaceships:credits', String(amount));
}
async function refreshCredits() {
  const token = getToken();
  if (!token) return;
  try {
    const res = await fetch('/spaceships/api/credits', {
      headers: { 'Authorization': 'Bearer ' + token },
    });
    const data = await res.json();
    if (data.ok) setCreditsDisplay(data.credits);
  } catch { }
}
const _cachedCr = parseInt(localStorage.getItem('spaceships:credits'), 10);
if (!isNaN(_cachedCr)) setCreditsDisplay(_cachedCr);
const CONTROL_GUIDES = {
  mouse_keys: [
    ['Mouse / Arrows', 'Steer'],
    ['LMB / F', 'Fire'],
    ['Scroll / W / S', 'Throttle'],
    ['Shift', 'Boost'],
    ['Space', 'Drift / Charge'],
    ['A / D', 'Roll'],
    ['RMB', 'Free-look'],
    ['V', 'Cockpit view'],
    ['Alt', 'Look back'],
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
    ['V', 'Cockpit view'],
    ['Alt', 'Look back'],
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
const profileOverlay = document.getElementById('profile-overlay');
const profilePaneMy = document.getElementById('profile-pane-my');
const profilePaneLb = document.getElementById('profile-pane-lb');
let lbLoaded = false;
document.getElementById('nameInput').addEventListener('click', openProfilePanel);
document.getElementById('btnCloseProfile').addEventListener('click', () => {
  profileOverlay.classList.add('hidden');
  lbLoaded = false;
});
document.getElementById('profile-tab-my').addEventListener('click', () => switchProfileTab('my'));
document.getElementById('profile-tab-lb').addEventListener('click', async () => {
  switchProfileTab('lb');
  if (!lbLoaded) { lbLoaded = true; await loadLeaderboard(); }
});
function switchProfileTab(tab) {
  document.getElementById('profile-tab-my').classList.toggle('active', tab === 'my');
  document.getElementById('profile-tab-lb').classList.toggle('active', tab === 'lb');
  profilePaneMy.classList.toggle('hidden', tab !== 'my');
  profilePaneLb.classList.toggle('hidden', tab !== 'lb');
}
function fmtCampaignBest(lives) {
  if (lives === null || lives === undefined) return '—';
  if (lives >= 3) return '✓ Flawless';
  return `✓ (${lives} ${lives === 1 ? 'life' : 'lives'} left)`;
}
function fmtTrialTime(t) {
  if (t === null || t === undefined) return '—';
  const total = Math.max(0, parseFloat(t));
  const m = Math.floor(total / 60);
  const s = (total % 60).toFixed(3).padStart(6, '0');
  return `${m}:${s}`;
}
function esc(str) {
  return String(str ?? '').replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}
async function openProfilePanel() {
  profileOverlay.classList.remove('hidden');
  lbLoaded = false;
  switchProfileTab('my');
  const token = getToken();
  if (!token) {
    profilePaneMy.innerHTML = '<div class="profile-no-account">Log in to view your profile and stats.</div>';
    return;
  }
  const username = localStorage.getItem('spaceships:pilotName');
  if (!username) {
    profilePaneMy.innerHTML = '<div class="profile-no-account">No pilot name found.</div>';
    return;
  }
  profilePaneMy.innerHTML = '<div class="profile-loading">Loading…</div>';
  try {
    const res = await fetch(`/spaceships/api/profile/${encodeURIComponent(username)}`);
    const data = await res.json();
    if (!data.ok) {
      profilePaneMy.innerHTML = `<div class="profile-error">${esc(data.error || 'Failed to load profile')}</div>`;
      return;
    }
    renderMyProfile(data.profile);
  } catch {
    profilePaneMy.innerHTML = '<div class="profile-error">Could not reach server.</div>';
  }
}
function renderMyProfile(p) {
  const winRate = p.gamesPlayed > 0 ? Math.round(p.matchesWon / p.gamesPlayed * 100) : 0;
  const earned = p.achievements.filter(a => a.earned);
  const locked = p.achievements.filter(a => !a.earned);
  function badgeHtml(a) {
    const cls = a.earned ? 'achievement-badge' : 'achievement-badge locked';
    const icon = a.earned ? esc(a.icon) : '🔒';
    let progressHtml = '';
    if (!a.earned && a.progress) {
      const { current, target, isTime } = a.progress;
      if (isTime) {
        const curStr = current !== null ? parseFloat(current).toFixed(1) + 's' : '—';
        progressHtml = `<span class="ach-time-hint">${curStr} / &lt;${target}s</span>`;
      } else {
        const pct = target > 0 ? Math.min(100, Math.round(current / target * 100)) : 0;
        progressHtml = `
          <div class="ach-progress-row">
            <div class="ach-progress-bar"><div class="ach-progress-fill" style="width:${pct}%"></div></div>
            <span class="ach-progress-pct">${current}/${target}</span>
          </div>`;
      }
    }
    return `<div class="${cls}" title="${esc(a.desc)}">
      <span class="ach-icon">${icon}</span>
      <div class="ach-badge-body">
        <span class="ach-label">${esc(a.label)}</span>
        ${progressHtml}
      </div>
    </div>`;
  }
  const earnedHtml = earned.length > 0
    ? earned.map(badgeHtml).join('')
    : '<div class="profile-no-ach">No achievements yet — get out there!</div>';
  profilePaneMy.innerHTML = `
    <div class="profile-header">
      <div class="profile-callsign">${esc(p.username)}</div>
      <div class="profile-rank">${esc(p.rank)}</div>
    </div>
    <div class="profile-stats-grid">
      <div class="stat-card"><div class="stat-label">KDR</div><div class="stat-value">${esc(p.kdr)}</div></div>
      <div class="stat-card"><div class="stat-label">KILLS</div><div class="stat-value">${p.totalKills}</div></div>
      <div class="stat-card"><div class="stat-label">DEATHS</div><div class="stat-value">${p.totalDeaths}</div></div>
      <div class="stat-card"><div class="stat-label">BOTS KILLED</div><div class="stat-value">${p.botsKilled}</div></div>
      <div class="stat-card"><div class="stat-label">MATCHES</div><div class="stat-value">${p.gamesPlayed}</div></div>
      <div class="stat-card"><div class="stat-label">WINS</div><div class="stat-value">${p.matchesWon}</div></div>
      <div class="stat-card"><div class="stat-label">LOSSES</div><div class="stat-value">${p.matchesLost}</div></div>
      <div class="stat-card"><div class="stat-label">WIN RATE</div><div class="stat-value">${winRate}%</div></div>
      <div class="stat-card"><div class="stat-label">BEST MATCH</div><div class="stat-value">${p.highScore}</div></div>
    </div>
    <div class="profile-section-title">TIME TRIALS — PERSONAL BEST</div>
    <div class="profile-trials">
      <div class="trial-row"><span>Trial 1</span><span>${fmtTrialTime(p.trial1Best)}</span></div>
      <div class="trial-row"><span>Trial 2</span><span>${fmtTrialTime(p.trial2Best)}</span></div>
      <div class="trial-row"><span>Trial 3</span><span>${fmtTrialTime(p.trial3Best)}</span></div>
      <div class="trial-row"><span>Trial 4</span><span>${fmtTrialTime(p.trial4Best)}</span></div>
    </div>
    <div class="profile-section-title">CAMPAIGN</div>
    <div class="profile-trials">
      <div class="trial-row"><span>Mission 1: Operation Ironclad</span><span>${fmtCampaignBest(p.campaign1BestLives)}</span></div>
      <div class="trial-row"><span>Mission 2: Operation Stormfront</span><span>${fmtCampaignBest(p.campaign2BestLives)}</span></div>
      <div class="trial-row"><span>Mission 3: Final Siege</span><span>${fmtCampaignBest(p.campaign3BestLives)}</span></div>
      <div class="trial-row"><span>Capital Ship Kills</span><span>${p.campaignBossKills ?? 0}</span></div>
      <div class="trial-row"><span>Total Completions</span><span>${p.campaignTotalCompletions ?? 0}</span></div>
    </div>
    <button class="ach-toggle-btn" id="achToggleBtn">
      ACHIEVEMENTS — ${earned.length} / ${p.achievements.length}
      <span class="ach-toggle-arrow">▾</span>
    </button>
    <div class="ach-collapsible" id="achContent">
      <div class="ach-section-label">UNLOCKED</div>
      ${earnedHtml}
      ${locked.length > 0 ? `<div class="ach-section-label locked-label">LOCKED</div>${locked.map(badgeHtml).join('')}` : ''}
    </div>
  `;
  const toggleBtn = profilePaneMy.querySelector('#achToggleBtn');
  const achContent = profilePaneMy.querySelector('#achContent');
  if (toggleBtn && achContent) {
    toggleBtn.addEventListener('click', () => {
      const open = achContent.classList.toggle('ach-open');
      toggleBtn.querySelector('.ach-toggle-arrow').textContent = open ? '▴' : '▾';
    });
  }
}
async function loadLeaderboard() {
  profilePaneLb.innerHTML = '<div class="profile-loading">Loading…</div>';
  try {
    const res = await fetch('/spaceships/api/leaderboard');
    const data = await res.json();
    if (!data.ok) {
      profilePaneLb.innerHTML = '<div class="profile-error">Failed to load leaderboard.</div>';
      return;
    }
    renderLeaderboard(data.leaderboard);
  } catch {
    profilePaneLb.innerHTML = '<div class="profile-error">Could not reach server.</div>';
  }
}
function renderLeaderboard(entries) {
  if (!entries || entries.length === 0) {
    profilePaneLb.innerHTML = '<div class="profile-no-account">No pilots on record yet.</div>';
    return;
  }
  const myName = localStorage.getItem('spaceships:pilotName') || '';
  const rows = entries.map((e, i) => `
    <tr class="${e.username === myName ? 'lb-you' : ''}">
      <td class="lb-rank">#${i + 1}</td>
      <td class="lb-name">${esc(e.username)}</td>
      <td class="lb-val">${esc(e.pilotRank)}</td>
      <td class="lb-val">${e.totalKills}</td>
      <td class="lb-val">${esc(e.kdr)}</td>
      <td class="lb-val">${e.matchesWon}</td>
      <td class="lb-val">${e.gamesPlayed}</td>
    </tr>`).join('');
  profilePaneLb.innerHTML = `
    <table class="lb-table">
      <thead>
        <tr>
          <th>#</th><th>PILOT</th><th>RANK</th><th>KILLS</th><th>KDR</th><th>WINS</th><th>PLAYED</th>
        </tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>`;
}
const showStatsInput = document.getElementById('showStatsInput');
showStatsInput.checked = localStorage.getItem('spaceships:showStats') !== '0';
showStatsInput.addEventListener('change', () => {
  const show = showStatsInput.checked;
  localStorage.setItem('spaceships:showStats', show ? '1' : '0');
  const hudStats = document.getElementById('hud-stats');
  if (hudStats) hudStats.style.display = show ? '' : 'none';
});
setTimeout(() => document.body.classList.remove('intro-active'), 3500);
(function startGamepadMenuNav() {
  const focusStyle = document.createElement('style');
  focusStyle.textContent = `
    .big:focus-visible, .link:focus-visible,
    .campaign-mission-btn:focus-visible, .trial-btn:focus-visible {
      outline: 2px solid #4aa3ff;
      outline-offset: 3px;
      box-shadow: 0 0 0 5px rgba(74,163,255,0.28), 0 0 18px rgba(74,163,255,0.35);
    }`;
  document.head.appendChild(focusStyle);
  const BACK_BTNS = {
    'lobby-multi': 'btnBackMulti',
    'lobby-create': 'btnBackCreate',
    'lobby-find': 'btnBackFind',
    'lobby-room': 'btnLeave',
    'lobby-single': 'btnBackSingle',
    'lobby-tutorial': 'btnBackTutorial',
    'lobby-trials': 'btnBackTrials',
    'lobby-campaign': 'btnBackCampaign',
  };
  function getActiveScreen() {
    return document.querySelector('.screen:not(.hidden)');
  }
  function getMenuFocusables() {
    const screen = getActiveScreen();
    if (!screen) return [];
    return [...screen.querySelectorAll(
      'button:not(.locked):not([disabled]), input[type="radio"], input[type="checkbox"]'
    )].filter(el => el.offsetParent !== null);
  }
  let navCooldown = 0;
  let prevNavUp = false, prevNavDown = false;
  let prevA = false, prevB = false, prevStart = false;
  let lastTs = null;
  function loop(ts) {
    if (!document.body.classList.contains('in-lobby')) return;
    const dt = lastTs == null ? 0 : Math.min((ts - lastTs) / 1000, 0.1);
    lastTs = ts;
    navCooldown = Math.max(0, navCooldown - dt);
    const rawGp = [...(navigator.getGamepads?.() ?? [])].find(g => g?.connected);
    if (!rawGp) { requestAnimationFrame(loop); return; }
    const bt = rawGp.buttons;
    const ax = rawGp.axes;
    const btn = (i) => bt[i]?.pressed ?? false;
    const DEAD = 0.5;
    const navUp = btn(12) || (ax[1] ?? 0) < -DEAD;
    const navDown = btn(13) || (ax[1] ?? 0) > DEAD;
    const pressA = btn(0);
    const pressB = btn(1);
    const pressStart = btn(9);
    const focusables = getMenuFocusables();
    if (focusables.length > 0 && !focusables.includes(document.activeElement)) {
      focusables[0].focus();
    }
    if (navCooldown === 0 && (navUp || navDown)) {
      const dir = navUp ? -1 : 1;
      const cur = focusables.indexOf(document.activeElement);
      const next = ((cur < 0 ? 0 : cur) + dir + focusables.length) % focusables.length;
      focusables[next].focus();
      navCooldown = (prevNavUp || prevNavDown) ? 0.12 : 0.28;
    }
    if (pressA && !prevA) {
      const el = document.activeElement;
      if (el && focusables.includes(el)) el.click();
    }
    if (pressB && !prevB) {
      const screen = getActiveScreen();
      const backId = screen && BACK_BTNS[screen.id];
      if (backId) document.getElementById(backId)?.click();
    }
    if (pressStart && !prevStart) {
      const screen = getActiveScreen();
      if (screen && screen.id !== 'lobby-main') {
        const backId = BACK_BTNS[screen.id];
        if (backId) document.getElementById(backId)?.click();
      }
    }
    prevNavUp = navUp; prevNavDown = navDown;
    prevA = pressA; prevB = pressB; prevStart = pressStart;
    requestAnimationFrame(loop);
  }
  requestAnimationFrame(loop);
}());