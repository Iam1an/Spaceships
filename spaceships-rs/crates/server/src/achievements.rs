//! `ACHIEVEMENT_DEFS` from `server/db.js`, transcribed.
//!
//! All 78 rows, in source order — the order matters, because
//! `getPilotProfile` maps over this table to build the `achievements` array and
//! the profile screen renders it in the order it arrives.
//!
//! The `label`, `icon`, and `desc` strings were extracted from `server/db.js`
//! mechanically rather than retyped, so the emoji are byte-identical (none of
//! them carry a U+FE0F variation selector, which is why several render as
//! text-presentation glyphs — `⚔`, `☠`, `⚖`, `⏱` — in both the JS and here).
//!
//! # JS truthiness, preserved
//!
//! Two families of check depend on JavaScript coercion rules that a naive Rust
//! transcription would get wrong:
//!
//! - `campaign_mN_flawless` is `p.campaignN_best_lives >= 3`. When the mission
//!   has never been completed that column is SQL `NULL`, which reaches JS as
//!   `null`, and `null >= 3` is **`false`** (null numifies to 0). So an
//!   uncompleted mission is not flawless — which is right, but only by
//!   accident. [`Option::is_some_and`] reproduces it explicitly.
//! - `high_roller` is `!!p.unlock_hull && ...` over INTEGER 0/1 columns. Those
//!   are read as [`bool`] here, which is the same thing.

use crate::jsfmt::JsNum;
use serde::Serialize;

/// The stat columns every achievement check reads.
///
/// A projection of the `pilots` row — `checkAndAwardAchievements` is handed the
/// whole row in JS, but only these columns are ever consulted.
#[derive(Debug, Clone, Default)]
pub struct PilotStats {
    /// `total_kills`.
    pub total_kills: i64,
    /// `total_deaths`.
    pub total_deaths: i64,
    /// `high_score` — best single-match kill count.
    pub high_score: i64,
    /// `matches_won`.
    pub matches_won: i64,
    /// `matches_lost`.
    pub matches_lost: i64,
    /// `games_played`.
    pub games_played: i64,
    /// `bots_killed`.
    pub bots_killed: i64,
    /// `trial1_best`..`trial4_best`, seconds. `None` for never completed.
    pub trial_best: [Option<f64>; 4],
    /// `campaign1_best_lives`..`campaign3_best_lives`. `None` for never
    /// completed.
    pub campaign_best_lives: [Option<i64>; 3],
    /// `campaign_boss_kills`.
    pub campaign_boss_kills: i64,
    /// `campaign_total_completions`.
    pub campaign_total_completions: i64,
    /// `unlock_hull` as a bool.
    pub unlock_hull: bool,
    /// `unlock_accent` as a bool.
    pub unlock_accent: bool,
    /// `unlock_trail` as a bool.
    pub unlock_trail: bool,
    /// `unlock_trail_shape` as a bool.
    pub unlock_trail_shape: bool,
    /// `unlock_admin_ship` as a bool.
    pub unlock_admin_ship: bool,
}

impl PilotStats {
    /// `trial1_best` through `trial4_best`.
    #[must_use]
    pub fn trials(&self) -> [Option<f64>; 4] {
        self.trial_best
    }

    /// How many of the four trials have a recorded time.
    #[must_use]
    pub fn trials_done(&self) -> i64 {
        self.trial_best.iter().filter(|t| t.is_some()).count() as i64
    }

    /// How many of the three campaign missions have been completed.
    #[must_use]
    pub fn campaigns_done(&self) -> i64 {
        self.campaign_best_lives
            .iter()
            .filter(|v| v.is_some())
            .count() as i64
    }

    /// How many campaign missions were completed without dying.
    #[must_use]
    pub fn campaigns_flawless(&self) -> i64 {
        self.campaign_best_lives
            .iter()
            .filter(|v| v.is_some_and(|n| n >= 3))
            .count() as i64
    }

