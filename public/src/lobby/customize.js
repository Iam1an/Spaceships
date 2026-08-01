// The customization room that slides in from the right: colour wheel, hull /
// accent / trail tabs, trail shape picker, and the two purchasable extras.
import { el, onClick, createStatusLine } from './dom.js';
import { lobbyEl } from './screens.js';
import { getToken } from '../auth.js';
import {
  getSavedShipColor, getSavedAccentColor, getSavedTrailColor, getSavedTrailShape,
  hslToHex, hexToHsl, initCustomizationScene,
} from '../customization.js';
import { setCreditsDisplay, cachedCredits } from './credits.js';
import { UNLOCK_COSTS, isUnlocked, updateCustUnlockUI, tryPurchaseUnlock } from './unlocks.js';

const COST_SAVE_COLORS = 50;
const TRAIL_SHAPE_KEY = 'spaceships:trailShape';

const custPanel = el('customization');
const colorWheelEl = el('colorWheel');
const brightnessEl = el('brightnessSlider');
const zoomEl = el('zoomSlider');
const colorPreviewEl = el('colorPreview');
const trailSwatchEl = el('trailColorSwatch');
const wheelCtx = colorWheelEl.getContext('2d');
const WHEEL_SIZE = colorWheelEl.width;
const WHEEL_R = WHEEL_SIZE / 2;

let custScene = null;

// ── Colour state ────────────────────────────────────────────────────────────
// One slot per tab. Each slot owns a storage key and the bit of the preview it
// paints. `colors` holds the committed value; `pick` is the live wheel position
// for whichever tab is open, and is folded back into `colors` on tab switch.
const COLOR_SLOTS = {
  hull: {
    key: 'spaceships:shipColor',
    preview: (hex) => { colorPreviewEl.style.background = hex; custScene?.setColor(hex); },
  },
  accent: {
    key: 'spaceships:shipAccentColor',
    preview: (hex) => { colorPreviewEl.style.borderColor = hex; custScene?.setAccentColor(hex); },
  },
  trail: {
    key: 'spaceships:trailColor',
    preview: (hex) => { if (trailSwatchEl) trailSwatchEl.style.background = hex; },
  },
};

const TAB_IDS = { hull: 'tabHull', accent: 'tabAccent', trail: 'tabTrail' };

// Pure black and pure white make the wheel useless, so lightness is clamped.
function toPickable(hex) {
  const { h, s, l } = hexToHsl(hex);
  return { h, s, l: Math.max(0.05, Math.min(0.88, l)) };
}

const colors = {
  hull: toPickable(getSavedShipColor()),
  accent: toPickable(getSavedAccentColor()),
  trail: toPickable(getSavedTrailColor()),
};
let colorTarget = 'hull';
let pick = { ...colors.hull };
brightnessEl.value = Math.round(pick.l * 100);

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
      const [r, g, b] = hslToRgbInt(h, s, pick.l);
      const i = (py * WHEEL_SIZE + px) * 4;
      d[i] = r; d[i + 1] = g; d[i + 2] = b; d[i + 3] = 255;
    }
  }
  wheelCtx.putImageData(img, 0, 0);
  const sx = WHEEL_R + Math.cos(pick.h * Math.PI / 180) * pick.s * (WHEEL_R - 2);
  const sy = WHEEL_R + Math.sin(pick.h * Math.PI / 180) * pick.s * (WHEEL_R - 2);
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
  const hex = hslToHex(pick.h, pick.s, pick.l);
  const slot = COLOR_SLOTS[colorTarget];
  slot.preview(hex);
  localStorage.setItem(slot.key, hex);
  // The glow always tracks the committed hull colour, not the live pick.
  const hullHex = hslToHex(colors.hull.h, colors.hull.s, colors.hull.l);
  colorPreviewEl.style.boxShadow = `0 0 18px ${hullHex}88`;
}

function setColorTarget(target) {
  colors[colorTarget] = { ...pick };
  colorTarget = target;
  pick = { ...colors[target] };
  brightnessEl.value = Math.round(pick.l * 100);
  for (const [slot, id] of Object.entries(TAB_IDS)) {
    el(id).classList.toggle('active', slot === target);
  }
  if (trailSwatchEl) trailSwatchEl.style.display = target === 'trail' ? '' : 'none';
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
  pick.h = ((Math.atan2(dy, dx) * 180 / Math.PI) + 360) % 360;
  pick.s = Math.min(1, dist / WHEEL_R);
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
  pick.l = brightnessEl.value / 100;
  drawWheel();
  applyColor();
});
zoomEl.addEventListener('input', () => {
  if (custScene) custScene.setZoom(parseFloat(zoomEl.value));
});

// ── Tabs (each one is a paid unlock) ────────────────────────────────────────
const showCustStatus = createStatusLine('custCrStatus', { ms: 3000, color: '#ff8a8a' });

for (const [feature, id] of Object.entries(TAB_IDS)) {
  onClick(id, async () => {
    if (isUnlocked(feature)) { setColorTarget(feature); return; }
    const result = await tryPurchaseUnlock(feature);
    if (!result.ok) {
      showCustStatus(`${feature} unlock costs ${UNLOCK_COSTS[feature]} ⬡ · ${result.msg}`);
      return;
    }
    showCustStatus(result.alreadyOwned ? 'Already unlocked!' : `Unlocked! −${UNLOCK_COSTS[feature]} ⬡`, '#66ff88');
    setColorTarget(feature);
  });
}

// ── Admin ship ──────────────────────────────────────────────────────────────
const showAdminShipStatus = createStatusLine('adminShipStatus', { ms: 4000, color: '#ff8a8a' });

