// The pilot profile overlay (stats + achievements) and the global leaderboard,
// plus the achievement popup that greets you when you come back from a match.
import { el, esc, onClick } from './dom.js';
import { getToken } from '../auth.js';

const PILOT_NAME_KEY = 'spaceships:pilotName';
const PENDING_ACHS_KEY = 'spaceships:pendingAchs';

const profileOverlay = el('profile-overlay');
const profilePaneMy = el('profile-pane-my');
const profilePaneLb = el('profile-pane-lb');
let leaderboardLoaded = false;

onClick('nameInput', openProfilePanel);
onClick('btnCloseProfile', () => {
  profileOverlay.classList.add('hidden');
  leaderboardLoaded = false;
});
onClick('profile-tab-my', () => switchProfileTab('my'));
onClick('profile-tab-lb', async () => {
  switchProfileTab('lb');
  if (!leaderboardLoaded) { leaderboardLoaded = true; await loadLeaderboard(); }
});

function switchProfileTab(tab) {
  el('profile-tab-my').classList.toggle('active', tab === 'my');
  el('profile-tab-lb').classList.toggle('active', tab === 'lb');
  profilePaneMy.classList.toggle('hidden', tab !== 'my');
  profilePaneLb.classList.toggle('hidden', tab !== 'lb');
}

function fmtCampaignBest(lives) {
  if (lives === null || lives === undefined) return '—';
  if (lives >= 3) return '✓ Flawless';
  return `✓ (${lives} ${lives === 1 ? 'life' : 'lives'} left)`;
}

function fmtTrialTime(t) {
  if (t === null || t === undefined) return '—';
  const total = Math.max(0, parseFloat(t));
  const m = Math.floor(total / 60);
  const s = (total % 60).toFixed(3).padStart(6, '0');
  return `${m}:${s}`;
}

async function openProfilePanel() {
  profileOverlay.classList.remove('hidden');
  leaderboardLoaded = false;
  switchProfileTab('my');
  const token = getToken();
  if (!token) {
    profilePaneMy.innerHTML = '<div class="profile-no-account">Log in to view your profile and stats.</div>';
    return;
  }
  const username = localStorage.getItem(PILOT_NAME_KEY);
  if (!username) {
    profilePaneMy.innerHTML = '<div class="profile-no-account">No pilot name found.</div>';
    return;
  }
  profilePaneMy.innerHTML = '<div class="profile-loading">Loading…</div>';
  try {
    const res = await fetch(`/spaceships/api/profile/${encodeURIComponent(username)}`);
    const data = await res.json();
    if (!data.ok) {
      profilePaneMy.innerHTML = `<div class="profile-error">${esc(data.error || 'Failed to load profile')}</div>`;
      return;
    }
    renderMyProfile(data.profile);
  } catch {
    profilePaneMy.innerHTML = '<div class="profile-error">Could not reach server.</div>';
  }
}

function achievementBadgeHtml(a) {
  const cls = a.earned ? 'achievement-badge' : 'achievement-badge locked';
  const icon = a.earned ? esc(a.icon) : '🔒';
  let progressHtml = '';
  if (!a.earned && a.progress) {
    const { current, target, isTime } = a.progress;
    if (isTime) {
      const curStr = current !== null ? parseFloat(current).toFixed(1) + 's' : '—';
      progressHtml = `<span class="ach-time-hint">${curStr} / &lt;${target}s</span>`;
    } else {
      const pct = target > 0 ? Math.min(100, Math.round(current / target * 100)) : 0;
      progressHtml = `
          <div class="ach-progress-row">
            <div class="ach-progress-bar"><div class="ach-progress-fill" style="width:${pct}%"></div></div>
            <span class="ach-progress-pct">${current}/${target}</span>
          </div>`;
    }
  }
  return `<div class="${cls}" title="${esc(a.desc)}">
      <span class="ach-icon">${icon}</span>
      <div class="ach-badge-body">
        <span class="ach-label">${esc(a.label)}</span>
        ${progressHtml}
      </div>
    </div>`;
}

