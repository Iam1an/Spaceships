// The settings gear panel: control scheme, graphics/gameplay toggles, volume,
// the controls cheat sheet, and the logout row.
import { el, onClick, bindToggle, bindVolumeSlider } from './dom.js';
import { getToken, clearToken } from '../auth.js';

export const SHOW_STATS_KEY = 'spaceships:showStats';

// ── Control scheme ──────────────────────────────────────────────────────────
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
  // Migrate the old boolean "no mouse" preference, or guess from the device.
  selectedScheme = localStorage.getItem(SAVED_NO_MOUSE_KEY) === '1'
    ? 'keyboard'
    : detectDefaultScheme();
  localStorage.setItem(SAVED_SCHEME_KEY, selectedScheme);
}

function setScheme(scheme) {
  if (!SCHEMES.includes(scheme)) return;
  selectedScheme = scheme;
  localStorage.setItem(SAVED_SCHEME_KEY, scheme);
  for (const picker of document.querySelectorAll('[data-scheme-picker]')) {
    for (const btn of picker.querySelectorAll('button[data-scheme]')) {
      btn.classList.toggle('selected', btn.dataset.scheme === scheme);
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

export function controlScheme() {
  return selectedScheme;
}

export function noMouse() {
  return selectedScheme === 'keyboard';
}

// ── Toggles ─────────────────────────────────────────────────────────────────
const settingsBtn = el('settingsBtn');
const settingsPanel = el('settingsPanel');

const hardModeInput = bindToggle('hardModeInput', 'spaceships:hardMode');

bindToggle('cockpitViewInput', 'spaceships:viewMode', { on: 'first', off: 'third' });

const pixelFilterInput = bindToggle('pixelFilterInput', 'spaceships:pixelFilter', { defaultOn: true });

// Ultra graphics is read once at game start, so the toggle only takes effect on
// the next launch. Off by default, and it overrides the pixel filter.
function syncPixelAvailability() {
  const on = ultraGraphicsInput.checked;
  pixelFilterInput.disabled = on;
  pixelFilterInput.closest('label').style.opacity = on ? '0.4' : '';
}
const ultraGraphicsInput = bindToggle('ultraGraphicsInput', 'spaceships:ultraGraphics', {
  onChange: syncPixelAvailability,
});
syncPixelAvailability();

bindToggle('enemyTrailsInput', 'spaceships:enemyTrails', { defaultOn: true });

bindToggle('showStatsInput', SHOW_STATS_KEY, {
  defaultOn: true,
  onChange: (show) => {
    const hudStats = el('hud-stats');
    if (hudStats) hudStats.style.display = show ? '' : 'none';
  },
});

export function hardMode() {
  return hardModeInput.checked;
}

// ── Volume ──────────────────────────────────────────────────────────────────
bindVolumeSlider('musicVolumeInput', 'musicVolumeVal', 'spaceships:musicVolume', 0.6,
  (v) => window.__shipAudio?.setMusicVolume?.(v));
bindVolumeSlider('sfxVolumeInput', 'sfxVolumeVal', 'spaceships:sfxVolume', 1.0,
  (v) => window.__shipAudio?.setSfxVolume?.(v));

// ── Panel open/close ────────────────────────────────────────────────────────
settingsBtn.addEventListener('click', (e) => {
  e.stopPropagation();
  settingsPanel.classList.toggle('hidden');
});
document.addEventListener('click', (e) => {
  if (settingsPanel.classList.contains('hidden')) return;
  if (settingsPanel.contains(e.target) || settingsBtn.contains(e.target)) return;
  settingsPanel.classList.add('hidden');
});

export function hideSettingsPanel() {
  settingsPanel.classList.add('hidden');
}

// ── Controls cheat sheet ────────────────────────────────────────────────────
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

const btnControlsPopup = el('btnControlsPopup');
const controlsPopup = el('controls-popup');
btnControlsPopup.addEventListener('click', () => {
  const isHidden = controlsPopup.classList.toggle('hidden');
  btnControlsPopup.textContent = isHidden ? 'CONTROLS ▾' : 'CONTROLS ▴';
  if (!isHidden) {
    const entries = CONTROL_GUIDES[controlScheme()] || CONTROL_GUIDES.mouse_keys;
    controlsPopup.innerHTML = entries
      .map(([key, desc]) => `<div class="ctrl-row"><span class="ctrl-key">${key}</span><span class="ctrl-desc">${desc}</span></div>`)
      .join('');
  }
});

// ── Logout row ──────────────────────────────────────────────────────────────
function parseJwtUsername(token) {
  try {
    return JSON.parse(atob(token.split('.')[1].replace(/-/g, '+').replace(/_/g, '/'))).username || null;
  } catch { return null; }
}

export function refreshLogoutRow() {
  const token = getToken();
  const username = token ? parseJwtUsername(token) : null;
  const row = el('settingsLogoutRow');
  if (username) {
    el('pilotLabelName').textContent = username;
    row.classList.remove('hidden');
  } else {
    row.classList.add('hidden');
  }
}

onClick('btnLogout', () => {
  clearToken();
  hideSettingsPanel();
  location.reload();
});
