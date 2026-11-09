// The pilot callsign field on the main screen.
import { el } from './dom.js';
import { getToken } from '../auth.js';
import { containsProfanity } from '../filter.js';

const SAVED_NAME_KEY = 'spaceships:pilotName';

const nameInput = el('nameInput');
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

export function pilotName() {
  const name = (nameInput.value || '').trim();
  if (!name || containsProfanity(name)) return localStorage.getItem(SAVED_NAME_KEY) || 'Pilot';
  return name;
}

// Guests and logged-in pilots both get their callsign from the account, so the
// field becomes read-only once we know who they are.
export function lockCallsignToAccount() {
  const isGuest = localStorage.getItem('spaceships:isGuest') === '1';
  if (!isGuest && !getToken()) return;
  nameInput.value = localStorage.getItem(SAVED_NAME_KEY) || 'Pilot';
  nameInput.readOnly = true;
  nameInput.style.opacity = '0.55';
  nameInput.title = isGuest
    ? 'Guests cannot change their callsign — log in to choose a name'
    : 'Your callsign is your account username and cannot be changed here';
}
