const TOKEN_KEY = 'spaceships:token';
const USERNAME_KEY = 'spaceships:pilotName';
function parseJwtPayload(token) {
  try {
    return JSON.parse(atob(token.split('.')[1].replace(/-/g, '+').replace(/_/g, '/')));
  } catch { return null; }
}
function isTokenValid(token) {
  if (!token) return false;
  const p = parseJwtPayload(token);
  if (!p?.exp) return false;
  return Date.now() < (p.exp * 1000) - 60_000;
}
export function getToken() {
  return localStorage.getItem(TOKEN_KEY) || '';
}
export function clearToken() {
  localStorage.removeItem(TOKEN_KEY);
}
function saveToken(token, username, colors = {}) {
  localStorage.setItem(TOKEN_KEY, token);
  if (username) localStorage.setItem(USERNAME_KEY, username);
  localStorage.removeItem('spaceships:isGuest');
  if (colors.shipColor) localStorage.setItem('spaceships:shipColor', colors.shipColor);
  if (colors.accentColor) localStorage.setItem('spaceships:shipAccentColor', colors.accentColor);
}
function generateGuestName() {
  const chars = 'ABCDEFGHJKLMNPQRSTUVWXYZ23456789';
  let suffix = '';
  for (let i = 0; i < 6; i++) suffix += chars[Math.floor(Math.random() * chars.length)];
  return 'Pilot' + suffix;
}
export function requireAuth() {
  if (isTokenValid(getToken())) return Promise.resolve();
  return showAuthOverlay();
}
function showAuthOverlay() {
  return new Promise((resolve) => {
    const overlay = document.getElementById('auth-overlay');
    const loginPane = document.getElementById('auth-login');
    const regPane = document.getElementById('auth-register');
    const errorEl = document.getElementById('auth-error');
    setTimeout(() => {
      overlay.classList.remove('hidden');
    }, 4500);
    function clearError() { errorEl.textContent = ''; }
    function showError(msg) { errorEl.textContent = msg; }
    function dismiss() {
      overlay.classList.add('hidden');
      resolve();
    }
    document.getElementById('auth-tab-login').addEventListener('click', () => {
      loginPane.classList.remove('hidden');
      regPane.classList.add('hidden');
      document.getElementById('auth-tab-login').classList.add('active');
      document.getElementById('auth-tab-register').classList.remove('active');
      clearError();
    });
    document.getElementById('auth-tab-register').addEventListener('click', () => {
      regPane.classList.remove('hidden');
      loginPane.classList.add('hidden');
      document.getElementById('auth-tab-register').classList.add('active');
      document.getElementById('auth-tab-login').classList.remove('active');
      clearError();
    });
    document.getElementById('auth-login-form').addEventListener('submit', async (e) => {
      e.preventDefault();
      clearError();
      const username = document.getElementById('login-username').value.trim();
      const password = document.getElementById('login-password').value;
      const btn = document.getElementById('login-submit');
      btn.disabled = true;
      btn.textContent = 'Authenticating…';
      try {
        const res = await fetch('/spaceships/api/login', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ username, password }),
        });
        const data = await res.json();
        if (!data.ok) { showError(data.error); return; }
        saveToken(data.token, data.username, { shipColor: data.shipColor, accentColor: data.accentColor });
        const nameInput = document.getElementById('nameInput');
        if (nameInput) nameInput.value = data.username;
        dismiss();
      } catch {
        showError('Could not reach server — check your connection.');
      } finally {
        btn.disabled = false;
        btn.textContent = 'Launch';
      }
    });
    document.getElementById('auth-register-form').addEventListener('submit', async (e) => {
      e.preventDefault();
      clearError();
      const username = document.getElementById('reg-username').value.trim();
      const password = document.getElementById('reg-password').value;
      const confirm = document.getElementById('reg-confirm').value;
      if (password !== confirm) { showError('Passwords do not match'); return; }
      const btn = document.getElementById('register-submit');
      btn.disabled = true;
      btn.textContent = 'Registering…';
      try {
        const regRes = await fetch('/spaceships/api/register', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ username, password }),
        });
        const regData = await regRes.json();
        if (!regData.ok) { showError(regData.error); return; }
        const loginRes = await fetch('/spaceships/api/login', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ username, password }),
        });
        const loginData = await loginRes.json();
        if (!loginData.ok) { showError(loginData.error); return; }
        saveToken(loginData.token, loginData.username, { shipColor: loginData.shipColor, accentColor: loginData.accentColor });
        const nameInput = document.getElementById('nameInput');
        if (nameInput) nameInput.value = loginData.username;
        dismiss();
      } catch {
        showError('Could not reach server — check your connection.');
      } finally {
        btn.disabled = false;
        btn.textContent = 'Enlist';
      }
    });
    document.getElementById('auth-guest-btn').addEventListener('click', () => {
      const name = generateGuestName();
      localStorage.setItem(USERNAME_KEY, name);
      localStorage.setItem('spaceships:isGuest', '1');
      dismiss();
    });
  });
}