function renderMyProfile(p) {
  const winRate = p.gamesPlayed > 0 ? Math.round(p.matchesWon / p.gamesPlayed * 100) : 0;
  const earned = p.achievements.filter(a => a.earned);
  const locked = p.achievements.filter(a => !a.earned);
  const earnedHtml = earned.length > 0
    ? earned.map(achievementBadgeHtml).join('')
    : '<div class="profile-no-ach">No achievements yet — get out there!</div>';
  profilePaneMy.innerHTML = `
    <div class="profile-header">
      <div class="profile-callsign">${esc(p.username)}</div>
      <div class="profile-rank">${esc(p.rank)}</div>
    </div>
    <div class="profile-stats-grid">
      <div class="stat-card"><div class="stat-label">KDR</div><div class="stat-value">${esc(p.kdr)}</div></div>
      <div class="stat-card"><div class="stat-label">KILLS</div><div class="stat-value">${p.totalKills}</div></div>
      <div class="stat-card"><div class="stat-label">DEATHS</div><div class="stat-value">${p.totalDeaths}</div></div>
      <div class="stat-card"><div class="stat-label">BOTS KILLED</div><div class="stat-value">${p.botsKilled}</div></div>
      <div class="stat-card"><div class="stat-label">MATCHES</div><div class="stat-value">${p.gamesPlayed}</div></div>
      <div class="stat-card"><div class="stat-label">WINS</div><div class="stat-value">${p.matchesWon}</div></div>
      <div class="stat-card"><div class="stat-label">LOSSES</div><div class="stat-value">${p.matchesLost}</div></div>
      <div class="stat-card"><div class="stat-label">WIN RATE</div><div class="stat-value">${winRate}%</div></div>
      <div class="stat-card"><div class="stat-label">BEST MATCH</div><div class="stat-value">${p.highScore}</div></div>
    </div>
    <div class="profile-section-title">TIME TRIALS — PERSONAL BEST</div>
    <div class="profile-trials">
      <div class="trial-row"><span>Trial 1</span><span>${fmtTrialTime(p.trial1Best)}</span></div>
      <div class="trial-row"><span>Trial 2</span><span>${fmtTrialTime(p.trial2Best)}</span></div>
      <div class="trial-row"><span>Trial 3</span><span>${fmtTrialTime(p.trial3Best)}</span></div>
      <div class="trial-row"><span>Trial 4</span><span>${fmtTrialTime(p.trial4Best)}</span></div>
    </div>
    <div class="profile-section-title">CAMPAIGN</div>
    <div class="profile-trials">
      <div class="trial-row"><span>Mission 1: Operation Ironclad</span><span>${fmtCampaignBest(p.campaign1BestLives)}</span></div>
      <div class="trial-row"><span>Mission 2: Operation Stormfront</span><span>${fmtCampaignBest(p.campaign2BestLives)}</span></div>
      <div class="trial-row"><span>Mission 3: Final Siege</span><span>${fmtCampaignBest(p.campaign3BestLives)}</span></div>
      <div class="trial-row"><span>Capital Ship Kills</span><span>${p.campaignBossKills ?? 0}</span></div>
      <div class="trial-row"><span>Total Completions</span><span>${p.campaignTotalCompletions ?? 0}</span></div>
    </div>
    <button class="ach-toggle-btn" id="achToggleBtn">
      ACHIEVEMENTS — ${earned.length} / ${p.achievements.length}
      <span class="ach-toggle-arrow">▾</span>
    </button>
    <div class="ach-collapsible" id="achContent">
      <div class="ach-section-label">UNLOCKED</div>
      ${earnedHtml}
      ${locked.length > 0 ? `<div class="ach-section-label locked-label">LOCKED</div>${locked.map(achievementBadgeHtml).join('')}` : ''}
    </div>
  `;
  const toggleBtn = profilePaneMy.querySelector('#achToggleBtn');
  const achContent = profilePaneMy.querySelector('#achContent');
  if (toggleBtn && achContent) {
    toggleBtn.addEventListener('click', () => {
      const open = achContent.classList.toggle('ach-open');
      toggleBtn.querySelector('.ach-toggle-arrow').textContent = open ? '▴' : '▾';
    });
  }
}

async function loadLeaderboard() {
  profilePaneLb.innerHTML = '<div class="profile-loading">Loading…</div>';
  try {
    const res = await fetch('/spaceships/api/leaderboard');
    const data = await res.json();
    if (!data.ok) {
      profilePaneLb.innerHTML = '<div class="profile-error">Failed to load leaderboard.</div>';
      return;
    }
    renderLeaderboard(data.leaderboard);
  } catch {
    profilePaneLb.innerHTML = '<div class="profile-error">Could not reach server.</div>';
  }
}

function renderLeaderboard(entries) {
  if (!entries || entries.length === 0) {
    profilePaneLb.innerHTML = '<div class="profile-no-account">No pilots on record yet.</div>';
    return;
  }
  const myName = localStorage.getItem(PILOT_NAME_KEY) || '';
  const rows = entries.map((e, i) => `
    <tr class="${e.username === myName ? 'lb-you' : ''}">
      <td class="lb-rank">#${i + 1}</td>
      <td class="lb-name">${esc(e.username)}</td>
      <td class="lb-val">${esc(e.pilotRank)}</td>
      <td class="lb-val">${e.totalKills}</td>
      <td class="lb-val">${esc(e.kdr)}</td>
      <td class="lb-val">${e.matchesWon}</td>
      <td class="lb-val">${e.gamesPlayed}</td>
    </tr>`).join('');
  profilePaneLb.innerHTML = `
    <table class="lb-table">
      <thead>
        <tr>
          <th>#</th><th>PILOT</th><th>RANK</th><th>KILLS</th><th>KDR</th><th>WINS</th><th>PLAYED</th>
        </tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>`;
}

// Achievements earned mid-match are stashed in localStorage and shown here once
// the pilot is back in the hangar.
export function checkPendingAchievements() {
  try {
    const raw = localStorage.getItem(PENDING_ACHS_KEY);
    if (!raw) return;
    const earned = JSON.parse(raw);
    if (!Array.isArray(earned) || !earned.length) return;
    localStorage.removeItem(PENDING_ACHS_KEY);
    const list = el('hangar-ach-list');
    const overlay = el('hangar-ach-overlay');
    if (!list || !overlay) return;
    list.innerHTML = earned.map(a =>
      `<div class="hangar-ach-row">
        <span class="ach-toast-icon">${esc(a.icon)}</span>
        <div class="ach-toast-body">
          <span class="ach-toast-title">ACHIEVEMENT UNLOCKED</span>
          <span class="ach-toast-label">${esc(a.label)}</span>
          <span class="ach-desc">${esc(a.desc)}</span>
        </div>
      </div>`
    ).join('');
    overlay.classList.remove('hidden');
    el('btnDismissHangarAch').onclick = () => {
      overlay.classList.add('hidden');
    };
  } catch { }
}