el('btnBuyAdminShip')?.addEventListener('click', async () => {
  if (isUnlocked('admin_ship')) return;
  const token = getToken();
  if (!token) { showAdminShipStatus('Log in to purchase the Admin Ship'); return; }
  const cached = cachedCredits();
  if (cached < UNLOCK_COSTS.admin_ship) {
    showAdminShipStatus(`Need ${(UNLOCK_COSTS.admin_ship - cached).toLocaleString()} more ⬡ (you have ${cached.toLocaleString()} ⬡)`);
    return;
  }
  showAdminShipStatus('Processing…', '#c8e0ff');
  const result = await tryPurchaseUnlock('admin_ship');
  if (result.ok) {
    showAdminShipStatus(result.alreadyOwned ? 'Already owned!' : 'Admin Ship unlocked! Active next match.', '#66ff88');
    updateCustUnlockUI();
  } else {
    showAdminShipStatus(result.msg || 'Purchase failed');
  }
});

// ── Trail shape ─────────────────────────────────────────────────────────────
function setTrailShape(shape) {
  localStorage.setItem(TRAIL_SHAPE_KEY, shape);
  for (const btn of document.querySelectorAll('.trail-shape-btn')) {
    btn.classList.toggle('active', btn.dataset.shape === shape);
  }
}

el('trail-shape-picker').addEventListener('click', async (e) => {
  const btn = e.target.closest('.trail-shape-btn');
  if (!btn) return;
  if (isUnlocked('trail_shape')) { setTrailShape(btn.dataset.shape); return; }
  const result = await tryPurchaseUnlock('trail_shape');
  if (!result.ok) {
    showCustStatus(`Trail shapes cost ${UNLOCK_COSTS.trail_shape} ⬡ to unlock · ${result.msg}`);
    return;
  }
  showCustStatus(result.alreadyOwned ? 'Already unlocked!' : `Trail shapes unlocked! −${UNLOCK_COSTS.trail_shape} ⬡`, '#66ff88');
  setTrailShape(btn.dataset.shape);
});
setTrailShape(getSavedTrailShape());

if (trailSwatchEl) trailSwatchEl.style.background = getSavedTrailColor();
drawWheel();
applyColor();
colorPreviewEl.style.borderColor = hslToHex(colors.accent.h, colors.accent.s, colors.accent.l);

// ── Panel open / close ──────────────────────────────────────────────────────
onClick('btnCustomize', () => {
  if (custPanel.classList.contains('open')) {
    closeCustomization();
    return;
  }
  lobbyEl.classList.add('slide-left');
  custPanel.classList.add('open');
  document.body.classList.add('customization-open');
  if (!custScene) {
    custScene = initCustomizationScene(el('custCanvas'));
  } else {
    custScene.resume();
  }
  custScene.setColor(getSavedShipColor());
  custScene.setAccentColor(getSavedAccentColor());
});

export function closeCustomization() {
  custPanel.classList.remove('open');
  document.body.classList.remove('customization-open');
  lobbyEl.classList.remove('slide-left');
  if (custScene) custScene.pause();
}

onClick('btnSaveCustom', closeCustomization);

// ── Save / reset ────────────────────────────────────────────────────────────
const showSaveStatus = createStatusLine('saveColorsStatus', { ms: 3000 });

async function saveColorsToServer() {
  const token = getToken();
  if (!token) {
    showSaveStatus('Log in to save to account', '#ff8a8a');
    return;
  }
  const cached = cachedCredits();
  if (cached < COST_SAVE_COLORS) {
    showSaveStatus(`Need ${COST_SAVE_COLORS - cached} more ⬡ to save (${COST_SAVE_COLORS} ⬡ per save)`, '#ff8a8a', 3500);
    return;
  }
  showSaveStatus('Saving…', '#c8e0ff', 0);
  try {
    const spendRes = await fetch('/spaceships/api/credits/spend', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'Authorization': 'Bearer ' + token },
      body: JSON.stringify({ amount: COST_SAVE_COLORS, reason: 'save_colors' }),
    });
    const spendData = await spendRes.json();
    if (!spendData.ok) {
      showSaveStatus(spendData.error || 'Not enough ⬡', '#ff8a8a');
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
      showSaveStatus('Saved!', '#66ff88');
    } else {
      showSaveStatus(data.error || ('Server error ' + res.status), '#ff8a8a');
    }
  } catch {
    showSaveStatus('No connection to server', '#ff8a8a');
  }
}

onClick('btnSaveColors', saveColorsToServer);

const DEFAULT_COLORS = { hull: '#9fb6cc', accent: '#2a3340', trail: '#66ddff' };

onClick('btnResetColors', () => {
  for (const [slot, hex] of Object.entries(DEFAULT_COLORS)) {
    localStorage.setItem(COLOR_SLOTS[slot].key, hex);
    colors[slot] = toPickable(hex);
  }
  pick = { ...colors[colorTarget] };
  brightnessEl.value = Math.round(pick.l * 100);
  if (custScene) {
    custScene.setColor(DEFAULT_COLORS.hull);
    custScene.setAccentColor(DEFAULT_COLORS.accent);
  }
  colorPreviewEl.style.background = DEFAULT_COLORS.hull;
  colorPreviewEl.style.borderColor = DEFAULT_COLORS.accent;
  colorPreviewEl.style.boxShadow = `0 0 18px ${DEFAULT_COLORS.hull}88`;
  if (trailSwatchEl) trailSwatchEl.style.background = DEFAULT_COLORS.trail;
  drawWheel();
});
