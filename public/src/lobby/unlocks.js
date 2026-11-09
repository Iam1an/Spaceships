// Paid customization unlocks. The server is the authority; localStorage is a
// cache so the UI can render the right badges before the fetch lands.
import { el } from './dom.js';
import { getToken } from '../auth.js';
import { setCreditsDisplay, cachedCredits } from './credits.js';
import { checkPendingAchievements } from './profile.js';

export const UNLOCK_COSTS = { hull: 250, accent: 400, trail: 500, trail_shape: 200, admin_ship: 125000 };

// Maps the /unlocks response fields onto the local feature flags.
const UNLOCK_FIELDS = {
  unlockHull: 'hull',
  unlockAccent: 'accent',
  unlockTrail: 'trail',
  unlockTrailShape: 'trail_shape',
  unlockAdminShip: 'admin_ship',
};

const COST_BADGE_IDS = {
  hull: 'hullTabCost',
  accent: 'accentTabCost',
  trail: 'trailTabCost',
  trail_shape: 'trailShapeCost',
};

export function isUnlocked(feature) {
  return localStorage.getItem(`spaceships:unlock_${feature}`) === '1';
}

function saveUnlockLocal(feature) {
  localStorage.setItem(`spaceships:unlock_${feature}`, '1');
}

export function updateCustUnlockUI() {
  for (const [feature, id] of Object.entries(COST_BADGE_IDS)) {
    const badge = el(id);
    if (!badge) continue;
    const owned = isUnlocked(feature);
    badge.textContent = owned ? '· ✓' : `· ${UNLOCK_COSTS[feature]} ⬡`;
    badge.style.color = owned ? '#66ff88' : '';
  }
  const adminBtn = el('btnBuyAdminShip');
  const adminCost = el('adminShipCost');
  if (adminBtn) {
    const owned = isUnlocked('admin_ship');
    adminBtn.textContent = owned ? '⚡ ADMIN SHIP · OWNED' : '⚡ ADMIN SHIP · 125,000 ⬡';
    adminBtn.style.opacity = owned ? '0.6' : '';
    adminBtn.style.cursor = owned ? 'default' : '';
    if (adminCost) {
      adminCost.textContent = owned ? '· ✓' : '· 125,000 ⬡';
      adminCost.style.color = owned ? '#66ff88' : '';
    }
  }
}

export async function refreshUnlocks() {
  const token = getToken();
  if (!token) return;
  try {
    const res = await fetch('/spaceships/api/unlocks', { headers: { 'Authorization': 'Bearer ' + token } });
    const data = await res.json();
    if (!data.ok) return;
    for (const [field, feature] of Object.entries(UNLOCK_FIELDS)) {
      if (data[field]) saveUnlockLocal(feature);
    }
    updateCustUnlockUI();
  } catch { }
}

export async function tryPurchaseUnlock(feature) {
  const cost = UNLOCK_COSTS[feature];
  if (!cost) return { ok: false, msg: 'Unknown feature' };
  const token = getToken();
  if (!token) return { ok: false, msg: 'Log in to unlock this feature' };
  if (isUnlocked(feature)) return { ok: true, alreadyOwned: true };
  const cached = cachedCredits();
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
