// Drive the lobby menus with a gamepad: d-pad/stick moves focus, A activates,
// B and Start go back. Runs only while the lobby is up.
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

function goBack() {
  const screen = getActiveScreen();
  const backId = screen && BACK_BTNS[screen.id];
  if (backId) document.getElementById(backId)?.click();
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
    // Short repeat once the stick is already held.
    navCooldown = (prevNavUp || prevNavDown) ? 0.12 : 0.28;
  }
  if (pressA && !prevA) {
    const el = document.activeElement;
    if (el && focusables.includes(el)) el.click();
  }
  if (pressB && !prevB) goBack();
  if (pressStart && !prevStart) {
    const screen = getActiveScreen();
    if (screen && screen.id !== 'lobby-main') goBack();
  }
  prevNavUp = navUp; prevNavDown = navDown;
  prevA = pressA; prevB = pressB; prevStart = pressStart;
  requestAnimationFrame(loop);
}

requestAnimationFrame(loop);
