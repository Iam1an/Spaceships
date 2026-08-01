// Handing off from the lobby to the game. The option object passed to
// startGame() is the game's entry contract — keep its shape as is.
import { startGame } from '../main.js';
import { el, onClick } from './dom.js';
import { lobbyEl } from './screens.js';
import { controlScheme, noMouse, hardMode, hideSettingsPanel, SHOW_STATS_KEY } from './settings.js';
import { pilotName } from './pilot.js';
import { closeCustomization } from './customize.js';
import { socket } from './net.js';

// HUD widgets that are hidden while the lobby is up.
const HUD_IDS = ['reticle', 'healthbar', 'chargebar', 'boostbar', 'heatbar', 'missilehud', 'flarehud'];

function showHud() {
  lobbyEl.classList.add('hidden');
  document.body.classList.remove('in-lobby');
  if (localStorage.getItem(SHOW_STATS_KEY) !== '0') {
    el('hud-stats').style.display = '';
  }
  for (const id of HUD_IDS) el(id).style.display = '';
  hideSettingsPanel();
}

onClick('btnBackToMenu', () => {
  if (window.confirm('Leave match and return to menu?')) {
    window.location.reload();
  }
});

export function enterMultiplayerGame({ you, host, spawn, asteroids, players, map, botAssignments }) {
  closeCustomization();
  showHud();
  startGame({
    ws: socket(), you, host, spawn,
    asteroids, players,
    noMouse: noMouse(),
    controlScheme: controlScheme(),
    hardMode: hardMode(),
    pilotName: pilotName(),
    map,
    botAssignments,
  });
}

function soloSelectedMap() {
  return el('soloMapTerrain')?.checked ? 'terrain' : 'space';
}

export function enterSoloGame(mode, opts = {}) {
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
