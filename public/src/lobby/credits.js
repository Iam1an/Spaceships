// The ⬡ credit counter in the top-left corner.
import { el } from './dom.js';
import { getToken } from '../auth.js';

const CREDITS_KEY = 'spaceships:credits';
const creditsAmountEl = el('creditsAmount');

export function setCreditsDisplay(amount) {
  if (creditsAmountEl) {
    creditsAmountEl.textContent = Number.isFinite(amount)
      ? amount.toLocaleString()
      : '—';
  }
  localStorage.setItem(CREDITS_KEY, String(amount));
}

// The last balance the server told us about. Purchases check it before firing a
// request so an obviously-unaffordable click never hits the network.
export function cachedCredits() {
  return parseInt(localStorage.getItem(CREDITS_KEY) || '0', 10);
}

export async function refreshCredits() {
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

// Show the cached balance immediately so the counter is not blank while the
// real one is in flight.
const cached = parseInt(localStorage.getItem(CREDITS_KEY), 10);
if (!isNaN(cached)) setCreditsDisplay(cached);
