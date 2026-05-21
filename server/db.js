import Database from 'better-sqlite3';
import bcrypt from 'bcryptjs';
import jwt from 'jsonwebtoken';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const DB_PATH = path.join(__dirname, '..', 'pilots.db');

// Set JWT_SECRET in your environment: e.g. export JWT_SECRET="some-long-random-string"
export const JWT_SECRET = process.env.JWT_SECRET || 'spaceships-dev-secret-change-in-prod';

const db = new Database(DB_PATH);

db.exec(`
  CREATE TABLE IF NOT EXISTS pilots (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    username       TEXT    NOT NULL UNIQUE COLLATE NOCASE,
    hashed_password TEXT   NOT NULL,
    rank           TEXT    NOT NULL DEFAULT 'Cadet',
    high_score     INTEGER NOT NULL DEFAULT 0,
    games_played   INTEGER NOT NULL DEFAULT 0,
    created_at     INTEGER NOT NULL DEFAULT (unixepoch())
  )
`);

// Idempotent migration: add color columns for existing DBs.
try { db.exec("ALTER TABLE pilots ADD COLUMN ship_color TEXT NOT NULL DEFAULT '#9fb6cc'"); } catch {}
try { db.exec("ALTER TABLE pilots ADD COLUMN ship_accent_color TEXT NOT NULL DEFAULT '#2a3340'"); } catch {}

// Prepared statements — compiled once, reused on every call.
const stmtByUsername  = db.prepare('SELECT * FROM pilots WHERE username = ?');
const stmtInsert      = db.prepare('INSERT INTO pilots (username, hashed_password) VALUES (?, ?)');
const stmtIncrGames   = db.prepare('UPDATE pilots SET games_played = games_played + 1 WHERE id = ?');
const stmtUpdateScore = db.prepare(
  'UPDATE pilots SET high_score = ? WHERE id = ? AND ? > high_score'
);
const stmtSaveColors  = db.prepare(
  'UPDATE pilots SET ship_color = ?, ship_accent_color = ? WHERE id = ?'
);

// ── Register ────────────────────────────────────────────────────────────────

export async function registerPilot(username, password) {
  const clean = String(username ?? '').replace(/[^A-Za-z0-9_\-]/g, '').trim();
  if (clean.length < 3 || clean.length > 20) {
    throw apiError('Callsign must be 3–20 alphanumeric characters', 400);
  }
  if (!password || String(password).length < 6) {
    throw apiError('Password must be at least 6 characters', 400);
  }
  if (stmtByUsername.get(clean)) {
    throw apiError('Callsign already taken', 409);
  }
  const hash = await bcrypt.hash(String(password), 10);
  const { lastInsertRowid } = stmtInsert.run(clean, hash);
  return { id: Number(lastInsertRowid), username: clean };
}

// ── Login ────────────────────────────────────────────────────────────────────

export async function loginPilot(username, password) {
  const pilot = stmtByUsername.get(String(username ?? ''));
  // Use a constant-time compare even on "not found" to prevent user enumeration.
  const hash = pilot?.hashed_password ?? '$2b$10$invalidhashpaddingtoconstanttime';
  const ok = await bcrypt.compare(String(password ?? ''), hash);
  if (!pilot || !ok) throw apiError('Invalid callsign or password', 401);
  const token = jwt.sign(
    { id: pilot.id, username: pilot.username },
    JWT_SECRET,
    { expiresIn: '7d' }
  );
  return {
    token,
    username: pilot.username,
    rank: pilot.rank,
    highScore: pilot.high_score,
    gamesPlayed: pilot.games_played,
    shipColor: pilot.ship_color || '#9fb6cc',
    accentColor: pilot.ship_accent_color || '#2a3340',
  };
}

// ── Token verification ────────────────────────────────────────────────────────

export function verifyToken(token) {
  // Throws JsonWebTokenError / TokenExpiredError on failure.
  return jwt.verify(token, JWT_SECRET);
}

// ── Stats ────────────────────────────────────────────────────────────────────

export function recordGamePlayed(pilotId) {
  stmtIncrGames.run(pilotId);
}

export function recordHighScore(pilotId, kills) {
  stmtUpdateScore.run(kills, pilotId, kills);
}

// ── Colors ───────────────────────────────────────────────────────────────────

export function savePilotColors(pilotId, shipColor, accentColor) {
  const safeHull   = /^#[0-9a-fA-F]{6}$/.test(shipColor)   ? shipColor   : '#9fb6cc';
  const safeAccent = /^#[0-9a-fA-F]{6}$/.test(accentColor) ? accentColor : '#2a3340';
  stmtSaveColors.run(safeHull, safeAccent, pilotId);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

function apiError(message, status) {
  return Object.assign(new Error(message), { status });
}
