// Single player: tutorial, practice, time trials, and the campaign. Trials and
// missions unlock in sequence, gated on the personal-best / mission-beaten keys
// the game writes to localStorage when you finish one.
import { el, onClick } from './dom.js';
import { showScreen, setError, wireScreenNav } from './screens.js';
import { enterSoloGame } from './launch.js';

const TRIALS = [
  { id: 'btnTrial1', mode: 'trials' },
  { id: 'btnTrial2', mode: 'trials2', label: 'Trial 2', requires: 'spaceships:trial1Best' },
  { id: 'btnTrial3', mode: 'trials3', label: 'Trial 3', requires: 'spaceships:trial2Best' },
  { id: 'btnTrial4', mode: 'trials4', label: 'Trial 4', requires: 'spaceships:trial3Best' },
];

const MISSIONS = [
  { id: 'btnMission1', missionId: 1, beatKey: 'spaceships:campaign1Beat' },
  { id: 'btnMission2', missionId: 2, beatKey: 'spaceships:campaign2Beat', requires: 'spaceships:campaign1Beat' },
  { id: 'btnMission3', missionId: 3, beatKey: 'spaceships:campaign3Beat', requires: 'spaceships:campaign2Beat' },
];

function isUnlocked(def) {
  return !def.requires || localStorage.getItem(def.requires) !== null;
}

// A locked button stays clickable so it can keep its layout; the handler is
// what refuses.
function wireLaunchButton(def, launch) {
  onClick(def.id, () => {
    if (def.requires && el(def.id).classList.contains('locked')) return;
    launch();
  });
}

function refreshTrialButtons() {
  for (const def of TRIALS) {
    if (!def.requires) continue;
    const btn = el(def.id);
    if (!btn) continue;
    const unlocked = isUnlocked(def);
    btn.classList.toggle('locked', !unlocked);
    btn.textContent = unlocked ? def.label : `[LOCKED]  ${def.label}`;
  }
}

function refreshCampaignButtons() {
  for (const def of MISSIONS) {
    const btn = el(def.id);
    if (btn && def.requires) btn.classList.toggle('locked', !isUnlocked(def));
    const status = el(`mission${def.missionId}Status`);
    if (status) status.textContent = localStorage.getItem(def.beatKey) ? '✓ COMPLETED' : '';
  }
}

for (const def of TRIALS) {
  wireLaunchButton(def, () => enterSoloGame(def.mode));
}

for (const def of MISSIONS) {
  wireLaunchButton(def, () => enterSoloGame('campaign', { map: 'space', missionId: def.missionId }));
}

wireScreenNav({
  btnSingle: 'single',
  btnBackSingle: 'main',
  btnTutorial: 'tutorial',
  btnBackTutorial: 'single',
  btnBackTrials: 'single',
  btnBackCampaign: 'main',
});

// These two refresh their lock state on the way in.
onClick('btnTrials', () => {
  setError('');
  refreshTrialButtons();
  showScreen('trials');
});

onClick('btnCampaign', () => {
  setError('');
  refreshCampaignButtons();
  showScreen('campaign');
});

onClick('btnTrain', () => enterSoloGame('train'));
onClick('btnSkirmish', () => enterSoloGame('skirmish'));
onClick('btnTutorialKeys', () => enterSoloGame('tutorial', { noMouse: true, controlScheme: 'keyboard' }));
onClick('btnTutorialMouse', () => enterSoloGame('tutorial', { noMouse: false, controlScheme: 'mouse_keys' }));