    /// `trial1_best`, spelled the way the generated table reads it.
    #[must_use]
    pub fn trial1_best(&self) -> Option<f64> {
        self.trial_best[0]
    }
    /// `trial2_best`.
    #[must_use]
    pub fn trial2_best(&self) -> Option<f64> {
        self.trial_best[1]
    }
    /// `trial3_best`.
    #[must_use]
    pub fn trial3_best(&self) -> Option<f64> {
        self.trial_best[2]
    }
    /// `trial4_best`.
    #[must_use]
    pub fn trial4_best(&self) -> Option<f64> {
        self.trial_best[3]
    }
    /// `campaign1_best_lives`.
    #[must_use]
    pub fn campaign1_best_lives(&self) -> Option<i64> {
        self.campaign_best_lives[0]
    }
    /// `campaign2_best_lives`.
    #[must_use]
    pub fn campaign2_best_lives(&self) -> Option<i64> {
        self.campaign_best_lives[1]
    }
    /// `campaign3_best_lives`.
    #[must_use]
    pub fn campaign3_best_lives(&self) -> Option<i64> {
        self.campaign_best_lives[2]
    }
}

/// The `progress` object on an unearned achievement.
///
/// `{ current, target }`, plus `isTime: true` on the four "under N seconds"
/// trial records — the profile screen switches to a seconds-remaining bar when
/// it sees that flag. The JS omits the key entirely otherwise, so it is skipped
/// rather than emitted as `false`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Progress {
    /// Progress so far, clamped to `target` on the counting achievements.
    pub current: JsNum,
    /// The threshold.
    pub target: JsNum,
    /// Present and `true` only on the trial-time records.
    #[serde(rename = "isTime", skip_serializing_if = "Option::is_none")]
    pub is_time: Option<bool>,
}

/// One achievement definition.
pub struct Def {
    /// Stable identifier, stored in `achievements.type`.
    pub kind: &'static str,
    /// Display title.
    pub label: &'static str,
    /// Emoji.
    pub icon: &'static str,
    /// One-line description.
    pub desc: &'static str,
    /// Credits paid on unlock.
    pub reward: i64,
    /// Whether the pilot currently qualifies.
    pub check: fn(&PilotStats) -> bool,
    /// Progress toward the threshold, for the unearned state. `None` where the
    /// JS writes `progress: null` — a binary achievement with nothing to show a
    /// bar for.
    pub progress: Option<fn(&PilotStats) -> Option<Progress>>,
}

/// `{ current, target }` with integer values.
fn count(current: i64, target: i64) -> Option<Progress> {
    Some(Progress {
        current: JsNum::Int(current),
        target: JsNum::Int(target),
        is_time: None,
    })
}

/// `{ current, target, isTime: true }` — a seconds value against a seconds
/// threshold.
fn time(current: f64, target: f64) -> Option<Progress> {
    Some(Progress {
        current: JsNum::Real(current),
        // The JS writes an integer literal here (`target: 30`), so it must not
        // acquire a decimal point on the way out.
        target: JsNum::Real(target),
        is_time: Some(true),
    })
}

/// `Number(bool)` — the `filter(Boolean).length` and `(cond ? 1 : 0)` sums the
/// combo achievements use for their progress bars.
fn b(v: bool) -> i64 {
    i64::from(v)
}

/// Looks up a definition by its stored `type`.
///
/// Used by the startup backfill, which reads `type` strings out of the
/// `achievements` table and needs each one's reward.
#[must_use]
pub fn def_for(kind: &str) -> Option<&'static Def> {
    ACHIEVEMENT_DEFS.iter().find(|d| d.kind == kind)
}

