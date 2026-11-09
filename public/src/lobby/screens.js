// The lobby is a stack of sibling "screens"; exactly one is visible at a time.
import { el, onClick } from './dom.js';

export const lobbyEl = el('lobby');

const errorEl = el('lobby-error');

const screens = {
  main: el('lobby-main'),
  multi: el('lobby-multi'),
  create: el('lobby-create'),
  find: el('lobby-find'),
  room: el('lobby-room'),
  single: el('lobby-single'),
  tutorial: el('lobby-tutorial'),
  trials: el('lobby-trials'),
  campaign: el('lobby-campaign'),
};

document.body.classList.add('in-lobby');

export function showScreen(name) {
  for (const key of Object.keys(screens)) {
    screens[key].classList.toggle('hidden', key !== name);
  }
}

export function setError(text) {
  errorEl.textContent = text || '';
}

export function isLobbyVisible() {
  return !lobbyEl.classList.contains('hidden');
}

// Most menu buttons do exactly two things: clear the error line and swap
// screens. `routes` maps a button id to the screen it opens.
export function wireScreenNav(routes) {
  for (const [buttonId, screen] of Object.entries(routes)) {
    onClick(buttonId, () => {
      setError('');
      showScreen(screen);
    });
  }
}
