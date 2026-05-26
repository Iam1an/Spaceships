import Database from 'better-sqlite3';
import bcrypt from 'bcryptjs';
import jwt from 'jsonwebtoken';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const DB_PATH = path.join(__dirname, '..', 'pilots.db');

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

// Idempotent migrations — add new columns without touching existing data.
const migrations = [
  "ALTER TABLE pilots ADD COLUMN ship_color TEXT NOT NULL DEFAULT '#9fb6cc'",
  "ALTER TABLE pilots ADD COLUMN ship_accent_color TEXT NOT NULL DEFAULT '#2a3340'",
  "ALTER TABLE pilots ADD COLUMN total_kills INTEGER NOT NULL DEFAULT 0",
  "ALTER TABLE pilots ADD COLUMN total_deaths INTEGER NOT NULL DEFAULT 0",
  "ALTER TABLE pilots ADD COLUMN matches_won INTEGER NOT NULL DEFAULT 0",
  "ALTER TABLE pilots ADD COLUMN matches_lost INTEGER NOT NULL DEFAULT 0",
  "ALTER TABLE pilots ADD COLUMN bots_killed INTEGER NOT NULL DEFAULT 0",
  "ALTER TABLE pilots ADD COLUMN trial1_best REAL",
  "ALTER TABLE pilots ADD COLUMN trial2_best REAL",
  "ALTER TABLE pilots ADD COLUMN trial3_best REAL",
  "ALTER TABLE pilots ADD COLUMN trial4_best REAL",
];
for (const sql of migrations) {
  try { db.exec(sql); } catch {}
}

db.exec(`
  CREATE TABLE IF NOT EXISTS achievements (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    pilot_id  INTEGER NOT NULL REFERENCES pilots(id),
    type      TEXT NOT NULL,
    earned_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE(pilot_id, type)
  )
`);

// ── Rank thresholds ───────────────────────────────────────────────────────────

function computeRank(totalKills) {
  if (totalKills >= 500) return 'Admiral';
  if (totalKills >= 250) return 'Commander';
  if (totalKills >= 100) return 'Veteran';
  if (totalKills >= 50)  return 'Ace';
  if (totalKills >= 10)  return 'Pilot';
  return 'Cadet';
}

// ── Achievement definitions ───────────────────────────────────────────────────