/// Every achievement, in `server/db.js` source order.
pub static ACHIEVEMENT_DEFS: [Def; 78] = [
    Def {
        kind: "first_kill",
        label: "First Blood",
        icon: "🔫",
        desc: "Get your first kill",
        reward: 100,
        check: |p| p.total_kills >= 1,
        progress: Some(|p| count(p.total_kills.min(1), 1)),
    },
    Def {
        kind: "kills_10",
        label: "Sharpshooter",
        icon: "🎯",
        desc: "Reach 10 total kills",
        reward: 150,
        check: |p| p.total_kills >= 10,
        progress: Some(|p| count(p.total_kills.min(10), 10)),
    },
    Def {
        kind: "kills_50",
        label: "Ace",
        icon: "⚔",
        desc: "Reach 50 total kills",
        reward: 350,
        check: |p| p.total_kills >= 50,
        progress: Some(|p| count(p.total_kills.min(50), 50)),
    },
    Def {
        kind: "kills_100",
        label: "Veteran",
        icon: "🏆",
        desc: "Reach 100 total kills",
        reward: 600,
        check: |p| p.total_kills >= 100,
        progress: Some(|p| count(p.total_kills.min(100), 100)),
    },
    Def {
        kind: "kills_500",
        label: "Legend",
        icon: "👑",
        desc: "Reach 500 total kills",
        reward: 2500,
        check: |p| p.total_kills >= 500,
        progress: Some(|p| count(p.total_kills.min(500), 500)),
    },
    Def {
        kind: "kills_1000",
        label: "Living Weapon",
        icon: "☠",
        desc: "Reach 1000 total kills",
        reward: 5000,
        check: |p| p.total_kills >= 1000,
        progress: Some(|p| count(p.total_kills.min(1000), 1000)),
    },
    Def {
        kind: "kills_2500",
        label: "Destroyer",
        icon: "🌑",
        desc: "Reach 2500 total kills",
        reward: 10000,
        check: |p| p.total_kills >= 2500,
        progress: Some(|p| count(p.total_kills.min(2500), 2500)),
    },
    Def {
        kind: "kills_5000",
        label: "Apocalypse",
        icon: "🌋",
        desc: "Reach 5000 total kills",
        reward: 25000,
        check: |p| p.total_kills >= 5000,
        progress: Some(|p| count(p.total_kills.min(5000), 5000)),
    },
    Def {
        kind: "kills_10000",
        label: "God of War",
        icon: "🔱",
        desc: "Reach 10000 total kills",
        reward: 50000,
        check: |p| p.total_kills >= 10000,
        progress: Some(|p| count(p.total_kills.min(10000), 10000)),
    },
    Def {
        kind: "highscore_5",
        label: "Hot Streak",
        icon: "🌡",
        desc: "5 kills in a single match",
        reward: 150,
        check: |p| p.high_score >= 5,
        progress: Some(|p| count(p.high_score.min(5), 5)),
    },
    Def {
        kind: "highscore_10",
        label: "Unstoppable",
        icon: "🌩",
        desc: "10 kills in a single match",
        reward: 300,
        check: |p| p.high_score >= 10,
        progress: Some(|p| count(p.high_score.min(10), 10)),
    },
    Def {
        kind: "highscore_20",
        label: "Killing Machine",
        icon: "💣",
        desc: "20 kills in a single match",
        reward: 750,
        check: |p| p.high_score >= 20,
        progress: Some(|p| count(p.high_score.min(20), 20)),
    },
    Def {
        kind: "highscore_30",
        label: "Rampage",
        icon: "🌪",
        desc: "30 kills in a single match",
        reward: 1500,
        check: |p| p.high_score >= 30,
        progress: Some(|p| count(p.high_score.min(30), 30)),
    },
    Def {
        kind: "highscore_50",
        label: "Obliterator",
        icon: "💥",
        desc: "50 kills in a single match",
        reward: 3500,
        check: |p| p.high_score >= 50,
        progress: Some(|p| count(p.high_score.min(50), 50)),
    },
    Def {
        kind: "first_win",
        label: "First Victory",
        icon: "🥇",
        desc: "Win your first match",
        reward: 200,
        check: |p| p.matches_won >= 1,
        progress: Some(|p| count(p.matches_won.min(1), 1)),
    },
    Def {
        kind: "wins_5",
        label: "On a Roll",
        icon: "🔥",
        desc: "Win 5 matches",
        reward: 400,
        check: |p| p.matches_won >= 5,
        progress: Some(|p| count(p.matches_won.min(5), 5)),
    },
    Def {
        kind: "wins_25",
        label: "Dominant Force",
        icon: "💪",
        desc: "Win 25 matches",
        reward: 1500,
        check: |p| p.matches_won >= 25,
        progress: Some(|p| count(p.matches_won.min(25), 25)),
    },
    Def {
        kind: "wins_50",
        label: "Warlord",
        icon: "🎖",
        desc: "Win 50 matches",
        reward: 3000,
        check: |p| p.matches_won >= 50,
        progress: Some(|p| count(p.matches_won.min(50), 50)),
    },
    Def {
        kind: "wins_100",
        label: "Conqueror",
        icon: "🏅",
        desc: "Win 100 matches",
        reward: 5000,
        check: |p| p.matches_won >= 100,
        progress: Some(|p| count(p.matches_won.min(100), 100)),
    },
    Def {
        kind: "wins_250",
        label: "Supreme Commander",
        icon: "🌠",
        desc: "Win 250 matches",
        reward: 15000,
        check: |p| p.matches_won >= 250,
        progress: Some(|p| count(p.matches_won.min(250), 250)),
    },
    Def {
        kind: "wins_500",
        label: "Overlord",
        icon: "🏛",
        desc: "Win 500 matches",
        reward: 30000,
        check: |p| p.matches_won >= 500,
        progress: Some(|p| count(p.matches_won.min(500), 500)),
    },
    Def {
        kind: "matches_10",
        label: "Frequent Flyer",
        icon: "🚀",
        desc: "Play 10 matches",
        reward: 100,
        check: |p| p.games_played >= 10,
        progress: Some(|p| count(p.games_played.min(10), 10)),
    },
    Def {
        kind: "matches_50",
        label: "Battle-Hardened",
        icon: "🛡",
        desc: "Play 50 matches",
        reward: 500,
        check: |p| p.games_played >= 50,
        progress: Some(|p| count(p.games_played.min(50), 50)),
    },
    Def {
        kind: "matches_100",
        label: "Iron Pilot",
        icon: "🔩",
        desc: "Play 100 matches",
        reward: 1500,
        check: |p| p.games_played >= 100,
        progress: Some(|p| count(p.games_played.min(100), 100)),
    },
    Def {
        kind: "matches_250",
        label: "Seasoned Pilot",
        icon: "🗺",
        desc: "Play 250 matches",
        reward: 2500,
        check: |p| p.games_played >= 250,
        progress: Some(|p| count(p.games_played.min(250), 250)),
    },
    Def {
        kind: "matches_500",
        label: "War Machine",
        icon: "🌍",
        desc: "Play 500 matches",
        reward: 6000,
        check: |p| p.games_played >= 500,
        progress: Some(|p| count(p.games_played.min(500), 500)),
    },
    Def {
        kind: "matches_1000",
        label: "Eternal Pilot",
        icon: "🌌",
        desc: "Play 1000 matches",
        reward: 15000,
        check: |p| p.games_played >= 1000,
        progress: Some(|p| count(p.games_played.min(1000), 1000)),
    },
    Def {
        kind: "bot_hunter",
        label: "Bot Hunter",
        icon: "🤖",
        desc: "Destroy 10 bots",
        reward: 100,
        check: |p| p.bots_killed >= 10,
        progress: Some(|p| count(p.bots_killed.min(10), 10)),
    },
    Def {
        kind: "bot_slayer",
        label: "Bot Slayer",
        icon: "💀",
        desc: "Destroy 100 bots",
        reward: 500,
        check: |p| p.bots_killed >= 100,
        progress: Some(|p| count(p.bots_killed.min(100), 100)),
    },
    Def {
        kind: "bot_exterminator",
        label: "Bot Exterminator",
        icon: "🔧",
        desc: "Destroy 500 bots",
        reward: 2000,
        check: |p| p.bots_killed >= 500,
        progress: Some(|p| count(p.bots_killed.min(500), 500)),
    },
    Def {
        kind: "bot_overlord",
        label: "Bot Overlord",
        icon: "🦾",
        desc: "Destroy 1000 bots",
        reward: 4000,
        check: |p| p.bots_killed >= 1000,
        progress: Some(|p| count(p.bots_killed.min(1000), 1000)),
    },
    Def {
        kind: "bot_apocalypse",
        label: "Bot Apocalypse",
        icon: "🤯",
        desc: "Destroy 5000 bots",
        reward: 15000,
        check: |p| p.bots_killed >= 5000,
        progress: Some(|p| count(p.bots_killed.min(5000), 5000)),
    },
    Def {
        kind: "kdr_positive",
        label: "Breaking Even",
        icon: "⚖",
        desc: "Reach a 1.0+ KDR (min 10 deaths)",
        reward: 750,
        check: |p| p.total_deaths >= 10 && p.total_kills >= p.total_deaths,
        progress: None,
    },
    Def {
        kind: "kdr_2",
        label: "Skilled Hunter",
        icon: "🦅",
        desc: "Reach a 2.0+ KDR (min 10 deaths)",
        reward: 2000,
        check: |p| p.total_deaths >= 10 && p.total_kills >= p.total_deaths * 2,
        progress: None,
    },
    Def {
        kind: "kdr_3",
        label: "Deadeye",
        icon: "🐺",
        desc: "Reach a 3.0+ KDR (min 10 deaths)",
        reward: 3500,
        check: |p| p.total_deaths >= 10 && p.total_kills >= p.total_deaths * 3,
        progress: None,
    },
    Def {
        kind: "kdr_5",
        label: "Ghost",
        icon: "👁",
        desc: "Reach a 5.0+ KDR (min 10 deaths)",
        reward: 8000,
        check: |p| p.total_deaths >= 10 && p.total_kills >= p.total_deaths * 5,
        progress: None,
    },
    Def {
        kind: "deaths_10",
        label: "First Casualty",
        icon: "🩹",
        desc: "Die 10 times",
        reward: 25,
        check: |p| p.total_deaths >= 10,
        progress: Some(|p| count(p.total_deaths.min(10), 10)),
    },
    Def {
        kind: "deaths_100",
        label: "Crash Test Pilot",
        icon: "⚰",
        desc: "Die 100 times",
        reward: 100,
        check: |p| p.total_deaths >= 100,
        progress: Some(|p| count(p.total_deaths.min(100), 100)),
    },
    Def {
        kind: "deaths_500",
        label: "Sacrifice",
        icon: "🕯",
        desc: "Die 500 times",
        reward: 250,
        check: |p| p.total_deaths >= 500,
        progress: Some(|p| count(p.total_deaths.min(500), 500)),
    },
    Def {
        kind: "losses_10",
        label: "Learning Curve",
        icon: "📉",
        desc: "Lose 10 matches",
        reward: 50,
        check: |p| p.matches_lost >= 10,
        progress: Some(|p| count(p.matches_lost.min(10), 10)),
    },
    Def {
        kind: "losses_50",
        label: "Punching Bag",
        icon: "😤",
        desc: "Lose 50 matches",
        reward: 150,
        check: |p| p.matches_lost >= 50,
        progress: Some(|p| count(p.matches_lost.min(50), 50)),
    },
    Def {
        kind: "trial1_complete",
        label: "Trial Runner",
        icon: "⏱",
        desc: "Complete Trial 1",
        reward: 300,
        check: |p| p.trial1_best().is_some(),
        progress: None,
    },
    Def {
        kind: "trial2_complete",
        label: "Speed Seeker",
        icon: "🌀",
        desc: "Complete Trial 2",
        reward: 400,
        check: |p| p.trial2_best().is_some(),
        progress: None,
    },
    Def {
        kind: "trial3_complete",
        label: "Precision Pilot",
        icon: "🔮",
        desc: "Complete Trial 3",
        reward: 600,
        check: |p| p.trial3_best().is_some(),
        progress: None,
    },
    Def {
        kind: "trial4_complete",
        label: "Elite Racer",
        icon: "🏁",
        desc: "Complete Trial 4",
        reward: 800,
        check: |p| p.trial4_best().is_some(),
        progress: None,
    },
    Def {
        kind: "all_trials",
        label: "Grand Champion",
        icon: "🌟",
        desc: "Complete all 4 trials",
        reward: 2500,
        check: |p| p.trials_done() == 4,
        progress: Some(|p| count(p.trials_done(), 4)),
    },
    Def {
        kind: "trial1_sub30",
        label: "Hypersonic",
        icon: "💫",
        desc: "Complete Trial 1 in under 30 seconds",
        reward: 1500,
        check: |p| p.trial1_best().is_some_and(|t| t < 30.0),
        progress: Some(|p| p.trial1_best().map(|t| time(t, 30.0)).unwrap_or(None)),
    },
    Def {
        kind: "trial2_sub50",
        label: "Lightning Dash",
        icon: "⚡",
        desc: "Complete Trial 2 in under 50 seconds",
        reward: 2000,
        check: |p| p.trial2_best().is_some_and(|t| t < 50.0),
        progress: Some(|p| p.trial2_best().map(|t| time(t, 50.0)).unwrap_or(None)),
    },
    Def {
        kind: "trial3_sub60",
        label: "Razor Edge",
        icon: "🔪",
        desc: "Complete Trial 3 in under 60 seconds",
        reward: 2500,
        check: |p| p.trial3_best().is_some_and(|t| t < 60.0),
        progress: Some(|p| p.trial3_best().map(|t| time(t, 60.0)).unwrap_or(None)),
    },
    Def {
        kind: "trial4_sub70",
        label: "Beyond Limits",
        icon: "🛸",
        desc: "Complete Trial 4 in under 70 seconds",
        reward: 3000,
        check: |p| p.trial4_best().is_some_and(|t| t < 70.0),
        progress: Some(|p| p.trial4_best().map(|t| time(t, 70.0)).unwrap_or(None)),
    },
    Def {
        kind: "speed_demon",
        label: "Speed Demon",
        icon: "💨",
        desc: "Complete any trial in under 30 seconds",
        reward: 5000,
        check: |p| p.trials().iter().any(|t| t.is_some_and(|v| v < 30.0)),
        progress: None,
    },
    Def {
        kind: "grinder",
        label: "Grinder",
        icon: "⚙",
        desc: "200+ kills with a 2.0+ KDR (min 10 deaths)",
        reward: 5000,
        check: |p| {
            p.total_deaths >= 10 && p.total_kills >= 200 && p.total_kills >= p.total_deaths * 2
        },
        progress: None,
    },
    Def {
        kind: "well_rounded",
        label: "Well Rounded",
        icon: "🌐",
        desc: "Play 50 matches, get 50 kills, complete Trial 1",
        reward: 3000,
        check: |p| p.games_played >= 50 && p.total_kills >= 50 && p.trial1_best().is_some(),
        progress: Some(|p| {
            count(
                b(p.games_played >= 50) + b(p.total_kills >= 50) + b(p.trial1_best().is_some()),
                3,
            )
        }),
    },
    Def {
        kind: "veteran_touch",
        label: "Veteran's Touch",
        icon: "🗡",
        desc: "500+ kills and 100+ match wins",
        reward: 8000,
        check: |p| p.total_kills >= 500 && p.matches_won >= 100,
        progress: Some(|p| count(b(p.total_kills >= 500) + b(p.matches_won >= 100), 2)),
    },
    Def {
        kind: "perfectionist",
        label: "Perfectionist",
        icon: "🎭",
        desc: "Complete all 4 trials with a 2.0+ KDR",
        reward: 7500,
        check: |p| {
            p.trials_done() == 4 && p.total_deaths >= 10 && p.total_kills >= p.total_deaths * 2
        },
        progress: Some(|p| {
            count(
                p.trials_done() + b(p.total_deaths >= 10 && p.total_kills >= p.total_deaths * 2),
                5,
            )
        }),
    },
    Def {
        kind: "jack_of_all",
        label: "Jack of All Trades",
        icon: "🃏",
        desc: "100+ player kills, 100+ bot kills, 10+ wins",
        reward: 4000,
        check: |p| p.total_kills >= 100 && p.bots_killed >= 100 && p.matches_won >= 10,
        progress: Some(|p| {
            count(
                b(p.total_kills >= 100) + b(p.bots_killed >= 100) + b(p.matches_won >= 10),
                3,
            )
        }),
    },
    Def {
        kind: "the_grind",
        label: "The Grind",
        icon: "⛏",
        desc: "1000 matches, 1000 kills, 1000 bots destroyed",
        reward: 20000,
        check: |p| p.games_played >= 1000 && p.total_kills >= 1000 && p.bots_killed >= 1000,
        progress: Some(|p| {
            count(
                b(p.games_played >= 1000) + b(p.total_kills >= 1000) + b(p.bots_killed >= 1000),
                3,
            )
        }),
    },
    Def {
        kind: "high_roller",
        label: "High Roller",
        icon: "💎",
        desc: "Unlock all customization features",
        reward: 1000,
        check: |p| p.unlock_hull && p.unlock_accent && p.unlock_trail && p.unlock_trail_shape,
        progress: Some(|p| {
            count(
                b(p.unlock_hull) + b(p.unlock_accent) + b(p.unlock_trail) + b(p.unlock_trail_shape),
                4,
            )
        }),
    },
    Def {
        kind: "campaign_m1_complete",
        label: "Ironclad",
        icon: "🎖",
        desc: "Complete Mission 1: Operation Ironclad",
        reward: 1500,
        check: |p| p.campaign1_best_lives().is_some(),
        progress: None,
    },
    Def {
        kind: "campaign_m2_complete",
        label: "Stormbreaker",
        icon: "⛈",
        desc: "Complete Mission 2: Operation Stormfront",
        reward: 3000,
        check: |p| p.campaign2_best_lives().is_some(),
        progress: None,
    },
    Def {
        kind: "campaign_m3_complete",
        label: "Final Victor",
        icon: "🔱",
        desc: "Complete Mission 3: Final Siege",
        reward: 6000,
        check: |p| p.campaign3_best_lives().is_some(),
        progress: None,
    },
    Def {
        kind: "campaign_all_complete",
        label: "Grand Commander",
        icon: "👑",
        desc: "Complete all 3 campaign missions",
        reward: 15000,
        check: |p| p.campaigns_done() == 3,
        progress: Some(|p| count(p.campaigns_done(), 3)),
    },
    Def {
        kind: "campaign_m1_flawless",
        label: "Ghost Pilot I",
        icon: "👻",
        desc: "Complete Mission 1 without dying",
        reward: 3000,
        check: |p| p.campaign1_best_lives().is_some_and(|v| v >= 3),
        progress: None,
    },
    Def {
        kind: "campaign_m2_flawless",
        label: "Ghost Pilot II",
        icon: "🕶",
        desc: "Complete Mission 2 without dying",
        reward: 6000,
        check: |p| p.campaign2_best_lives().is_some_and(|v| v >= 3),
        progress: None,
    },
    Def {
        kind: "campaign_m3_flawless",
        label: "Untouchable",
        icon: "⚡",
        desc: "Complete Mission 3 without dying",
        reward: 12000,
        check: |p| p.campaign3_best_lives().is_some_and(|v| v >= 3),
        progress: None,
    },
    Def {
        kind: "campaign_all_flawless",
        label: "Immaculate",
        icon: "💎",
        desc: "Complete all 3 campaign missions without dying",
        reward: 30000,
        check: |p| p.campaigns_flawless() == 3,
        progress: Some(|p| count(p.campaigns_flawless(), 3)),
    },
    Def {
        kind: "campaign_boss_first",
        label: "Capital Punishment",
        icon: "💥",
        desc: "Destroy the Capital Ship for the first time",
        reward: 2000,
        check: |p| p.campaign_boss_kills >= 1,
        progress: Some(|p| count(p.campaign_boss_kills.min(1), 1)),
    },
    Def {
        kind: "campaign_boss_5",
        label: "Fleet Slayer",
        icon: "🚀",
        desc: "Destroy the Capital Ship 5 times",
        reward: 5000,
        check: |p| p.campaign_boss_kills >= 5,
        progress: Some(|p| count(p.campaign_boss_kills.min(5), 5)),
    },
    Def {
        kind: "campaign_boss_10",
        label: "Dreadnought Hunter",
        icon: "🎯",
        desc: "Destroy the Capital Ship 10 times",
        reward: 10000,
        check: |p| p.campaign_boss_kills >= 10,
        progress: Some(|p| count(p.campaign_boss_kills.min(10), 10)),
    },
    Def {
        kind: "campaign_boss_25",
        label: "Capital Executioner",
        icon: "☠",
        desc: "Destroy the Capital Ship 25 times",
        reward: 20000,
        check: |p| p.campaign_boss_kills >= 25,
        progress: Some(|p| count(p.campaign_boss_kills.min(25), 25)),
    },
    Def {
        kind: "campaign_boss_50",
        label: "Fleet Annihilator",
        icon: "🔥",
        desc: "Destroy the Capital Ship 50 times",
        reward: 40000,
        check: |p| p.campaign_boss_kills >= 50,
        progress: Some(|p| count(p.campaign_boss_kills.min(50), 50)),
    },
    Def {
        kind: "campaign_runs_5",
        label: "Seasoned Operative",
        icon: "✈",
        desc: "Complete any campaign mission 5 times total",
        reward: 2500,
        check: |p| p.campaign_total_completions >= 5,
        progress: Some(|p| count(p.campaign_total_completions.min(5), 5)),
    },
    Def {
        kind: "campaign_runs_10",
        label: "Iron Will",
        icon: "🛡",
        desc: "Complete any campaign mission 10 times total",
        reward: 5000,
        check: |p| p.campaign_total_completions >= 10,
        progress: Some(|p| count(p.campaign_total_completions.min(10), 10)),
    },
    Def {
        kind: "campaign_runs_25",
        label: "Veteran Commander",
        icon: "⭐",
        desc: "Complete any campaign mission 25 times total",
        reward: 12000,
        check: |p| p.campaign_total_completions >= 25,
        progress: Some(|p| count(p.campaign_total_completions.min(25), 25)),
    },
    Def {
        kind: "campaign_runs_50",
        label: "Elite Ace",
        icon: "🌟",
        desc: "Complete any campaign mission 50 times total",
        reward: 25000,
        check: |p| p.campaign_total_completions >= 50,
        progress: Some(|p| count(p.campaign_total_completions.min(50), 50)),
    },
    Def {
        kind: "campaign_runs_100",
        label: "Campaign Legend",
        icon: "🏆",
        desc: "Complete any campaign mission 100 times total",
        reward: 60000,
        check: |p| p.campaign_total_completions >= 100,
        progress: Some(|p| count(p.campaign_total_completions.min(100), 100)),
    },
    Def {
        kind: "campaign_and_trials",
        label: "All-Rounder",
        icon: "🌐",
        desc: "Complete all 3 campaign missions and all 4 time trials",
        reward: 10000,
        check: |p| p.campaigns_done() == 3 && p.trials_done() == 4,
        progress: Some(|p| count(p.campaigns_done() + p.trials_done(), 7)),
    },
    Def {
        kind: "campaign_and_kills",
        label: "Warmonger",
        icon: "🌋",
        desc: "Complete all 3 campaign missions with 500+ total kills",
        reward: 15000,
        check: |p| p.campaigns_done() == 3 && p.total_kills >= 500,
        progress: Some(|p| count(p.campaigns_done() + b(p.total_kills >= 500), 4)),
    },
];
