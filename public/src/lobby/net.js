// The lobby's WebSocket transport. It owns the socket and dispatches incoming
// frames by `type`; the screens register what they care about. Message shapes
// are the server's wire format and must not change here.
import { getToken } from '../auth.js';
import { setError, isLobbyVisible } from './screens.js';

let ws = null;
const handlers = new Map();

export function onLobbyMessage(type, handler) {
  handlers.set(type, handler);
}

// The live socket is handed to the game on launch so the match keeps using it.
export function socket() {
  return ws;
}

export function send(obj) {
  if (ws && ws.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify(obj));
  }
}

export function disconnect() {
  if (ws) ws.close();
  ws = null;
}

export function connect() {
  if (ws && ws.readyState === WebSocket.OPEN) return Promise.resolve();
  if (ws) ws.close();
  if (!location.host || location.protocol === 'file:') {
    return Promise.reject(new Error('Open via http://localhost:4000 (run `npm start` first)'));
  }
  return new Promise((resolve, reject) => {
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const token = getToken();
    const wsUrl = `${proto}//${location.host}/ws${token ? '?token=' + encodeURIComponent(token) : ''}`;
    ws = new WebSocket(wsUrl);
    let openedOnce = false;
    let settled = false;
    ws.addEventListener('open', () => {
      openedOnce = true;
      settled = true;
      resolve();
    }, { once: true });
    ws.addEventListener('error', () => {
      if (!settled) {
        settled = true;
        reject(new Error('Could not reach server — is `npm start` running?'));
      }
    }, { once: true });
    ws.addEventListener('message', (e) => {
      let msg;
      try { msg = JSON.parse(e.data); } catch { return; }
      handlers.get(msg.type)?.(msg);
    });
    ws.addEventListener('close', () => {
      if (openedOnce && isLobbyVisible()) {
        setError('Disconnected from server');
      }
    });
  });
}