const ACHIEVEMENT_DEFS = [
  // ── Kill milestones ───────────────────────────────────────────────────────────
  { type: 'first_kill',        label: 'First Blood',        icon: '🔫', desc: 'Get your first kill',                         check: p => p.total_kills >= 1 },
  { type: 'kills_10',          label: 'Sharpshooter',       icon: '🎯', desc: 'Reach 10 total kills',                        check: p => p.total_kills >= 10 },
  { type: 'kills_50',          label: 'Ace',                icon: '⚔',  desc: 'Reach 50 total kills',                        check: p => p.total_kills >= 50 },
  { type: 'kills_100',         label: 'Veteran',            icon: '🏆', desc: 'Reach 100 total kills',                       check: p => p.total_kills >= 100 },
  { type: 'kills_500',         label: 'Legend',             icon: '👑', desc: 'Reach 500 total kills',                       check: p => p.total_kills >= 500 },
  { type: 'kills_1000',        label: 'Living Weapon',      icon: '☠',  desc: 'Reach 1000 total kills',                      check: p => p.total_kills >= 1000 },
  // ── Best single-match kills ───────────────────────────────────────────────────
  { type: 'highscore_5',       label: 'Hot Streak',         icon: '🌡', desc: '5 kills in a single match',                   check: p => p.high_score >= 5 },
  { type: 'highscore_10',      label: 'Unstoppable',        icon: '🌩', desc: '10 kills in a single match',                  check: p => p.high_score >= 10 },
  { type: 'highscore_20',      label: 'Killing Machine',    icon: '💣', desc: '20 kills in a single match',                  check: p => p.high_score >= 20 },
  // ── Match wins ────────────────────────────────────────────────────────────────
  { type: 'first_win',         label: 'First Victory',      icon: '🥇', desc: 'Win your first match',                        check: p => p.matches_won >= 1 },
  { type: 'wins_5',            label: 'On a Roll',          icon: '🔥', desc: 'Win 5 matches',                               check: p => p.matches_won >= 5 },
  { type: 'wins_25',           label: 'Dominant Force',     icon: '💪', desc: 'Win 25 matches',                              check: p => p.matches_won >= 25 },
  { type: 'wins_50',           label: 'Warlord',            icon: '🎖', desc: 'Win 50 matches',                              check: p => p.matches_won >= 50 },
  // ── Matches played ────────────────────────────────────────────────────────────
  { type: 'matches_10',        label: 'Frequent Flyer',     icon: '🚀', desc: 'Play 10 matches',                             check: p => p.games_played >= 10 },
  { type: 'matches_50',        label: 'Battle-Hardened',    icon: '🛡', desc: 'Play 50 matches',                             check: p => p.games_played >= 50 },
  { type: 'matches_100',       label: 'Iron Pilot',         icon: '🔩', desc: 'Play 100 matches',                            check: p => p.games_played >= 100 },
  // ── Bot kills ─────────────────────────────────────────────────────────────────
  { type: 'bot_hunter',        label: 'Bot Hunter',         icon: '🤖', desc: 'Destroy 10 bots',                             check: p => p.bots_killed >= 10 },
  { type: 'bot_slayer',        label: 'Bot Slayer',         icon: '💀', desc: 'Destroy 100 bots',                            check: p => p.bots_killed >= 100 },
  { type: 'bot_exterminator',  label: 'Bot Exterminator',   icon: '🔧', desc: 'Destroy 500 bots',                            check: p => p.bots_killed >= 500 },
  // ── KDR ───────────────────────────────────────────────────────────────────────
  { type: 'kdr_positive',      label: 'Breaking Even',      icon: '⚖',  desc: 'Reach a 1.0+ KDR (min 10 deaths)',           check: p => p.total_deaths >= 10 && p.total_kills >= p.total_deaths },
  { type: 'kdr_2',             label: 'Skilled Hunter',     icon: '🦅', desc: 'Reach a 2.0+ KDR (min 10 deaths)',           check: p => p.total_deaths >= 10 && p.total_kills >= p.total_deaths * 2 },
  // ── Trials completion ─────────────────────────────────────────────────────────
  { type: 'trial1_complete',   label: 'Trial Runner',       icon: '⏱', desc: 'Complete Trial 1',                            check: p => p.trial1_best !== null },
  { type: 'trial2_complete',   label: 'Speed Seeker',       icon: '🌀', desc: 'Complete Trial 2',                            check: p => p.trial2_best !== null },
  { type: 'trial3_complete',   label: 'Precision Pilot',    icon: '🔮', desc: 'Complete Trial 3',                            check: p => p.trial3_best !== null },
  { type: 'trial4_complete',   label: 'Elite Racer',        icon: '🏁', desc: 'Complete Trial 4',                            check: p => p.trial4_best !== null },
  { type: 'all_trials',        label: 'Grand Champion',     icon: '🌟', desc: 'Complete all 4 trials',                       check: p => p.trial1_best !== null && p.trial2_best !== null && p.trial3_best !== null && p.trial4_best !== null },
  // ── Trial time records ────────────────────────────────────────────────────────
  { type: 'trial1_sub30',      label: 'Hypersonic',         icon: '💫', desc: 'Complete Trial 1 in under 30 seconds',        check: p => p.trial1_best !== null && p.trial1_best < 30 },
  { type: 'trial2_sub50',      label: 'Lightning Dash',     icon: '⚡', desc: 'Complete Trial 2 in under 50 seconds',        check: p => p.trial2_best !== null && p.trial2_best < 50 },
  { type: 'trial3_sub60',      label: 'Razor Edge',         icon: '🔪', desc: 'Complete Trial 3 in under 60 seconds',        check: p => p.trial3_best !== null && p.trial3_best < 60 },
  { type: 'trial4_sub70',      label: 'Beyond Limits',      icon: '🛸', desc: 'Complete Trial 4 in under 70 seconds',        check: p => p.trial4_best !== null && p.trial4_best < 70 },
  { type: 'speed_demon',       label: 'Speed Demon',        icon: '💨', desc: 'Complete any trial in under 30 seconds',      check: p => [p.trial1_best, p.trial2_best, p.trial3_best, p.trial4_best].some(t => t !== null && t < 30) },
];

const ACHIEVEMENT_MAP = Object.fromEntries(ACHIEVEMENT_DEFS.map(a => [a.type, a]));

// ── Prepared statements ───────────────────────────────────────────────────────

