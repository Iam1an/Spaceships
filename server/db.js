import Database from 'better-sqlite3';
import bcrypt from 'bcryptjs';
import jwt from 'jsonwebtoken';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const DB_PATH = path.join(__dirname, '..', 'pilots.db');

if (process.env.NODE_ENV === 'production' && !process.env.JWT_SECRET) {
  throw new Error("FATAL: JWT_SECRET must be defined in production environment!");
}
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
  "ALTER TABLE pilots ADD COLUMN credits INTEGER NOT NULL DEFAULT 0",
  "ALTER TABLE pilots ADD COLUMN unlock_colors INTEGER NOT NULL DEFAULT 0",
  "ALTER TABLE pilots ADD COLUMN unlock_trail INTEGER NOT NULL DEFAULT 0",
  "ALTER TABLE pilots ADD COLUMN unlock_hull INTEGER NOT NULL DEFAULT 0",
  "ALTER TABLE pilots ADD COLUMN unlock_accent INTEGER NOT NULL DEFAULT 0",
  "ALTER TABLE pilots ADD COLUMN unlock_trail_shape INTEGER NOT NULL DEFAULT 0",
  "ALTER TABLE pilots ADD COLUMN campaign1_best_lives INTEGER",
  "ALTER TABLE pilots ADD COLUMN campaign2_best_lives INTEGER",
  "ALTER TABLE pilots ADD COLUMN campaign3_best_lives INTEGER",
  "ALTER TABLE pilots ADD COLUMN campaign_boss_kills INTEGER NOT NULL DEFAULT 0",
  "ALTER TABLE pilots ADD COLUMN campaign_total_completions INTEGER NOT NULL DEFAULT 0",
  "ALTER TABLE pilots ADD COLUMN unlock_admin_ship INTEGER NOT NULL DEFAULT 0",
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
try { db.exec("ALTER TABLE achievements ADD COLUMN credited INTEGER NOT NULL DEFAULT 0"); } catch {}

db.exec(`
  CREATE TABLE IF NOT EXISTS credit_transactions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    pilot_id   INTEGER NOT NULL REFERENCES pilots(id),
    amount     INTEGER NOT NULL,
    reason     TEXT    NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
  )
`);

// ── Rank thresholds ───────────────────────────────────────────────────────────

function computeRank(totalKills) {
  if (totalKills >= 10000) return 'Grand Admiral';
  if (totalKills >= 5000)  return 'Fleet Admiral';
  if (totalKills >= 2500)  return 'Vice Admiral';
  if (totalKills >= 1500)  return 'Rear Admiral';
  if (totalKills >= 1000)  return 'Commodore';
  if (totalKills >= 750)   return 'Captain';
  if (totalKills >= 500)   return 'Admiral';
  if (totalKills >= 250)   return 'Commander';
  if (totalKills >= 100)   return 'Veteran';
  if (totalKills >= 50)    return 'Ace';
  if (totalKills >= 25)    return 'Flight Officer';
  if (totalKills >= 10)    return 'Pilot';
  if (totalKills >= 5)     return 'Recruit';
  return 'Cadet';
}

// ── Achievement definitions ───────────────────────────────────────────────────

// ── Credit rates ──────────────────────────────────────────────────────────────
export const CR_PER_KILL     = 5;   // player kills in any match
export const CR_PER_BOT_KILL = 2;   // bot kills in solo modes
export const CR_WIN_BONUS    = 50;  // flat bonus for winning a match