const stmtByUsername  = db.prepare('SELECT * FROM pilots WHERE username = ?');
const stmtById        = db.prepare('SELECT * FROM pilots WHERE id = ?');
const stmtInsert      = db.prepare('INSERT INTO pilots (username, hashed_password) VALUES (?, ?)');
const stmtIncrGames   = db.prepare('UPDATE pilots SET games_played = games_played + 1 WHERE id = ?');
const stmtUpdateScore = db.prepare(
  'UPDATE pilots SET high_score = ? WHERE id = ? AND ? > high_score'
);
const stmtSaveColors  = db.prepare(
  'UPDATE pilots SET ship_color = ?, ship_accent_color = ? WHERE id = ?'
);
const stmtMatchResult = db.prepare(`
  UPDATE pilots SET
    games_played = games_played + 1,
    total_kills  = total_kills  + ?,
    total_deaths = total_deaths + ?,
    matches_won  = matches_won  + ?,
    matches_lost = matches_lost + ?,
    bots_killed  = bots_killed  + ?,
    high_score   = CASE WHEN ? > high_score THEN ? ELSE high_score END
  WHERE id = ?
`);
const stmtUpdateRank = db.prepare('UPDATE pilots SET rank = ? WHERE id = ?');
const stmtTrialBests = [
  db.prepare('UPDATE pilots SET trial1_best = ? WHERE id = ? AND (trial1_best IS NULL OR trial1_best > ?)'),
  db.prepare('UPDATE pilots SET trial2_best = ? WHERE id = ? AND (trial2_best IS NULL OR trial2_best > ?)'),
  db.prepare('UPDATE pilots SET trial3_best = ? WHERE id = ? AND (trial3_best IS NULL OR trial3_best > ?)'),
  db.prepare('UPDATE pilots SET trial4_best = ? WHERE id = ? AND (trial4_best IS NULL OR trial4_best > ?)'),
];
const stmtAwardAch = db.prepare('INSERT OR IGNORE INTO achievements (pilot_id, type) VALUES (?, ?)');
const stmtGetAchs  = db.prepare('SELECT type, earned_at FROM achievements WHERE pilot_id = ? ORDER BY earned_at ASC');
const stmtLeaderboard = db.prepare(`
  SELECT username, rank, total_kills, total_deaths, matches_won, matches_lost, games_played, high_score
  FROM pilots
  ORDER BY total_kills DESC, matches_won DESC
  LIMIT 50
`);

// ── Internal helpers ──────────────────────────────────────────────────────────

function checkAndAwardAchievements(pilotId, pilot) {
  const existing = new Set(stmtGetAchs.all(pilotId).map(r => r.type));
  const newlyEarned = [];
  for (const def of ACHIEVEMENT_DEFS) {
    if (def.check(pilot) && !existing.has(def.type)) {
      stmtAwardAch.run(pilotId, def.type);
      newlyEarned.push({ type: def.type, label: def.label, icon: def.icon });
    }
  }
  return newlyEarned;
}

function kdrStr(kills, deaths) {
  return deaths > 0 ? (kills / deaths).toFixed(2) : kills.toFixed(2);
}

function apiError(message, status) {
  return Object.assign(new Error(message), { status });
}

// ── Register ──────────────────────────────────────────────────────────────────

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

// ── Login ─────────────────────────────────────────────────────────────────────

export async function loginPilot(username, password) {
  const pilot = stmtByUsername.get(String(username ?? ''));
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
    username:     pilot.username,
    rank:         pilot.rank,
    highScore:    pilot.high_score,
    gamesPlayed:  pilot.games_played,
    shipColor:    pilot.ship_color || '#9fb6cc',
    accentColor:  pilot.ship_accent_color || '#2a3340',
    totalKills:   pilot.total_kills,
    totalDeaths:  pilot.total_deaths,
    matchesWon:   pilot.matches_won,
    matchesLost:  pilot.matches_lost,
    botsKilled:   pilot.bots_killed,
    kdr:          kdrStr(pilot.total_kills, pilot.total_deaths),
    trial1Best:   pilot.trial1_best,
    trial2Best:   pilot.trial2_best,
    trial3Best:   pilot.trial3_best,
    trial4Best:   pilot.trial4_best,
  };
}

// ── Token verification ────────────────────────────────────────────────────────

export function verifyToken(token) {
  return jwt.verify(token, JWT_SECRET);
}

// ── Legacy stat helpers (still used by multiplayer endMatch fallback) ─────────