const ACHIEVEMENT_DEFS = [
  // ── Kill milestones ───────────────────────────────────────────────────────────
  { type: 'first_kill',        label: 'First Blood',        icon: '🔫', desc: 'Get your first kill',                         reward:   100, check: p => p.total_kills >= 1,    progress: p => ({ current: Math.min(p.total_kills, 1),    target: 1 }) },
  { type: 'kills_10',          label: 'Sharpshooter',       icon: '🎯', desc: 'Reach 10 total kills',                        reward:   150, check: p => p.total_kills >= 10,   progress: p => ({ current: Math.min(p.total_kills, 10),   target: 10 }) },
  { type: 'kills_50',          label: 'Ace',                icon: '⚔',  desc: 'Reach 50 total kills',                        reward:   350, check: p => p.total_kills >= 50,   progress: p => ({ current: Math.min(p.total_kills, 50),   target: 50 }) },
  { type: 'kills_100',         label: 'Veteran',            icon: '🏆', desc: 'Reach 100 total kills',                       reward:   600, check: p => p.total_kills >= 100,  progress: p => ({ current: Math.min(p.total_kills, 100),  target: 100 }) },
  { type: 'kills_500',         label: 'Legend',             icon: '👑', desc: 'Reach 500 total kills',                       reward:  2500, check: p => p.total_kills >= 500,  progress: p => ({ current: Math.min(p.total_kills, 500),  target: 500 }) },
  { type: 'kills_1000',        label: 'Living Weapon',      icon: '☠',  desc: 'Reach 1000 total kills',                      reward:  5000, check: p => p.total_kills >= 1000,  progress: p => ({ current: Math.min(p.total_kills, 1000),  target: 1000 }) },
  { type: 'kills_2500',        label: 'Destroyer',          icon: '🌑', desc: 'Reach 2500 total kills',                      reward: 10000, check: p => p.total_kills >= 2500,  progress: p => ({ current: Math.min(p.total_kills, 2500),  target: 2500 }) },
  { type: 'kills_5000',        label: 'Apocalypse',         icon: '🌋', desc: 'Reach 5000 total kills',                      reward: 25000, check: p => p.total_kills >= 5000,  progress: p => ({ current: Math.min(p.total_kills, 5000),  target: 5000 }) },
  { type: 'kills_10000',       label: 'God of War',         icon: '🔱', desc: 'Reach 10000 total kills',                     reward: 50000, check: p => p.total_kills >= 10000, progress: p => ({ current: Math.min(p.total_kills, 10000), target: 10000 }) },
  // ── Best single-match kills ───────────────────────────────────────────────────
  { type: 'highscore_5',       label: 'Hot Streak',         icon: '🌡', desc: '5 kills in a single match',                   reward:   150, check: p => p.high_score >= 5,     progress: p => ({ current: Math.min(p.high_score, 5),   target: 5 }) },
  { type: 'highscore_10',      label: 'Unstoppable',        icon: '🌩', desc: '10 kills in a single match',                  reward:   300, check: p => p.high_score >= 10,    progress: p => ({ current: Math.min(p.high_score, 10),  target: 10 }) },
  { type: 'highscore_20',      label: 'Killing Machine',    icon: '💣', desc: '20 kills in a single match',                  reward:   750, check: p => p.high_score >= 20,    progress: p => ({ current: Math.min(p.high_score, 20),  target: 20 }) },
  { type: 'highscore_30',      label: 'Rampage',            icon: '🌪', desc: '30 kills in a single match',                  reward:  1500, check: p => p.high_score >= 30,    progress: p => ({ current: Math.min(p.high_score, 30),  target: 30 }) },
  { type: 'highscore_50',      label: 'Obliterator',        icon: '💥', desc: '50 kills in a single match',                  reward:  3500, check: p => p.high_score >= 50,    progress: p => ({ current: Math.min(p.high_score, 50),  target: 50 }) },
  // ── Match wins ────────────────────────────────────────────────────────────────
  { type: 'first_win',         label: 'First Victory',      icon: '🥇', desc: 'Win your first match',                        reward:   200, check: p => p.matches_won >= 1,    progress: p => ({ current: Math.min(p.matches_won, 1),   target: 1 }) },
  { type: 'wins_5',            label: 'On a Roll',          icon: '🔥', desc: 'Win 5 matches',                               reward:   400, check: p => p.matches_won >= 5,    progress: p => ({ current: Math.min(p.matches_won, 5),   target: 5 }) },
  { type: 'wins_25',           label: 'Dominant Force',     icon: '💪', desc: 'Win 25 matches',                              reward:  1500, check: p => p.matches_won >= 25,   progress: p => ({ current: Math.min(p.matches_won, 25),  target: 25 }) },
  { type: 'wins_50',           label: 'Warlord',            icon: '🎖', desc: 'Win 50 matches',                              reward:  3000, check: p => p.matches_won >= 50,   progress: p => ({ current: Math.min(p.matches_won, 50),  target: 50 }) },
  { type: 'wins_100',          label: 'Conqueror',          icon: '🏅', desc: 'Win 100 matches',                             reward:  5000, check: p => p.matches_won >= 100,  progress: p => ({ current: Math.min(p.matches_won, 100), target: 100 }) },
  { type: 'wins_250',          label: 'Supreme Commander',  icon: '🌠', desc: 'Win 250 matches',                             reward: 15000, check: p => p.matches_won >= 250,  progress: p => ({ current: Math.min(p.matches_won, 250), target: 250 }) },
  { type: 'wins_500',          label: 'Overlord',           icon: '🏛', desc: 'Win 500 matches',                             reward: 30000, check: p => p.matches_won >= 500,  progress: p => ({ current: Math.min(p.matches_won, 500), target: 500 }) },
  // ── Matches played ────────────────────────────────────────────────────────────
  { type: 'matches_10',        label: 'Frequent Flyer',     icon: '🚀', desc: 'Play 10 matches',                             reward:   100, check: p => p.games_played >= 10,   progress: p => ({ current: Math.min(p.games_played, 10),   target: 10 }) },
  { type: 'matches_50',        label: 'Battle-Hardened',    icon: '🛡', desc: 'Play 50 matches',                             reward:   500, check: p => p.games_played >= 50,   progress: p => ({ current: Math.min(p.games_played, 50),   target: 50 }) },
  { type: 'matches_100',       label: 'Iron Pilot',         icon: '🔩', desc: 'Play 100 matches',                            reward:  1500, check: p => p.games_played >= 100,  progress: p => ({ current: Math.min(p.games_played, 100),  target: 100 }) },
  { type: 'matches_250',       label: 'Seasoned Pilot',     icon: '🗺', desc: 'Play 250 matches',                            reward:  2500, check: p => p.games_played >= 250,  progress: p => ({ current: Math.min(p.games_played, 250),  target: 250 }) },
  { type: 'matches_500',       label: 'War Machine',        icon: '🌍', desc: 'Play 500 matches',                            reward:  6000, check: p => p.games_played >= 500,  progress: p => ({ current: Math.min(p.games_played, 500),  target: 500 }) },
  { type: 'matches_1000',      label: 'Eternal Pilot',      icon: '🌌', desc: 'Play 1000 matches',                           reward: 15000, check: p => p.games_played >= 1000, progress: p => ({ current: Math.min(p.games_played, 1000), target: 1000 }) },
  // ── Bot kills ─────────────────────────────────────────────────────────────────
  { type: 'bot_hunter',        label: 'Bot Hunter',         icon: '🤖', desc: 'Destroy 10 bots',                             reward:   100, check: p => p.bots_killed >= 10,    progress: p => ({ current: Math.min(p.bots_killed, 10),    target: 10 }) },
  { type: 'bot_slayer',        label: 'Bot Slayer',         icon: '💀', desc: 'Destroy 100 bots',                            reward:   500, check: p => p.bots_killed >= 100,   progress: p => ({ current: Math.min(p.bots_killed, 100),   target: 100 }) },
  { type: 'bot_exterminator',  label: 'Bot Exterminator',   icon: '🔧', desc: 'Destroy 500 bots',                            reward:  2000, check: p => p.bots_killed >= 500,   progress: p => ({ current: Math.min(p.bots_killed, 500),   target: 500 }) },
  { type: 'bot_overlord',      label: 'Bot Overlord',       icon: '🦾', desc: 'Destroy 1000 bots',                           reward:  4000, check: p => p.bots_killed >= 1000,  progress: p => ({ current: Math.min(p.bots_killed, 1000),  target: 1000 }) },
  { type: 'bot_apocalypse',    label: 'Bot Apocalypse',     icon: '🤯', desc: 'Destroy 5000 bots',                           reward: 15000, check: p => p.bots_killed >= 5000,  progress: p => ({ current: Math.min(p.bots_killed, 5000),  target: 5000 }) },
  // ── KDR ───────────────────────────────────────────────────────────────────────
  { type: 'kdr_positive',      label: 'Breaking Even',      icon: '⚖',  desc: 'Reach a 1.0+ KDR (min 10 deaths)',           reward:   750, check: p => p.total_deaths >= 10 && p.total_kills >= p.total_deaths,          progress: null },
  { type: 'kdr_2',             label: 'Skilled Hunter',     icon: '🦅', desc: 'Reach a 2.0+ KDR (min 10 deaths)',           reward:  2000, check: p => p.total_deaths >= 10 && p.total_kills >= p.total_deaths * 2,      progress: null },
  { type: 'kdr_3',             label: 'Deadeye',            icon: '🐺', desc: 'Reach a 3.0+ KDR (min 10 deaths)',           reward:  3500, check: p => p.total_deaths >= 10 && p.total_kills >= p.total_deaths * 3,      progress: null },
  { type: 'kdr_5',             label: 'Ghost',              icon: '👁', desc: 'Reach a 5.0+ KDR (min 10 deaths)',           reward:  8000, check: p => p.total_deaths >= 10 && p.total_kills >= p.total_deaths * 5,      progress: null },
  // ── Deaths (perseverance) ─────────────────────────────────────────────────────
  { type: 'deaths_10',         label: 'First Casualty',     icon: '🩹', desc: 'Die 10 times',                               reward:    25, check: p => p.total_deaths >= 10,  progress: p => ({ current: Math.min(p.total_deaths, 10),  target: 10 }) },
  { type: 'deaths_100',        label: 'Crash Test Pilot',   icon: '⚰', desc: 'Die 100 times',                              reward:   100, check: p => p.total_deaths >= 100, progress: p => ({ current: Math.min(p.total_deaths, 100), target: 100 }) },
  { type: 'deaths_500',        label: 'Sacrifice',          icon: '🕯', desc: 'Die 500 times',                              reward:   250, check: p => p.total_deaths >= 500, progress: p => ({ current: Math.min(p.total_deaths, 500), target: 500 }) },
  // ── Losses (perseverance) ─────────────────────────────────────────────────────
  { type: 'losses_10',         label: 'Learning Curve',     icon: '📉', desc: 'Lose 10 matches',                            reward:    50, check: p => p.matches_lost >= 10,  progress: p => ({ current: Math.min(p.matches_lost, 10),  target: 10 }) },
  { type: 'losses_50',         label: 'Punching Bag',       icon: '😤', desc: 'Lose 50 matches',                            reward:   150, check: p => p.matches_lost >= 50,  progress: p => ({ current: Math.min(p.matches_lost, 50),  target: 50 }) },
  // ── Trials completion ─────────────────────────────────────────────────────────
  { type: 'trial1_complete',   label: 'Trial Runner',       icon: '⏱', desc: 'Complete Trial 1',                            reward:   300, check: p => p.trial1_best !== null, progress: null },
  { type: 'trial2_complete',   label: 'Speed Seeker',       icon: '🌀', desc: 'Complete Trial 2',                            reward:   400, check: p => p.trial2_best !== null, progress: null },
  { type: 'trial3_complete',   label: 'Precision Pilot',    icon: '🔮', desc: 'Complete Trial 3',                            reward:   600, check: p => p.trial3_best !== null, progress: null },
  { type: 'trial4_complete',   label: 'Elite Racer',        icon: '🏁', desc: 'Complete Trial 4',                            reward:   800, check: p => p.trial4_best !== null, progress: null },
  { type: 'all_trials',        label: 'Grand Champion',     icon: '🌟', desc: 'Complete all 4 trials',                       reward:  2500, check: p => p.trial1_best !== null && p.trial2_best !== null && p.trial3_best !== null && p.trial4_best !== null, progress: p => ({ current: [p.trial1_best, p.trial2_best, p.trial3_best, p.trial4_best].filter(t => t !== null).length, target: 4 }) },
  // ── Trial time records ────────────────────────────────────────────────────────
  { type: 'trial1_sub30',      label: 'Hypersonic',         icon: '💫', desc: 'Complete Trial 1 in under 30 seconds',        reward:  1500, check: p => p.trial1_best !== null && p.trial1_best < 30, progress: p => p.trial1_best !== null ? { current: p.trial1_best, target: 30, isTime: true } : null },
  { type: 'trial2_sub50',      label: 'Lightning Dash',     icon: '⚡', desc: 'Complete Trial 2 in under 50 seconds',        reward:  2000, check: p => p.trial2_best !== null && p.trial2_best < 50, progress: p => p.trial2_best !== null ? { current: p.trial2_best, target: 50, isTime: true } : null },
  { type: 'trial3_sub60',      label: 'Razor Edge',         icon: '🔪', desc: 'Complete Trial 3 in under 60 seconds',        reward:  2500, check: p => p.trial3_best !== null && p.trial3_best < 60, progress: p => p.trial3_best !== null ? { current: p.trial3_best, target: 60, isTime: true } : null },
  { type: 'trial4_sub70',      label: 'Beyond Limits',      icon: '🛸', desc: 'Complete Trial 4 in under 70 seconds',        reward:  3000, check: p => p.trial4_best !== null && p.trial4_best < 70, progress: p => p.trial4_best !== null ? { current: p.trial4_best, target: 70, isTime: true } : null },
  { type: 'speed_demon',       label: 'Speed Demon',        icon: '💨', desc: 'Complete any trial in under 30 seconds',      reward:  5000, check: p => [p.trial1_best, p.trial2_best, p.trial3_best, p.trial4_best].some(t => t !== null && t < 30), progress: null },
  // ── Combo / milestone ────────────────────────────────────────────────────────
  { type: 'grinder',           label: 'Grinder',            icon: '⚙',  desc: '200+ kills with a 2.0+ KDR (min 10 deaths)',  reward:  5000, check: p => p.total_deaths >= 10 && p.total_kills >= 200 && p.total_kills >= p.total_deaths * 2, progress: null },
  { type: 'well_rounded',      label: 'Well Rounded',       icon: '🌐', desc: 'Play 50 matches, get 50 kills, complete Trial 1', reward: 3000, check: p => p.games_played >= 50 && p.total_kills >= 50 && p.trial1_best !== null, progress: p => ({ current: (p.games_played >= 50 ? 1 : 0) + (p.total_kills >= 50 ? 1 : 0) + (p.trial1_best !== null ? 1 : 0), target: 3 }) },
  { type: 'veteran_touch',     label: "Veteran's Touch",    icon: '🗡', desc: '500+ kills and 100+ match wins',               reward:  8000, check: p => p.total_kills >= 500 && p.matches_won >= 100, progress: p => ({ current: (p.total_kills >= 500 ? 1 : 0) + (p.matches_won >= 100 ? 1 : 0), target: 2 }) },
  { type: 'perfectionist',     label: 'Perfectionist',      icon: '🎭', desc: 'Complete all 4 trials with a 2.0+ KDR',       reward:  7500, check: p => p.trial1_best !== null && p.trial2_best !== null && p.trial3_best !== null && p.trial4_best !== null && p.total_deaths >= 10 && p.total_kills >= p.total_deaths * 2, progress: p => ({ current: [p.trial1_best !== null, p.trial2_best !== null, p.trial3_best !== null, p.trial4_best !== null, p.total_deaths >= 10 && p.total_kills >= p.total_deaths * 2].filter(Boolean).length, target: 5 }) },
  { type: 'jack_of_all',       label: 'Jack of All Trades', icon: '🃏', desc: '100+ player kills, 100+ bot kills, 10+ wins',  reward:  4000, check: p => p.total_kills >= 100 && p.bots_killed >= 100 && p.matches_won >= 10, progress: p => ({ current: (p.total_kills >= 100 ? 1 : 0) + (p.bots_killed >= 100 ? 1 : 0) + (p.matches_won >= 10 ? 1 : 0), target: 3 }) },
  { type: 'the_grind',         label: 'The Grind',          icon: '⛏', desc: '1000 matches, 1000 kills, 1000 bots destroyed', reward: 20000, check: p => p.games_played >= 1000 && p.total_kills >= 1000 && p.bots_killed >= 1000, progress: p => ({ current: (p.games_played >= 1000 ? 1 : 0) + (p.total_kills >= 1000 ? 1 : 0) + (p.bots_killed >= 1000 ? 1 : 0), target: 3 }) },
  // ── Customization ─────────────────────────────────────────────────────────────
  { type: 'high_roller',       label: 'High Roller',        icon: '💎', desc: 'Unlock all customization features',            reward:  1000, check: p => !!p.unlock_hull && !!p.unlock_accent && !!p.unlock_trail && !!p.unlock_trail_shape, progress: p => ({ current: [p.unlock_hull, p.unlock_accent, p.unlock_trail, p.unlock_trail_shape].filter(Boolean).length, target: 4 }) },
  // ── Campaign — mission completions ───────────────────────────────────────────
  { type: 'campaign_m1_complete',  label: 'Ironclad',           icon: '🎖', desc: 'Complete Mission 1: Operation Ironclad',                     reward:  1500, check: p => p.campaign1_best_lives !== null, progress: null },
  { type: 'campaign_m2_complete',  label: 'Stormbreaker',       icon: '⛈', desc: 'Complete Mission 2: Operation Stormfront',                   reward:  3000, check: p => p.campaign2_best_lives !== null, progress: null },
  { type: 'campaign_m3_complete',  label: 'Final Victor',       icon: '🔱', desc: 'Complete Mission 3: Final Siege',                            reward:  6000, check: p => p.campaign3_best_lives !== null, progress: null },
  { type: 'campaign_all_complete', label: 'Grand Commander',    icon: '👑', desc: 'Complete all 3 campaign missions',                           reward: 15000, check: p => p.campaign1_best_lives !== null && p.campaign2_best_lives !== null && p.campaign3_best_lives !== null, progress: p => ({ current: [p.campaign1_best_lives, p.campaign2_best_lives, p.campaign3_best_lives].filter(v => v !== null).length, target: 3 }) },
  // ── Campaign — flawless runs (no deaths = 3 lives remaining) ─────────────────
  { type: 'campaign_m1_flawless',  label: 'Ghost Pilot I',      icon: '👻', desc: 'Complete Mission 1 without dying',                          reward:  3000, check: p => p.campaign1_best_lives >= 3, progress: null },
  { type: 'campaign_m2_flawless',  label: 'Ghost Pilot II',     icon: '🕶', desc: 'Complete Mission 2 without dying',                          reward:  6000, check: p => p.campaign2_best_lives >= 3, progress: null },
  { type: 'campaign_m3_flawless',  label: 'Untouchable',        icon: '⚡', desc: 'Complete Mission 3 without dying',                          reward: 12000, check: p => p.campaign3_best_lives >= 3, progress: null },
  { type: 'campaign_all_flawless', label: 'Immaculate',         icon: '💎', desc: 'Complete all 3 campaign missions without dying',             reward: 30000, check: p => p.campaign1_best_lives >= 3 && p.campaign2_best_lives >= 3 && p.campaign3_best_lives >= 3, progress: p => ({ current: [p.campaign1_best_lives, p.campaign2_best_lives, p.campaign3_best_lives].filter(v => v >= 3).length, target: 3 }) },
  // ── Campaign — boss kills ─────────────────────────────────────────────────────
  { type: 'campaign_boss_first',   label: 'Capital Punishment', icon: '💥', desc: 'Destroy the Capital Ship for the first time',               reward:  2000, check: p => p.campaign_boss_kills >= 1,   progress: p => ({ current: Math.min(p.campaign_boss_kills, 1),   target: 1 }) },
  { type: 'campaign_boss_5',       label: 'Fleet Slayer',       icon: '🚀', desc: 'Destroy the Capital Ship 5 times',                          reward:  5000, check: p => p.campaign_boss_kills >= 5,   progress: p => ({ current: Math.min(p.campaign_boss_kills, 5),   target: 5 }) },
  { type: 'campaign_boss_10',      label: 'Dreadnought Hunter', icon: '🎯', desc: 'Destroy the Capital Ship 10 times',                         reward: 10000, check: p => p.campaign_boss_kills >= 10,  progress: p => ({ current: Math.min(p.campaign_boss_kills, 10),  target: 10 }) },
  { type: 'campaign_boss_25',      label: 'Capital Executioner',icon: '☠',  desc: 'Destroy the Capital Ship 25 times',                         reward: 20000, check: p => p.campaign_boss_kills >= 25,  progress: p => ({ current: Math.min(p.campaign_boss_kills, 25),  target: 25 }) },
  { type: 'campaign_boss_50',      label: 'Fleet Annihilator',  icon: '🔥', desc: 'Destroy the Capital Ship 50 times',                         reward: 40000, check: p => p.campaign_boss_kills >= 50,  progress: p => ({ current: Math.min(p.campaign_boss_kills, 50),  target: 50 }) },
  // ── Campaign — total completions ──────────────────────────────────────────────
  { type: 'campaign_runs_5',       label: 'Seasoned Operative', icon: '✈',  desc: 'Complete any campaign mission 5 times total',               reward:  2500, check: p => p.campaign_total_completions >= 5,   progress: p => ({ current: Math.min(p.campaign_total_completions, 5),   target: 5 }) },
  { type: 'campaign_runs_10',      label: 'Iron Will',          icon: '🛡', desc: 'Complete any campaign mission 10 times total',              reward:  5000, check: p => p.campaign_total_completions >= 10,  progress: p => ({ current: Math.min(p.campaign_total_completions, 10),  target: 10 }) },
  { type: 'campaign_runs_25',      label: 'Veteran Commander',  icon: '⭐', desc: 'Complete any campaign mission 25 times total',              reward: 12000, check: p => p.campaign_total_completions >= 25,  progress: p => ({ current: Math.min(p.campaign_total_completions, 25),  target: 25 }) },
  { type: 'campaign_runs_50',      label: 'Elite Ace',          icon: '🌟', desc: 'Complete any campaign mission 50 times total',              reward: 25000, check: p => p.campaign_total_completions >= 50,  progress: p => ({ current: Math.min(p.campaign_total_completions, 50),  target: 50 }) },
  { type: 'campaign_runs_100',     label: 'Campaign Legend',    icon: '🏆', desc: 'Complete any campaign mission 100 times total',             reward: 60000, check: p => p.campaign_total_completions >= 100, progress: p => ({ current: Math.min(p.campaign_total_completions, 100), target: 100 }) },
  // ── Campaign — cross-mode combos ──────────────────────────────────────────────
  { type: 'campaign_and_trials',   label: 'All-Rounder',        icon: '🌐', desc: 'Complete all 3 campaign missions and all 4 time trials',    reward: 10000, check: p => p.campaign1_best_lives !== null && p.campaign2_best_lives !== null && p.campaign3_best_lives !== null && p.trial1_best !== null && p.trial2_best !== null && p.trial3_best !== null && p.trial4_best !== null, progress: p => ({ current: [p.campaign1_best_lives, p.campaign2_best_lives, p.campaign3_best_lives].filter(v => v !== null).length + [p.trial1_best, p.trial2_best, p.trial3_best, p.trial4_best].filter(v => v !== null).length, target: 7 }) },
  { type: 'campaign_and_kills',    label: 'Warmonger',          icon: '🌋', desc: 'Complete all 3 campaign missions with 500+ total kills',    reward: 15000, check: p => p.campaign1_best_lives !== null && p.campaign2_best_lives !== null && p.campaign3_best_lives !== null && p.total_kills >= 500, progress: p => ({ current: (p.campaign1_best_lives !== null ? 1 : 0) + (p.campaign2_best_lives !== null ? 1 : 0) + (p.campaign3_best_lives !== null ? 1 : 0) + (p.total_kills >= 500 ? 1 : 0), target: 4 }) },
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
const stmtAwardAch      = db.prepare('INSERT OR IGNORE INTO achievements (pilot_id, type) VALUES (?, ?)');
const stmtGetAchs       = db.prepare('SELECT type, earned_at, credited FROM achievements WHERE pilot_id = ? ORDER BY earned_at ASC');
const stmtMarkCredited  = db.prepare('UPDATE achievements SET credited = 1 WHERE pilot_id = ? AND type = ?');
const stmtAddCredits    = db.prepare('UPDATE pilots SET credits = credits + ? WHERE id = ?');
const stmtSubCredits    = db.prepare('UPDATE pilots SET credits = MAX(0, credits - ?) WHERE id = ?');
const stmtGetCredits    = db.prepare('SELECT credits FROM pilots WHERE id = ?');
const stmtLogTx         = db.prepare('INSERT INTO credit_transactions (pilot_id, amount, reason) VALUES (?, ?, ?)');
const stmtGetTxHistory  = db.prepare('SELECT amount, reason, created_at FROM credit_transactions WHERE pilot_id = ? ORDER BY created_at DESC LIMIT ?');
const stmtUncredited    = db.prepare("SELECT pilot_id, type FROM achievements WHERE credited = 0");
const stmtCampaignComplete = db.prepare(`
  UPDATE pilots SET
    campaign_boss_kills        = campaign_boss_kills + 1,
    campaign_total_completions = campaign_total_completions + 1
  WHERE id = ?
`);
const stmtCampaignBestLives = [
  db.prepare('UPDATE pilots SET campaign1_best_lives = ? WHERE id = ? AND (campaign1_best_lives IS NULL OR campaign1_best_lives < ?)'),
  db.prepare('UPDATE pilots SET campaign2_best_lives = ? WHERE id = ? AND (campaign2_best_lives IS NULL OR campaign2_best_lives < ?)'),
  db.prepare('UPDATE pilots SET campaign3_best_lives = ? WHERE id = ? AND (campaign3_best_lives IS NULL OR campaign3_best_lives < ?)'),
];
const stmtLeaderboard = db.prepare(`
  SELECT username, rank, total_kills, total_deaths, matches_won, matches_lost, games_played, high_score
  FROM pilots
  ORDER BY total_kills DESC, matches_won DESC
  LIMIT 50
`);

// ── Internal helpers ──────────────────────────────────────────────────────────

function _addCredits(pilotId, amount, reason) {
  stmtAddCredits.run(amount, pilotId);
  stmtLogTx.run(pilotId, amount, reason);
}

function checkAndAwardAchievements(pilotId, pilot) {
  const existing = new Set(stmtGetAchs.all(pilotId).map(r => r.type));
  const newlyEarned = [];
  for (const def of ACHIEVEMENT_DEFS) {
    if (def.check(pilot) && !existing.has(def.type)) {
      stmtAwardAch.run(pilotId, def.type);
      stmtMarkCredited.run(pilotId, def.type);
      if (def.reward > 0) _addCredits(pilotId, def.reward, `achievement:${def.type}`);
      newlyEarned.push({ type: def.type, label: def.label, icon: def.icon, reward: def.reward });
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
  // Use a valid (but dummy) bcrypt hash so the compare time is consistent
  // This is the hash for "dummy_fallback_password"
  const hash = pilot?.hashed_password ?? '$2a$10$wT0E8K.01.t7VqgLwA9xDuw16x5lH.98889Z2f/gB17vR6g5p3uO2';
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
    credits:          pilot.credits,
    unlockHull:       !!pilot.unlock_hull,
    unlockAccent:     !!pilot.unlock_accent,
    unlockTrail:      !!pilot.unlock_trail,
    unlockTrailShape: !!pilot.unlock_trail_shape,
    unlockAdminShip:  !!pilot.unlock_admin_ship,
    campaign1BestLives:       pilot.campaign1_best_lives ?? null,
    campaign2BestLives:       pilot.campaign2_best_lives ?? null,
    campaign3BestLives:       pilot.campaign3_best_lives ?? null,
    campaignBossKills:        pilot.campaign_boss_kills  ?? 0,
    campaignTotalCompletions: pilot.campaign_total_completions ?? 0,
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

  // Award match credits: kills + bot kills + win bonus
  const matchCr = (kills * CR_PER_KILL) + (botsKilled * CR_PER_BOT_KILL) + (won === true ? CR_WIN_BONUS : 0);
  if (matchCr > 0) {
    const parts = [];
    if (kills > 0)      parts.push(`kills(${kills})`);
    if (botsKilled > 0) parts.push(`bots(${botsKilled})`);
    if (won === true)    parts.push('win_bonus');
    _addCredits(pilotId, matchCr, `match:${parts.join(',')}`);
  }

  const pilot = stmtById.get(pilotId);
  if (!pilot) return { newAchievements: [], creditsEarned: matchCr };
  const newRank = computeRank(pilot.total_kills);
  if (newRank !== pilot.rank) stmtUpdateRank.run(newRank, pilotId);
  const newAchs = checkAndAwardAchievements(pilotId, pilot);
  const achCr   = newAchs.reduce((s, a) => s + (a.reward || 0), 0);
  return { newAchievements: newAchs, creditsEarned: matchCr + achCr };
}

// ── Trial time ────────────────────────────────────────────────────────────────

export function recordTrialTime(pilotId, trialNum, time) {
  const idx = Number(trialNum) - 1;
  if (idx < 0 || idx > 3) return { newAchievements: [], creditsEarned: 0 };
  stmtTrialBests[idx].run(time, pilotId, time);
  const pilot = stmtById.get(pilotId);
  if (!pilot) return { newAchievements: [], creditsEarned: 0 };
  const newAchs = checkAndAwardAchievements(pilotId, pilot);
  const achCr   = newAchs.reduce((s, a) => s + (a.reward || 0), 0);
  return { newAchievements: newAchs, creditsEarned: achCr };
}

// ── Campaign result ───────────────────────────────────────────────────────────

export function recordCampaignResult(pilotId, missionNum, livesRemaining) {
  const idx = Number(missionNum) - 1;
  if (idx < 0 || idx > 2) return { newAchievements: [], creditsEarned: 0 };
  const lives = Math.max(0, Math.min(3, Math.round(Number(livesRemaining) || 0)));
  stmtCampaignComplete.run(pilotId);
  stmtCampaignBestLives[idx].run(lives, pilotId, lives);
  const missionCr = [500, 1000, 2000][idx];
  _addCredits(pilotId, missionCr, `campaign:mission${missionNum}`);
  const pilot = stmtById.get(pilotId);
  if (!pilot) return { newAchievements: [], creditsEarned: missionCr };
  const newAchs = checkAndAwardAchievements(pilotId, pilot);
  const achCr = newAchs.reduce((s, a) => s + (a.reward || 0), 0);
  return { newAchievements: newAchs, creditsEarned: missionCr + achCr };
}

// ── Profile ───────────────────────────────────────────────────────────────────

export function getPilotProfile(username) {
  const pilot = stmtByUsername.get(String(username ?? ''));
  if (!pilot) return null;
  const achRows = stmtGetAchs.all(pilot.id);
  const earnedMap = new Map(achRows.map(r => [r.type, r.earned_at]));
  const achievements = ACHIEVEMENT_DEFS.map(def => {
    const earned = earnedMap.has(def.type);
    return {
      type:     def.type,
      label:    def.label,
      icon:     def.icon,
      desc:     def.desc,
      earned,
      earnedAt: earnedMap.get(def.type) ?? null,
      progress: (!earned && def.progress) ? def.progress(pilot) : null,
    };
  });
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
    credits:          pilot.credits,
    unlockHull:       !!pilot.unlock_hull,
    unlockAccent:     !!pilot.unlock_accent,
    unlockTrail:      !!pilot.unlock_trail,
    unlockTrailShape: !!pilot.unlock_trail_shape,
    unlockAdminShip:  !!pilot.unlock_admin_ship,
    campaign1BestLives:       pilot.campaign1_best_lives ?? null,
    campaign2BestLives:       pilot.campaign2_best_lives ?? null,
    campaign3BestLives:       pilot.campaign3_best_lives ?? null,
    campaignBossKills:        pilot.campaign_boss_kills  ?? 0,
    campaignTotalCompletions: pilot.campaign_total_completions ?? 0,
    createdAt:        pilot.created_at,
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

// ── Customization unlocks ─────────────────────────────────────────────────────

export const UNLOCK_COSTS = { hull: 250, accent: 400, trail: 500, trail_shape: 200, admin_ship: 125000 };

const stmtGetUnlocks = db.prepare(
  'SELECT unlock_hull, unlock_accent, unlock_trail, unlock_trail_shape, unlock_admin_ship FROM pilots WHERE id = ?'
);
const stmtSetUnlock = {
  hull:        db.prepare('UPDATE pilots SET unlock_hull        = 1 WHERE id = ?'),
  accent:      db.prepare('UPDATE pilots SET unlock_accent      = 1 WHERE id = ?'),
  trail:       db.prepare('UPDATE pilots SET unlock_trail       = 1 WHERE id = ?'),
  trail_shape: db.prepare('UPDATE pilots SET unlock_trail_shape = 1 WHERE id = ?'),
  admin_ship:  db.prepare('UPDATE pilots SET unlock_admin_ship  = 1 WHERE id = ?'),
};

export function getCustomizationUnlocks(pilotId) {
  const r = stmtGetUnlocks.get(pilotId);
  return {
    unlockHull:       !!r?.unlock_hull,
    unlockAccent:     !!r?.unlock_accent,
    unlockTrail:      !!r?.unlock_trail,
    unlockTrailShape: !!r?.unlock_trail_shape,
    unlockAdminShip:  !!r?.unlock_admin_ship,
  };
}

export function purchaseUnlock(pilotId, feature) {
  const cost = UNLOCK_COSTS[feature];
  if (!cost) return { ok: false, error: 'Unknown feature' };
  const u = getCustomizationUnlocks(pilotId);
  const already = (feature === 'hull' && u.unlockHull) || (feature === 'accent' && u.unlockAccent)
    || (feature === 'trail' && u.unlockTrail) || (feature === 'trail_shape' && u.unlockTrailShape)
    || (feature === 'admin_ship' && u.unlockAdminShip);
  if (already) return { ok: true, alreadyOwned: true, balance: getCredits(pilotId) };
  const result = spendCredits(pilotId, cost, `unlock:${feature}`);
  if (!result.ok) return { ok: false, error: 'Insufficient credits', balance: result.balance };
  stmtSetUnlock[feature].run(pilotId);
  const updated = stmtById.get(pilotId);
  const newAchs = updated ? checkAndAwardAchievements(pilotId, updated) : [];
  return { ok: true, alreadyOwned: false, balance: result.balance, newAchievements: newAchs };
}

// ── Credits (public API) ──────────────────────────────────────────────────────

export function getCredits(pilotId) {
  return stmtGetCredits.get(pilotId)?.credits ?? 0;
}

export function awardCredits(pilotId, amount, reason = 'admin') {
  const safe = Math.max(1, Math.floor(Number(amount) || 0));
  _addCredits(pilotId, safe, reason);
  return getCredits(pilotId);
}

export function spendCredits(pilotId, amount, reason = 'purchase') {
  const safe = Math.max(1, Math.floor(Number(amount) || 0));
  const current = getCredits(pilotId);
  if (current < safe) return { ok: false, balance: current };
  stmtSubCredits.run(safe, pilotId);
  stmtLogTx.run(pilotId, -safe, reason);
  return { ok: true, balance: getCredits(pilotId) };
}

export function getCreditHistory(pilotId, limit = 50) {
  return stmtGetTxHistory.all(pilotId, limit);
}

// ── Startup backfill ──────────────────────────────────────────────────────────
// Awards any achievements that pilots already qualify for but were registered
// before the achievement system existed. Safe to run on every server start
// because INSERT OR IGNORE silently skips already-earned achievements.

const stmtAllPilots = db.prepare('SELECT * FROM pilots');

function backfillAchievements() {
  const pilots = stmtAllPilots.all();
  let newAchs = 0;
  let rankFixes = 0;
  for (const pilot of pilots) {
    const earned = checkAndAwardAchievements(pilot.id, pilot);
    newAchs += earned.length;
    // Recalculate rank in case thresholds changed.
    const correct = computeRank(pilot.total_kills);
    if (correct !== pilot.rank) {
      stmtUpdateRank.run(correct, pilot.id);
      rankFixes++;
    }
  }
  if (newAchs > 0)   console.log(`[achievements] backfill awarded ${newAchs} achievement(s)`);
  if (rankFixes > 0) console.log(`[ranks] corrected ${rankFixes} pilot rank(s)`);

  // Credit any achievements that pre-date the credits system (credited = 0).
  const uncredited = stmtUncredited.all();
  let crTotal = 0;
  for (const row of uncredited) {
    const def = ACHIEVEMENT_MAP[row.type];
    if (def?.reward > 0) {
      _addCredits(row.pilot_id, def.reward, `achievement:${row.type}`);
      crTotal += def.reward;
    }
    stmtMarkCredited.run(row.pilot_id, row.type);
  }
  if (uncredited.length > 0) console.log(`[credits] credited ${uncredited.length} pre-existing achievement(s) (+${crTotal} CR total)`);
}

backfillAchievements();

// ── Colors ────────────────────────────────────────────────────────────────────

export function savePilotColors(pilotId, shipColor, accentColor) {
  const safeHull   = /^#[0-9a-fA-F]{6}$/.test(shipColor)   ? shipColor   : '#9fb6cc';
  const safeAccent = /^#[0-9a-fA-F]{6}$/.test(accentColor) ? accentColor : '#2a3340';
  stmtSaveColors.run(safeHull, safeAccent, pilotId);
}