export function recordGamePlayed(pilotId) {
  stmtIncrGames.run(pilotId);
}

export function recordHighScore(pilotId, kills) {
  stmtUpdateScore.run(kills, pilotId, kills);
}

// ── Match result ──────────────────────────────────────────────────────────────

export function recordMatchResult(pilotId, { kills = 0, deaths = 0, won = null, botsKilled = 0 } = {}) {
  const wonInc  = won === true  ? 1 : 0;
  const lostInc = won === false ? 1 : 0;
  stmtMatchResult.run(kills, deaths, wonInc, lostInc, botsKilled, kills, kills, pilotId);
  const pilot = stmtById.get(pilotId);
  if (!pilot) return [];
  const newRank = computeRank(pilot.total_kills);
  if (newRank !== pilot.rank) stmtUpdateRank.run(newRank, pilotId);
  return checkAndAwardAchievements(pilotId, pilot);
}

// ── Trial time ────────────────────────────────────────────────────────────────

export function recordTrialTime(pilotId, trialNum, time) {
  const idx = Number(trialNum) - 1;
  if (idx < 0 || idx > 3) return [];
  stmtTrialBests[idx].run(time, pilotId, time);
  const pilot = stmtById.get(pilotId);
  if (!pilot) return [];
  return checkAndAwardAchievements(pilotId, pilot);
}

// ── Profile ───────────────────────────────────────────────────────────────────

export function getPilotProfile(username) {
  const pilot = stmtByUsername.get(String(username ?? ''));
  if (!pilot) return null;
  const achRows = stmtGetAchs.all(pilot.id);
  const earnedMap = new Map(achRows.map(r => [r.type, r.earned_at]));
  const achievements = ACHIEVEMENT_DEFS.map(def => ({
    type:     def.type,
    label:    def.label,
    icon:     def.icon,
    desc:     def.desc,
    earned:   earnedMap.has(def.type),
    earnedAt: earnedMap.get(def.type) ?? null,
  }));
  return {
    username:     pilot.username,
    rank:         pilot.rank,
    highScore:    pilot.high_score,
    gamesPlayed:  pilot.games_played,
    totalKills:   pilot.total_kills,
    totalDeaths:  pilot.total_deaths,
    matchesWon:   pilot.matches_won,
    matchesLost:  pilot.matches_lost,
    botsKilled:   pilot.bots_killed,
    kdr:          kdrStr(pilot.total_kills, pilot.total_deaths),
    trial1Best:   pilot.trial1_best,
    trial2Best:   pilot.trial2_best,
    trial3Best:   pilot.trial3_best,
    trial4Best:   pilot.trial4_best,
    achievements,
    createdAt:    pilot.created_at,
  };
}

// ── Leaderboard ───────────────────────────────────────────────────────────────

export function getLeaderboard() {
  return stmtLeaderboard.all().map((p, i) => ({
    position:     i + 1,
    username:     p.username,
    pilotRank:    p.rank,
    totalKills:   p.total_kills,
    totalDeaths:  p.total_deaths,
    matchesWon:   p.matches_won,
    matchesLost:  p.matches_lost,
    gamesPlayed:  p.games_played,
    highScore:    p.high_score,
    kdr:          kdrStr(p.total_kills, p.total_deaths),
  }));
}

// ── Startup backfill ──────────────────────────────────────────────────────────
// Awards any achievements that pilots already qualify for but were registered
// before the achievement system existed. Safe to run on every server start
// because INSERT OR IGNORE silently skips already-earned achievements.

const stmtAllPilots = db.prepare('SELECT * FROM pilots');

function backfillAchievements() {
  const pilots = stmtAllPilots.all();
  let total = 0;
  for (const pilot of pilots) {
    const newOnes = checkAndAwardAchievements(pilot.id, pilot);
    total += newOnes.length;
  }
  if (total > 0) console.log(`[achievements] backfill awarded ${total} achievement(s) to existing pilots`);
}

backfillAchievements();

// ── Colors ────────────────────────────────────────────────────────────────────

export function savePilotColors(pilotId, shipColor, accentColor) {
  const safeHull   = /^#[0-9a-fA-F]{6}$/.test(shipColor)   ? shipColor   : '#9fb6cc';
  const safeAccent = /^#[0-9a-fA-F]{6}$/.test(accentColor) ? accentColor : '#2a3340';
  stmtSaveColors.run(safeHull, safeAccent, pilotId);
}
