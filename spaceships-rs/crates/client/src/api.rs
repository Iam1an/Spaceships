//! The REST half of the server: accounts, credits, unlocks, records, results.
//!
//! `net.rs` owns the socket and says nothing about HTTP. Everything the JS
//! client reaches with `fetch` — `/api/login`, `/api/profile/:username`,
//! `/api/leaderboard`, `/api/credits`, `/api/unlocks`, and the three
//! `*-result` reports — arrives here instead, and comes out the other side as
//! **one resource, [`LobbyData`], plus [`Account`]**. `ui.rs` reads those two
//! and never learns that a request exists.
//!
//! # One interface, two backends
//!
//! The same split `net.rs` makes, for the same reason: a browser cannot open a
//! socket and a native binary has no `fetch`.
//!
//! ```text
//!         Fetch::start(Request) -> Fetch      Fetch::poll() -> Option<Result<..>>
//!                    │                                    ▲
//!  ┌─────────────────┴────────────────────────────────────┴──────────────────┐
//!  │  native                                   wasm32                        │
//!  │  ──────                                   ──────                        │
//!  │  one short-lived OS thread                `window.fetch`, driven by     │
//!  │  + std::net::TcpStream                    `spawn_local`                 │
//!  │  + hand-rolled HTTP/1.1                                                 │
//!  │                                                                         │
//!  │  answer crosses on a std mpsc             answer crosses in an          │
//!  │                                           Rc<RefCell<Option<..>>>       │
//!  └─────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Why a hand-rolled client and not `reqwest`/`ureq`
//!
//! Because the hard part of an HTTP client is TLS, and **there is no TLS to
//! do**. `deploy/README.md`'s Caddy site block is port 80 with no 443 listener,
//! so every endpoint this client will ever call is plain `http://` — the same
//! fact that lets `net.rs` compile without `rustls-tls-webpki-roots`. What is
//! left is: open a TCP connection, write a request line and four headers, read
//! until the peer closes, and split on the blank line. That is the whole native
//! backend, it needs no new dependency at all, and it cannot drag a second
//! async runtime or fifteen crates of certificate handling into a payload that
//! is already 15 MB of wasm.
//!
//! `reqwest` would not have helped on the web half either: its wasm backend is
//! `web_sys::fetch` behind a wrapper, which is what the wasm side here already
//! is, minus the wrapper.
//!
//! The cost, stated plainly: no `https://`, no keep-alive, no redirects, no
//! content negotiation, and one thread per request. If this client ever has to
//! speak to a TLS endpoint, that is the point at which a real client crate
//! earns its place — and [`Request::send`] says so where it refuses the scheme.
//!
//! ## Nothing here ever blocks a frame
//!
//! Same discipline as `net.rs`. [`Fetch::poll`] moves whatever has arrived and
//! returns; a request that is still in flight costs one `try_recv` a frame.
//! [`ApiLink`] is a **non-send** resource for exactly the reason `NetLink` is:
//! the wasm backend is `Rc`/`RefCell` all the way down and can never be `Send`.
//!
//! # Where the token lives
//!
//! - **Web**: `localStorage['spaceships:token']`, the key `public/src/auth.js`
//!   already uses, so a pilot signed in on the JS client is signed in on the
//!   wasm one and vice versa.
//! - **Native**: a file under the user's own config directory, written `0600`
//!   (see [`store`]). It is a bearer credential valid for seven days, so it does
//!   not go anywhere world-readable, into the repository, or into an
//!   environment variable — `SPACESHIPS_TOKEN` is still *read*, for scripted
//!   runs, but nothing is ever written back to it.
//!
//! # Failure is normal
//!
//! No network, a server that is down, a token the server rejects, and a token
//! that simply expired are all ordinary. None of them may hang the lobby or
//! lose a match:
//!
//! - Every request has a connect and a read timeout, and runs off the frame.
//! - A `401` on any authenticated call **signs the pilot out** — token cleared,
//!   `LobbyData` back to a guest record — because that is what an expired
//!   seven-day token looks like and the alternative is a lobby that silently
//!   shows nothing.
//! - Anything else leaves the data that is already on screen alone and marks it
//!   [`DataSource::Stale`], so the `FEED` annunciator says so.
//! - Guest is always reachable and always works. A guest issues exactly one
//!   request in its life (the leaderboard, which is public) and can decline
//!   even that by never opening the page.

use bevy::prelude::*;
use serde_json::Value;

use crate::net::{api_base_for, ConnState, NetCommand, NetConfig, NetStatus};
use crate::sim_bridge::{MatchSetup, SimFrame, LOCAL_ID};
use spaceships_sim as sim;

use sim::world::{Mode, SimEvent, Team};

// ─────────────────────────────────────────────────────────────────────────────
// Tuning
// ─────────────────────────────────────────────────────────────────────────────

/// Shortest gap between two unforced [`ApiRequest::Refresh`]es.
///
/// Walking between the record, the standings and the armory is three page
/// changes and would otherwise be three round trips per second. A forced
/// refresh — one that follows a sign-in or a purchase — ignores this.
const REFRESH_COOLDOWN: f32 = 8.0;

/// How long a native request waits for the connection, and then for the answer.
///
/// Generous, because it is paid on a thread and nothing waits for it. Bounded,
/// because a thread that never returns is a leak.
#[cfg(not(target_arch = "wasm32"))]
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(6);
#[cfg(not(target_arch = "wasm32"))]
const IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);

/// The most a response body may be, in bytes.
///
/// `/api/leaderboard` is fifty rows and `/api/profile` carries every
/// achievement definition, so the real ceiling is tens of kilobytes. This is
/// three orders of magnitude above that and exists only so that a server
/// answering with something enormous cannot exhaust memory.
#[cfg(not(target_arch = "wasm32"))]
const MAX_BODY: usize = 4 << 20;

/// `/api/register`'s rules, from `server/db.js`: 3–20 characters, letters and
/// digits only.
pub const CALLSIGN_MAX: usize = 20;
pub const CALLSIGN_MIN: usize = 3;
/// `/api/register`'s other rule.
pub const PASSWORD_MIN: usize = 6;
/// Longer than any password worth typing on a game pad, and short enough that
/// the field cannot be used to build a request that is not one.
pub const PASSWORD_MAX: usize = 64;

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

/// Registers the REST client: two resources the lobby reads, one message it
/// writes, and the three systems that move the bytes.
pub struct ApiPlugin;

impl Plugin for ApiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Account>()
            .insert_resource(LobbyData::placeholder())
            .init_resource::<Tally>()
            .add_message::<ApiRequest>()
            // Non-send: `Fetch` on wasm is `Rc`-based. Same reasoning as
            // `net::NetLink`, and the same consequence — the pump runs on the
            // main thread and costs microseconds.
            .init_non_send::<ApiLink>()
            .add_systems(Startup, restore_session)
            // `PreUpdate`, before anything in `Update` reads `LobbyData`, so a
            // page drawn this frame shows what landed this frame. Chained: a
            // request submitted now should not be polled until the next frame,
            // which `submit` after `pump` guarantees.
            .add_systems(PreUpdate, (pump, submit).chain())
            // After the fixed step, so the events it reads are this tick's.
            .add_systems(FixedUpdate, report_results.after(crate::sim_bridge::SimSet));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// What the rest of the client sees
// ─────────────────────────────────────────────────────────────────────────────

/// Whether [`LobbyData`] came from the server, and how much of it did.
///
/// The `FEED` annunciator prints this, so it is the one thing in the lobby that
/// must never flatter itself: a screenshot has to say whether the numbers on it
/// are real.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataSource {
    /// [`LobbyData::placeholder`] — nothing has been fetched. `CACHED`.
    Placeholder,
    /// Public endpoints only: the leaderboard, with a guest's own empty record.
    /// `PUBLIC`.
    Public,
    /// An authenticated pilot's own data. `LIVE`.
    Live,
    /// Real data that a later refresh failed to renew. `STALE`.
    Stale,
}

impl DataSource {
    /// What the `FEED` lamp says.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            DataSource::Placeholder => "CACHED",
            DataSource::Public => "PUBLIC",
            DataSource::Live => "LIVE",
            DataSource::Stale => "STALE",
        }
    }

    /// Whether this is a reason to raise the caution lamp: the pages are
    /// showing something other than the server's current answer.
    #[must_use]
    pub fn is_suspect(self) -> bool {
        matches!(self, DataSource::Placeholder | DataSource::Stale)
    }
}

/// One pilot's dossier. `GET /api/profile/:username`, plus `GET /api/credits`
/// for the balance and `GET /api/unlocks` for [`PilotRecord::owned`].
#[derive(Debug, Clone)]
pub struct PilotRecord {
    /// The callsign that goes out with `name` and that names this pilot in
    /// every other client's target boxes.
    pub callsign: String,
    /// `pilots.rank`, which `server/db.js`'s `computeRank` derives from the
    /// kill count. Not recomputed here — a second ladder is a second thing to
    /// drift.
    pub rank: String,
    /// The server's own `kdr` string, formatted its way rather than ours.
    ///
    /// This replaced an invented "service number". The record page needs three
    /// things on its header line and the server has exactly three to give.
    pub kdr: String,
    /// `createdAt` as `YYYY-MM-DD`, or `-` when there is no account.
    pub enlisted: String,
    pub credits: u32,
    pub kills: u32,
    pub deaths: u32,
    pub matches_won: u32,
    pub matches_lost: u32,
    pub bots_killed: u32,
    /// Derived from `gamesPlayed` at the nominal five-minute match.
    pub flight_minutes: u32,
    /// `trial1_best`–`trial4_best`, seconds. `None` is "no time set".
    pub trial_best: [Option<f32>; 4],
    /// `campaign{1,2,3}_best_lives`. `None` is "not beaten".
    pub campaign_lives: [Option<u8>; 3],
    pub campaign_boss_kills: u32,
    /// Which armory rungs are held, as a bitset over `ui::ARMORY`. See
    /// [`UNLOCKS`] for which rungs the server actually has a column for.
    pub owned: u32,
}

impl PilotRecord {
    /// The record a pilot with no account has: their chosen callsign and
    /// nothing else. Every number is zero because every number *is* zero — a
    /// guest's kills are not recorded anywhere.
    #[must_use]
    pub fn guest(callsign: String) -> PilotRecord {
        PilotRecord {
            callsign,
            rank: "GUEST".to_owned(),
            kdr: "0.00".to_owned(),
            enlisted: "-".to_owned(),
            credits: 0,
            kills: 0,
            deaths: 0,
            matches_won: 0,
            matches_lost: 0,
            bots_killed: 0,
            flight_minutes: 0,
            trial_best: [None; 4],
            campaign_lives: [None; 3],
            campaign_boss_kills: 0,
            // The issued airframe, and only that.
            owned: 1,
        }
    }
}

/// One row of the squadron standings. `GET /api/leaderboard`.
#[derive(Debug, Clone)]
pub struct Standing {
    pub callsign: String,
    pub rank: String,
    pub kills: u32,
    pub wins: u32,
    /// Drawn amber: this is the local pilot.
    pub you: bool,
}

/// Everything the lobby's pages read that this client does not simulate.
///
/// **This is the seam.** No page touches an HTTP client; they read this
/// resource. It lives here rather than in `ui.rs` so the dependency runs one
/// way — the module that *fills* it owns it, and the module that draws it only
/// reads.
#[derive(Resource, Debug, Clone)]
pub struct LobbyData {
    pub source: DataSource,
    pub pilot: PilotRecord,
    pub standings: Vec<Standing>,
    /// Bumped on every write. `ui.rs`'s `MenuModel` watches this rather than
    /// the fields, for the same reason it watches `NetSession::rev`: the model
    /// is `Copy` and a `Vec<Standing>` cannot go in it.
    pub rev: u32,
}

impl LobbyData {
    fn bump(&mut self) {
        self.rev = self.rev.wrapping_add(1);
    }

    /// Plausible numbers, so the pages can be looked at and judged before
    /// anything has been fetched. The `FEED` lamp blinks `CACHED` the whole
    /// time this is what is on screen.
    ///
    /// The balance is deliberately mid-ladder: tiers 0 to 2 are held, tier 3 is
    /// open with one rung out of reach, and everything above it is locked but
    /// visible. That is all four rung states on one screenshot.
    #[must_use]
    pub fn placeholder() -> LobbyData {
        LobbyData {
            source: DataSource::Placeholder,
            pilot: PilotRecord {
                // The one placeholder field that is not decoration: it is the
                // name that goes out with `name` and that every other pilot in
                // the room reads off a target box.
                callsign: MatchSetup::from_env().callsign,
                rank: "FLIGHT LIEUTENANT".to_owned(),
                kdr: "1.60".to_owned(),
                enlisted: "2026-03-11".to_owned(),
                credits: 5_200,
                kills: 418,
                deaths: 261,
                matches_won: 63,
                matches_lost: 41,
                bots_killed: 1_206,
                flight_minutes: 1_247,
                trial_best: [Some(92.4), Some(118.7), None, None],
                campaign_lives: [Some(3), Some(1), None],
                campaign_boss_kills: 2,
                owned: 0b0000_0000_1111_1111,
            },
            standings: [
                ("VANDAL", "GRAND ADMIRAL", 4_120, 611, false),
                ("HALCYON", "FLEET ADMIRAL", 3_884, 552, false),
                ("NOMAD", "ADMIRAL", 2_940, 480, false),
                ("SABLE", "COMMODORE", 2_311, 402, false),
                ("IRONSIDE", "CAPTAIN", 1_884, 340, false),
                ("MERIDIAN", "COMMANDER", 1_402, 288, false),
                ("PILOT", "FLIGHT LIEUTENANT", 418, 63, true),
                ("TALLY-HO", "FLIGHT LIEUTENANT", 402, 60, false),
            ]
            .into_iter()
            .map(|(callsign, rank, kills, wins, you)| Standing {
                callsign: callsign.to_owned(),
                rank: rank.to_owned(),
                kills,
                wins,
                you,
            })
            .collect(),
            rev: 0,
        }
    }
}

/// Who is signed in, and what the last request had to say about it.
///
/// Separate from [`LobbyData`] because they change for different reasons:
/// signing in is a decision, and the dossier behind it is a consequence.
#[derive(Resource, Debug, Clone, Default)]
pub struct Account {
    /// The JWT, or `None` for a guest. This is also what `net.rs` puts on the
    /// socket URL — [`push_token_to_socket`] keeps the two equal.
    pub token: Option<String>,
    /// The server's spelling of the username the token belongs to.
    pub username: Option<String>,
    /// Requests in flight. Non-zero is what the auth page shows as `WORKING`.
    pub busy: u32,
    /// One-shot message for the lobby footer, taken by `ui.rs`.
    pub notice: Option<String>,
    /// Bumped on every change; the lobby's model watches this.
    pub rev: u32,
}

impl Account {
    /// Whether there is a token to send.
    #[must_use]
    pub fn signed_in(&self) -> bool {
        self.token.is_some()
    }

    fn bump(&mut self) {
        self.rev = self.rev.wrapping_add(1);
    }

    fn say(&mut self, msg: impl Into<String>) {
        self.notice = Some(msg.into());
        self.bump();
    }

    /// Takes the pending notice, if there is one. Taking rather than reading:
    /// it is a one-shot for the footer, and one that stayed set would
    /// re-announce itself on every repaint.
    pub fn take_notice(&mut self) -> Option<String> {
        let notice = self.notice.take();
        if notice.is_some() {
            self.bump();
        }
        notice
    }
}

/// What the lobby can ask the server for. Write it with
/// `MessageWriter<ApiRequest>`.
///
/// Deliberately coarse: the lobby says *what it wants*, never which endpoints
/// that is. [`Refresh`](ApiRequest::Refresh) is four requests when signed in
/// and one when not, and no page has to know that.
// No `Eq`: a trial time is an `f64`, because that is what the sim measures a
// lap in and rounding it on the way to the server would change a personal best.
#[derive(Message, Debug, Clone, PartialEq)]
pub enum ApiRequest {
    /// `POST /api/login`.
    SignIn { username: String, password: String },
    /// `POST /api/register`, then sign in with the same credentials.
    Enlist { username: String, password: String },
    /// Forget the token and go back to being a guest. Never fails.
    SignOut,
    /// Re-read whatever this pilot's pages show. Rate-limited by
    /// [`REFRESH_COOLDOWN`] unless `force`.
    Refresh { force: bool },
    /// `POST /api/unlock/:feature`.
    Unlock(&'static str),
    /// `POST /api/solo-result`.
    SoloResult {
        kills: u32,
        deaths: u32,
        bots: u32,
        won: Option<bool>,
    },
    /// `POST /api/trial-result`.
    TrialResult { number: u8, seconds: f64 },
    /// `POST /api/campaign-result`.
    CampaignResult { mission: u8, lives: u8 },
}

/// The five armory rungs the server actually has a column for, as
/// `(ui::ARMORY index, /api/unlock/:feature)`.
///
/// The ladder in `ui.rs` is sixteen rungs and the database has five booleans —
/// `unlock_hull`, `unlock_accent`, `unlock_trail`, `unlock_trail_shape`,
/// `unlock_admin_ship`. This table is the whole of the overlap, and it is the
/// reason a live account's ladder stops where it does: the rest of `ARMORY` is
/// `BACKLOG.md`'s planned economy, and buying something the server has never
/// heard of would be spending real credits on a bit that is only in this
/// process. `ui.rs` refuses those rungs rather than pretending.
///
/// The costs are pinned against `UNLOCK_COSTS` in `server/db.js` by a test.
pub const UNLOCKS: [(usize, &str, u32); 5] = [
    (2, "trail_shape", 200),
    (3, "hull", 250),
    (4, "accent", 400),
    (5, "trail", 500),
    (15, "admin_ship", 125_000),
];

/// The `/api/unlock/:feature` name for an armory index, if it has one.
#[must_use]
pub fn unlock_key(rung: usize) -> Option<&'static str> {
    UNLOCKS
        .iter()
        .find(|(i, _, _)| *i == rung)
        .map(|(_, key, _)| *key)
}

/// The armory bitset the server's five booleans describe.
///
/// Bit 0 — the issued airframe — is always set: every pilot has it, and it is
/// what opens tier 1.
fn owned_bits(flags: &Value) -> u32 {
    let mut owned = 1u32;
    for (rung, key, _) in UNLOCKS {
        let field = match key {
            "hull" => "unlockHull",
            "accent" => "unlockAccent",
            "trail" => "unlockTrail",
            "trail_shape" => "unlockTrailShape",
            _ => "unlockAdminShip",
        };
        if truthy(flags.get(field)) {
            owned |= 1 << rung;
        }
    }
    owned
}

// ─────────────────────────────────────────────────────────────────────────────
// The in-flight table
// ─────────────────────────────────────────────────────────────────────────────

/// What a reply is an answer to. One variant per thing [`pump`] has to do with
/// a body.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Job {
    /// A sign-in whose reply carries the token.
    SignIn,
    /// A registration; the credentials are kept so the sign-in can follow it.
    Enlist {
        username: String,
        password: String,
    },
    Profile,
    Credits,
    Unlocks,
    Leaderboard,
    /// A purchase, so the ladder can be re-read once it lands.
    Unlock,
    /// A result report. Nothing on screen depends on it; it is here so a
    /// failure can be logged rather than swallowed.
    Report(&'static str),
}

impl Job {
    /// Whether a `401` on this job means the token is dead.
    ///
    /// `/api/profile` and `/api/leaderboard` need no token, so a 401 from them
    /// is a server bug, not an expiry.
    fn is_authenticated(&self) -> bool {
        !matches!(self, Job::Profile | Job::Leaderboard)
    }
}

/// Owns every request in flight. Private and non-send — see the module docs.
#[derive(Default)]
struct ApiLink {
    /// Tagged with the job so a reply knows what it answers.
    inflight: Vec<(Job, Fetch)>,
    /// `Time::elapsed_secs` of the last refresh, for [`REFRESH_COOLDOWN`].
    refreshed_at: Option<f32>,
    /// Requests waiting for something. Only ever the sign-in that follows a
    /// registration.
    queued: Vec<ApiRequest>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Systems
// ─────────────────────────────────────────────────────────────────────────────

/// Loads whatever credential is on this machine, before the first frame, and
/// asks the server to confirm it.
///
/// The refresh is the whole of "verify the token": `/api/credits` needs one, so
/// a token the server will not accept comes back `401` and [`apply`] signs the
/// pilot out — which is what an expired seven-day credential looks like. It is
/// also what fills the balance and the rank before the pilot has navigated
/// anywhere, so the header is right on the first page rather than after a tour
/// of the display.
///
/// **A guest issues nothing here**, because there is nothing to confirm: the
/// early return above is what keeps a session with no account genuinely silent.
/// And nothing waits — a client started with no network gets a failed refresh,
/// a `STALE`/`CACHED` lamp and a working lobby.
fn restore_session(
    mut account: ResMut<Account>,
    mut data: ResMut<LobbyData>,
    mut config: ResMut<NetConfig>,
    mut out: MessageWriter<ApiRequest>,
) {
    let Some((token, username)) = store::load() else {
        return;
    };
    out.write(ApiRequest::Refresh { force: true });
    account.token = Some(token);
    account.username = username.clone();
    account.bump();
    if let Some(name) = username {
        // Before any fetch, so the lobby and the socket already announce the
        // right pilot. The numbers beside it are still the placeholder's, and
        // the `FEED` lamp still says `CACHED` until a reply lands.
        data.pilot.callsign.clone_from(&name);
        data.bump();
    }
    // `SPACESHIPS_TOKEN` is the developer's override and wins: `net.rs` put it
    // there, and a stored credential must not silently replace it.
    if config.token.is_none() {
        config.token.clone_from(&account.token);
    }
}

/// Turns [`ApiRequest`]s into HTTP.
fn submit(
    time: Res<Time>,
    mut requests: MessageReader<ApiRequest>,
    mut link: NonSendMut<ApiLink>,
    mut account: ResMut<Account>,
    mut data: ResMut<LobbyData>,
    mut config: ResMut<NetConfig>,
    status: Res<NetStatus>,
    mut commands: MessageWriter<NetCommand>,
) {
    let base = api_base_for(&config.url);
    let queued: Vec<ApiRequest> = link.queued.drain(..).collect();
    for request in queued.iter().chain(requests.read()) {
        match request {
            ApiRequest::SignIn { username, password } => {
                start(
                    &mut link,
                    &mut account,
                    Job::SignIn,
                    Request::post(
                        &format!("{base}/api/login"),
                        None,
                        credentials(username, password),
                    ),
                );
            }
            ApiRequest::Enlist { username, password } => {
                start(
                    &mut link,
                    &mut account,
                    Job::Enlist {
                        username: username.clone(),
                        password: password.clone(),
                    },
                    Request::post(
                        &format!("{base}/api/register"),
                        None,
                        credentials(username, password),
                    ),
                );
            }
            ApiRequest::SignOut => {
                sign_out(&mut account, &mut data, "SIGNED OUT");
                push_token_to_socket(&account, &mut config, &status, &mut commands);
            }
            ApiRequest::Refresh { force } => {
                let now = time.elapsed_secs();
                let due = *force
                    || link
                        .refreshed_at
                        .is_none_or(|then| now - then >= REFRESH_COOLDOWN);
                if !due {
                    continue;
                }
                link.refreshed_at = Some(now);
                refresh(&mut link, &mut account, &base);
            }
            ApiRequest::Unlock(feature) => {
                let Some(token) = account.token.clone() else {
                    account.say("SIGN IN TO REQUISITION");
                    continue;
                };
                start(
                    &mut link,
                    &mut account,
                    Job::Unlock,
                    Request::post(
                        &format!("{base}/api/unlock/{feature}"),
                        Some(token),
                        String::new(),
                    ),
                );
            }
            ApiRequest::SoloResult {
                kills,
                deaths,
                bots,
                won,
            } => {
                let body = format!(
                    r#"{{"kills":{kills},"deaths":{deaths},"botsKilled":{bots},"won":{}}}"#,
                    match won {
                        Some(true) => "true",
                        Some(false) => "false",
                        None => "null",
                    }
                );
                report(&mut link, &mut account, &base, "solo-result", body);
            }
            ApiRequest::TrialResult { number, seconds } => {
                let body = format!(r#"{{"trialNum":{number},"time":{seconds}}}"#);
                report(&mut link, &mut account, &base, "trial-result", body);
            }
            ApiRequest::CampaignResult { mission, lives } => {
                let body = format!(r#"{{"missionNum":{mission},"livesRemaining":{lives}}}"#);
                report(&mut link, &mut account, &base, "campaign-result", body);
            }
        }
    }
}

/// The four (or one) requests a refresh is.
///
/// Each has a distinct job, which is why the redundancy between them is only
/// apparent: `/api/profile` is the dossier and needs no token, `/api/credits`
/// is the balance **and the token check**, `/api/unlocks` is the armory ladder,
/// and `/api/leaderboard` is the standings. A guest issues the last one alone.
fn refresh(link: &mut ApiLink, account: &mut Account, base: &str) {
    start(
        link,
        account,
        Job::Leaderboard,
        Request::get(&format!("{base}/api/leaderboard"), None),
    );
    let (Some(token), Some(username)) = (account.token.clone(), account.username.clone()) else {
        return;
    };
    start(
        link,
        account,
        Job::Profile,
        Request::get(
            &format!("{base}/api/profile/{}", percent_encode(&username)),
            None,
        ),
    );
    start(
        link,
        account,
        Job::Credits,
        Request::get(&format!("{base}/api/credits"), Some(token.clone())),
    );
    start(
        link,
        account,
        Job::Unlocks,
        Request::get(&format!("{base}/api/unlocks"), Some(token)),
    );
}

/// One of the three `*-result` posts, or nothing at all for a guest.
///
/// A guest reporting a match is not an error and does not deserve a notice: the
/// JS does exactly this (`if (!token) return`), and a tutorial flown without an
/// account should say nothing about it.
fn report(link: &mut ApiLink, account: &mut Account, base: &str, what: &'static str, body: String) {
    let Some(token) = account.token.clone() else {
        return;
    };
    start(
        link,
        account,
        Job::Report(what),
        Request::post(&format!("{base}/api/{what}"), Some(token), body),
    );
}

/// Starts one request and counts it.
fn start(link: &mut ApiLink, account: &mut Account, job: Job, request: Request) {
    match request.send() {
        Ok(fetch) => {
            link.inflight.push((job, fetch));
            account.busy = account.busy.saturating_add(1);
            account.bump();
        }
        Err(err) => {
            // A malformed URL or a `https://` endpoint fails here rather than
            // four layers down, and is reported the same way a refused
            // connection is.
            warn!("api: {job:?} could not start: {err}");
            account.say(err);
        }
    }
}

/// Polls everything in flight and applies whatever landed.
fn pump(
    mut link: NonSendMut<ApiLink>,
    mut account: ResMut<Account>,
    mut data: ResMut<LobbyData>,
    mut config: ResMut<NetConfig>,
    status: Res<NetStatus>,
    mut commands: MessageWriter<NetCommand>,
) {
    let mut done: Vec<(Job, Result<Response, String>)> = Vec::new();
    link.inflight.retain_mut(|(job, fetch)| match fetch.poll() {
        Some(result) => {
            done.push((job.clone(), result));
            false
        }
        None => true,
    });
    if done.is_empty() {
        return;
    }
    for (job, result) in done {
        account.busy = account.busy.saturating_sub(1);
        apply(&job, result, &mut link, &mut account, &mut data);
    }
    account.bump();
    push_token_to_socket(&account, &mut config, &status, &mut commands);
}

/// One reply.
fn apply(
    job: &Job,
    result: Result<Response, String>,
    link: &mut ApiLink,
    account: &mut Account,
    data: &mut LobbyData,
) {
    let response = match result {
        Ok(r) => r,
        Err(err) => {
            // No network, no server, a timeout. The data already on screen is
            // kept and marked, which is the honest thing: it *was* true.
            warn!("api: {job:?} failed: {err}");
            mark_stale(data);
            account.say(offline_notice(&err));
            return;
        }
    };

    let body: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);

    if response.status == 401 && job.is_authenticated() {
        // The seven-day token ran out, or the server's `JWT_SECRET` changed.
        // Either way this credential is dead and keeping it would fail every
        // request from here on.
        sign_out(account, data, "SESSION EXPIRED - SIGN IN AGAIN");
        return;
    }
    if !response.ok() {
        let message = body.get("error").and_then(Value::as_str).map_or_else(
            || format!("SERVER SAID {}", response.status),
            str::to_uppercase,
        );
        warn!("api: {job:?} -> {} {message}", response.status);
        // A rejected sign-in is the user's problem to fix and leaves the data
        // alone; a failed read is the feed's problem and marks it.
        match job {
            Job::SignIn | Job::Enlist { .. } | Job::Unlock | Job::Report(_) => {}
            _ => mark_stale(data),
        }
        account.say(message);
        return;
    }

    match job {
        Job::SignIn => {
            let Some(token) = body.get("token").and_then(Value::as_str) else {
                account.say("SERVER SENT NO TOKEN");
                return;
            };
            let username = body
                .get("username")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            account.token = Some(token.to_owned());
            account.username = Some(username.clone());
            store::save(token, &username);
            // `login` answers with the whole pilot row, so the record is
            // complete before a single further request goes out. The refresh
            // below is for the leaderboard and the unlock flags.
            data.pilot = record_from(&body, &username);
            data.source = DataSource::Live;
            data.bump();
            account.say(format!("WELCOME BACK, {}", username.to_uppercase()));
            link.queued.push(ApiRequest::Refresh { force: true });
        }
        Job::Enlist { username, password } => {
            account.say("ENLISTED");
            link.queued.push(ApiRequest::SignIn {
                username: username.clone(),
                password: password.clone(),
            });
        }
        Job::Profile => {
            let Some(profile) = body.get("profile") else {
                account.say("NO SUCH PILOT");
                return;
            };
            let name = profile
                .get("username")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            // The unlock flags are `Profile`'s too, but `Unlocks` is the
            // authoritative read and lands beside this one; taking them here as
            // well means a refresh that loses one request still has them.
            data.pilot = record_from(profile, &name);
            data.source = DataSource::Live;
            mark_you(data, account);
            data.bump();
        }
        Job::Credits => {
            if let Some(credits) = number(body.get("credits")) {
                data.pilot.credits = credits as u32;
                data.source = DataSource::Live;
                data.bump();
            }
        }
        Job::Unlocks => {
            data.pilot.owned = owned_bits(&body);
            data.source = DataSource::Live;
            data.bump();
        }
        Job::Leaderboard => {
            if let Some(rows) = body.get("leaderboard").and_then(Value::as_array) {
                data.standings = rows.iter().map(standing_from).collect();
                mark_you(data, account);
                if data.source != DataSource::Live {
                    data.source = if account.signed_in() {
                        DataSource::Live
                    } else {
                        DataSource::Public
                    };
                }
                data.bump();
            }
        }
        Job::Unlock => {
            if let Some(balance) = number(body.get("balance")) {
                data.pilot.credits = balance as u32;
                data.bump();
            }
            account.say(if truthy(body.get("alreadyOwned")) {
                "ALREADY HELD"
            } else {
                "REQUISITION APPROVED"
            });
            // The reply says what it cost, never what is now held. Re-read.
            link.queued.push(ApiRequest::Refresh { force: true });
        }
        Job::Report(what) => {
            if let Some(total) = number(body.get("totalCredits")) {
                data.pilot.credits = total as u32;
                data.bump();
            }
            let earned = number(body.get("creditsEarned")).unwrap_or(0.0);
            info!("api: {what} recorded (+{earned} credits)");
            if earned > 0.0 {
                account.say(format!("RECORDED  +{earned} CREDITS"));
            }
            link.queued.push(ApiRequest::Refresh { force: true });
        }
    }
}

/// Drops the credential and puts the lobby back to a guest.
///
/// The callsign is kept when it is the pilot's own — signing out should not
/// rename the aircraft mid-session — but everything that was earned goes,
/// because none of it belongs to a guest.
fn sign_out(account: &mut Account, data: &mut LobbyData, why: &str) {
    account.token = None;
    account.username = None;
    store::clear();
    account.say(why);
    data.pilot = PilotRecord::guest(data.pilot.callsign.clone());
    data.source = if data.standings.is_empty() {
        DataSource::Placeholder
    } else {
        DataSource::Public
    };
    for row in &mut data.standings {
        row.you = false;
    }
    data.bump();
}

/// Marks the feed stale, but only when there was something real to go stale.
fn mark_stale(data: &mut LobbyData) {
    if data.source != DataSource::Placeholder {
        data.source = DataSource::Stale;
        data.bump();
    }
}

/// Flags the local pilot's row in the standings.
fn mark_you(data: &mut LobbyData, account: &Account) {
    let Some(me) = account.username.as_deref() else {
        return;
    };
    for row in &mut data.standings {
        row.you = row.callsign.eq_ignore_ascii_case(me);
    }
}

/// Keeps the socket's token equal to the account's.
///
/// This is the whole of "an authenticated player's kills persist": the server
/// ties a match to a pilot from `?token=` on the upgrade, and a socket opened
/// before the sign-in is carrying the wrong one (or none). Reconnecting is
/// cheap and is only done when there is a socket to reconnect — a lobby that
/// has never opened one just gets the right token when it does.
fn push_token_to_socket(
    account: &Account,
    config: &mut NetConfig,
    status: &NetStatus,
    commands: &mut MessageWriter<NetCommand>,
) {
    if config.token == account.token {
        return;
    }
    config.token.clone_from(&account.token);
    if status.state != ConnState::Offline {
        info!("net: reconnecting to re-present the token");
        commands.write(NetCommand::Reconnect);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Decoding
// ─────────────────────────────────────────────────────────────────────────────

/// A `login` or `profile` payload as a [`PilotRecord`].
///
/// The two share every field this needs — `loginPilot` and `getPilotProfile` in
/// `server/db.js` build the same object from the same row — so one decoder
/// serves both, and the day they diverge one of them stops filling a field
/// rather than one of two decoders going quietly stale.
fn record_from(v: &Value, username: &str) -> PilotRecord {
    let n = |key: &str| number(v.get(key)).unwrap_or(0.0).max(0.0) as u32;
    let games = n("gamesPlayed");
    PilotRecord {
        callsign: if username.is_empty() {
            v.get("username")
                .and_then(Value::as_str)
                .unwrap_or("PILOT")
                .to_owned()
        } else {
            username.to_owned()
        },
        rank: v
            .get("rank")
            .and_then(Value::as_str)
            .unwrap_or("CADET")
            .to_owned(),
        kdr: v
            .get("kdr")
            .and_then(Value::as_str)
            .unwrap_or("0.00")
            .to_owned(),
        enlisted: number(v.get("createdAt")).map_or_else(|| "-".to_owned(), |t| iso_date(t as i64)),
        credits: n("credits"),
        kills: n("totalKills"),
        deaths: n("totalDeaths"),
        matches_won: n("matchesWon"),
        matches_lost: n("matchesLost"),
        bots_killed: n("botsKilled"),
        // `gamesPlayed` at the nominal five-minute match. An estimate, and
        // labelled as flight hours rather than as anything the server counted.
        flight_minutes: games.saturating_mul(5),
        trial_best: [
            trial(v, "trial1Best"),
            trial(v, "trial2Best"),
            trial(v, "trial3Best"),
            trial(v, "trial4Best"),
        ],
        campaign_lives: [
            lives(v, "campaign1BestLives"),
            lives(v, "campaign2BestLives"),
            lives(v, "campaign3BestLives"),
        ],
        campaign_boss_kills: n("campaignBossKills"),
        owned: owned_bits(v),
    }
}

fn trial(v: &Value, key: &str) -> Option<f32> {
    number(v.get(key)).filter(|t| *t > 0.0).map(|t| t as f32)
}

fn lives(v: &Value, key: &str) -> Option<u8> {
    number(v.get(key)).map(|l| l.clamp(0.0, 3.0) as u8)
}

/// One `/api/leaderboard` row.
fn standing_from(v: &Value) -> Standing {
    Standing {
        callsign: v
            .get("username")
            .and_then(Value::as_str)
            .unwrap_or("-")
            .to_owned(),
        rank: v
            .get("pilotRank")
            .and_then(Value::as_str)
            .unwrap_or("CADET")
            .to_owned(),
        kills: number(v.get("totalKills")).unwrap_or(0.0).max(0.0) as u32,
        wins: number(v.get("matchesWon")).unwrap_or(0.0).max(0.0) as u32,
        // Filled by `mark_you` once the account is known, which is not
        // necessarily when this row is decoded.
        you: false,
    }
}

/// `Number(v)`, near enough: the server sends integers, floats and `null` in
/// the same fields depending on the column, and `null` is "no value" rather
/// than zero.
fn number(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        _ => false,
    }
}

/// One JSON object with two string fields, escaped.
///
/// Hand-built rather than `serde_json::json!` because the escaping is the only
/// part that matters and doing it explicitly is what makes it reviewable: a
/// password may contain anything at all, including a quote and a backslash.
fn credentials(username: &str, password: &str) -> String {
    format!(
        r#"{{"username":{},"password":{}}}"#,
        Value::String(username.to_owned()),
        Value::String(password.to_owned())
    )
}

/// `encodeURIComponent` for a path segment.
///
/// A callsign is `[A-Za-z0-9]{3,20}` by the server's own rule, so this never
/// has anything to do — but the value reaching it came out of a text field, and
/// a username with a `/` in it must not become a different route.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(byte) {
            out.push(*byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

/// A Unix timestamp as `YYYY-MM-DD`, UTC.
///
/// Hinnant's civil-from-days, which is exact for every date this will ever see
/// and is eleven lines against a calendar crate. `sim`'s no-dependency rule
/// does not apply to this crate, but a date formatter is not a dependency worth
/// having.
fn iso_date(unix_seconds: i64) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// A transport error as something the footer can print.
///
/// The underlying strings are `std::io::Error`'s and a browser's, and neither
/// is written for a pilot. The distinction that matters is only ever "is the
/// server there".
fn offline_notice(err: &str) -> &'static str {
    let e = err.to_ascii_lowercase();
    if e.contains("refused") || e.contains("unreachable") || e.contains("failed to fetch") {
        "SERVER NOT ANSWERING"
    } else if e.contains("timed out") || e.contains("timeout") {
        "SERVER TIMED OUT"
    } else if e.contains("resolve") || e.contains("nodename") || e.contains("name or service") {
        "SERVER NOT FOUND"
    } else {
        "DATA LINK FAILED"
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Match results
// ─────────────────────────────────────────────────────────────────────────────

/// What this match has been worth so far.
///
/// The simulation does not keep a per-pilot scoreline in [`sim::world::Frame`]
/// — `Frame::ships` carries no kill count — so the tally is accumulated from
/// [`SimEvent::ShipDestroyed`] as it happens, which is also exactly what
/// `main.js` does with its `scores` map.
#[derive(Resource, Debug, Default)]
struct Tally {
    kills: u32,
    deaths: u32,
    /// Every opponent in a solo match is a bot, so this is the same number as
    /// `kills` there — kept separate because the endpoint takes both and a
    /// networked match would not.
    bots: u32,
    /// Whether this match has already been reported, so a result card that
    /// stays on screen for six seconds reports once.
    reported: bool,
    /// The mode the tally belongs to, so a new match resets it.
    mode: Mode,
}

/// Reports a finished solo match, a completed circuit, or a won operation.
///
/// Multiplayer is **not** here: `server/index.js` persists every authenticated
/// pilot's stats itself at `match-end`, and the socket now carries the token
/// that ties the connection to the pilot. Posting a solo result for a networked
/// match would double-count it.
fn report_results(
    frame: Res<SimFrame>,
    setup: Res<MatchSetup>,
    mut tally: ResMut<Tally>,
    mut out: MessageWriter<ApiRequest>,
) {
    if tally.mode != setup.mode {
        *tally = Tally {
            mode: setup.mode,
            ..Tally::default()
        };
    }

    for event in &frame.0.events {
        match *event {
            SimEvent::ShipDestroyed { id, killer, .. } => {
                if id == LOCAL_ID {
                    tally.deaths += 1;
                } else if killer == Some(LOCAL_ID) {
                    tally.kills += 1;
                    if setup.mode.is_solo() {
                        tally.bots += 1;
                    }
                }
            }
            // A circuit is reported on the lap, not at the end of the match:
            // there is no end. The server keeps the personal best and ignores
            // anything slower, so sending only an improvement is an
            // optimisation rather than a rule.
            SimEvent::LapComplete { time, is_best } => {
                if is_best {
                    if let Mode::Trials(n) = setup.mode {
                        out.write(ApiRequest::TrialResult {
                            number: n,
                            seconds: time,
                        });
                    }
                }
            }
            SimEvent::CampaignVictory { lives_left } => {
                if let Mode::Campaign(m) = setup.mode {
                    out.write(ApiRequest::CampaignResult {
                        mission: m,
                        lives: lives_left.clamp(0, 3) as u8,
                    });
                }
            }
            SimEvent::MatchEnded { winner } => {
                if tally.reported || !setup.mode.is_solo() {
                    continue;
                }
                tally.reported = true;
                out.write(ApiRequest::SoloResult {
                    kills: tally.kills,
                    deaths: tally.deaths,
                    bots: tally.bots,
                    won: outcome(winner, &frame.0),
                });
            }
            _ => {}
        }
    }
}

/// Whether the local pilot won, as `/api/solo-result`'s tri-state `won`.
///
/// `null` is a draw *or* a match the local pilot was not on a side of, which is
/// the same reading `hud.rs::watch_match_end` takes for its result card — one
/// definition of "did I win", two consumers.
fn outcome(winner: Option<Team>, frame: &sim::world::Frame) -> Option<bool> {
    let mine = frame
        .ships
        .iter()
        .find(|s| s.flags.contains(sim::world::ShipFlags::LOCAL))
        .map(|s| s.team)
        .filter(|t| *t >= 0)?;
    let winner = winner?;
    // `Team::index()` is a `usize` and `ShipView::team` an `i32` with `-1` for
    // "no team" (`tick.rs`), which the filter above has already excluded.
    Some(i32::try_from(winner.index()).is_ok_and(|w| w == mine))
}

// ─────────────────────────────────────────────────────────────────────────────
// The credential store
// ─────────────────────────────────────────────────────────────────────────────

/// This installation's own directory, by the platform's convention.
///
/// The credential store was the first thing to need one and is no longer the
/// only one — `replay.rs` puts recordings in `replays/` beside it — so the
/// directory is resolved here and the file names are the callers'. See
/// [`store::path`] for why a bearer credential goes here rather than beside the
/// executable, and note the same reasoning does *not* apply to a recording: a
/// replay is not a secret, it lives here because this is where the game's own
/// files live.
///
/// `SPACESHIPS_STATE_DIR` overrides it, which is how the tests exercise both
/// without touching a real one.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn state_dir() -> Option<std::path::PathBuf> {
    if let Some(explicit) = std::env::var_os("SPACESHIPS_STATE_DIR") {
        return Some(std::path::PathBuf::from(explicit));
    }
    Some(if cfg!(target_os = "macos") {
        std::path::PathBuf::from(std::env::var_os("HOME")?)
            .join("Library/Application Support/Spaceships")
    } else if cfg!(windows) {
        std::path::PathBuf::from(std::env::var_os("APPDATA")?).join("Spaceships")
    } else if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        std::path::PathBuf::from(xdg).join("spaceships")
    } else {
        std::path::PathBuf::from(std::env::var_os("HOME")?).join(".config/spaceships")
    })
}

/// Where the JWT is kept between runs.
///
/// Two implementations, one contract: [`load`](store::load), [`save`](store::save),
/// [`clear`](store::clear).
mod store {
    /// `localStorage['spaceships:token']`, the key `public/src/auth.js` uses,
    /// so the JS client and the wasm client share one session.
    #[cfg(target_arch = "wasm32")]
    pub const TOKEN_KEY: &str = "spaceships:token";
    #[cfg(target_arch = "wasm32")]
    const NAME_KEY: &str = "spaceships:callsign";

    #[cfg(target_arch = "wasm32")]
    fn storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok().flatten()
    }

    #[cfg(target_arch = "wasm32")]
    pub fn load() -> Option<(String, Option<String>)> {
        let store = storage()?;
        let token = store
            .get_item(TOKEN_KEY)
            .ok()
            .flatten()
            .filter(|t| !t.is_empty())?;
        let name = store
            .get_item(NAME_KEY)
            .ok()
            .flatten()
            .filter(|n| !n.is_empty());
        Some((token, name))
    }

    #[cfg(target_arch = "wasm32")]
    pub fn save(token: &str, username: &str) {
        let Some(store) = storage() else { return };
        let _ = store.set_item(TOKEN_KEY, token);
        let _ = store.set_item(NAME_KEY, username);
    }

    #[cfg(target_arch = "wasm32")]
    pub fn clear() {
        let Some(store) = storage() else { return };
        let _ = store.remove_item(TOKEN_KEY);
        let _ = store.remove_item(NAME_KEY);
    }

    // -- native ------------------------------------------------------------

    /// A file under the user's own configuration directory, mode `0600`.
    ///
    /// # Why here and not somewhere easier
    ///
    /// The token is a bearer credential: anything holding it *is* the pilot,
    /// for seven days, with no password. So the two easy answers are both
    /// wrong. A file beside the executable travels with a copied `.app` and
    /// lands in whatever directory the binary was unpacked into, which on a
    /// shared machine is frequently world-readable. An environment variable is
    /// visible in `ps` output on some systems and is inherited by every child
    /// process the game ever spawns.
    ///
    /// What this is **not** is the system keychain. macOS `Security.framework`
    /// is the right home for a credential and would mean an `objc`/`security-
    /// framework` dependency and a platform-specific path per target for a game
    /// whose account grants access to a leaderboard. `0600` in the user's own
    /// config directory is the same protection `~/.ssh/config`, `~/.netrc` and
    /// `~/.aws/credentials` rely on, and it is honest about what it is: safe
    /// from other users on the machine, not from anything running *as* the
    /// user.
    ///
    /// `SPACESHIPS_STATE_DIR` overrides the directory, which is how the tests
    /// exercise this without touching a real one.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn path() -> Option<std::path::PathBuf> {
        Some(super::state_dir()?.join("credentials.json"))
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn load() -> Option<(String, Option<String>)> {
        // The developer's override, and the reason nothing is ever written back
        // to it: a value that came from the environment is the operator's, not
        // this file's, and overwriting it would be a surprise.
        if let Some(token) = std::env::var("SPACESHIPS_TOKEN")
            .ok()
            .filter(|t| !t.is_empty())
        {
            let name = std::env::var("SPACESHIPS_CALLSIGN")
                .ok()
                .filter(|n| !n.trim().is_empty());
            return Some((token, name));
        }
        load_from(&path()?)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn save(token: &str, username: &str) {
        let Some(path) = path() else { return };
        if let Err(e) = save_to(&path, token, username) {
            bevy::log::warn!("api: could not save the credential: {e}");
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn clear() {
        let Some(path) = path() else { return };
        clear_at(&path);
    }

    // The three above resolve *where*; the three below do the work, and take
    // the path. Split so the tests can exercise a real file in a directory they
    // own without touching the process environment — `std::env::set_var` is
    // process-global and cargo runs tests on threads, so a test that moved
    // `SPACESHIPS_STATE_DIR` raced every other test that reads an environment
    // variable. It did, intermittently, before this split.

    #[cfg(not(target_arch = "wasm32"))]
    pub fn load_from(path: &std::path::Path) -> Option<(String, Option<String>)> {
        let text = std::fs::read_to_string(path).ok()?;
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        let token = v
            .get("token")
            .and_then(serde_json::Value::as_str)
            .filter(|t| !t.is_empty())?
            .to_owned();
        let name = v
            .get("username")
            .and_then(serde_json::Value::as_str)
            .filter(|n| !n.is_empty())
            .map(str::to_owned);
        Some((token, name))
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn save_to(path: &std::path::Path, token: &str, username: &str) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            restrict(dir, 0o700);
        }
        let body = serde_json::json!({ "token": token, "username": username }).to_string();
        write_private(path, &body)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn clear_at(path: &std::path::Path) {
        // `NotFound` is the expected case for a guest who pressed sign out.
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => bevy::log::warn!("api: could not clear the credential: {e}"),
        }
    }

    /// Creates the file with the restrictive mode already applied, so the
    /// credential is never briefly readable.
    #[cfg(all(unix, not(target_arch = "wasm32")))]
    fn write_private(path: &std::path::Path, body: &str) -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(body.as_bytes())
    }

    /// Everywhere else the umask equivalent is the platform's business.
    #[cfg(all(not(unix), not(target_arch = "wasm32")))]
    fn write_private(path: &std::path::Path, body: &str) -> std::io::Result<()> {
        std::fs::write(path, body)
    }

    #[cfg(all(unix, not(target_arch = "wasm32")))]
    fn restrict(path: &std::path::Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }

    #[cfg(all(not(unix), not(target_arch = "wasm32")))]
    fn restrict(_path: &std::path::Path, _mode: u32) {}
}

// ─────────────────────────────────────────────────────────────────────────────
// The transport contract
// ─────────────────────────────────────────────────────────────────────────────

/// One request, before it has a backend.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Request {
    method: &'static str,
    url: String,
    /// The bearer token, if this endpoint needs one.
    token: Option<String>,
    /// A JSON body, or empty for a `GET`.
    body: String,
}

impl Request {
    fn get(url: &str, token: Option<String>) -> Request {
        Request {
            method: "GET",
            url: url.to_owned(),
            token,
            body: String::new(),
        }
    }

    fn post(url: &str, token: Option<String>, body: String) -> Request {
        Request {
            method: "POST",
            url: url.to_owned(),
            token,
            body,
        }
    }
}

/// What came back.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Response {
    status: u16,
    body: String,
}

impl Response {
    fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// A URL split into the pieces a request line needs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Parts {
    host: String,
    port: u16,
    /// Path plus query, starting with `/`.
    target: String,
}

/// Splits `http://host[:port]/path?query`.
///
/// Shared by both backends: the wasm one does not need it to *make* the
/// request, but it is where the `https://` refusal lives and a browser build
/// that quietly accepted one and then failed CORS would be worse.
fn split_url(url: &str) -> Result<Parts, String> {
    if url.starts_with("https://") {
        return Err(format!(
            "this client speaks http:// only ({url}); production is Caddy on \
             port 80 with no TLS listener, so a wss/https endpoint means the \
             deployment changed"
        ));
    }
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("not an http url: {url}"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err(format!("no host in {url}"));
    }
    // Only the last colon separates a port, so an IPv6 literal in brackets
    // survives — `[::1]:4000` splits at the colon after the bracket.
    let (host, port) = match authority.rfind(':') {
        Some(i) if authority[i + 1..].chars().all(|c| c.is_ascii_digit()) => (
            &authority[..i],
            authority[i + 1..]
                .parse::<u16>()
                .map_err(|e| e.to_string())?,
        ),
        _ => (authority, 80),
    };
    Ok(Parts {
        host: host.to_owned(),
        port,
        target: path.to_owned(),
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Native backend: one thread, std::net, HTTP/1.1 by hand
// ─────────────────────────────────────────────────────────────────────────────

/// Native `Fetch`.
///
/// One thread per request, joined implicitly by the channel closing. That is
/// affordable because a request is a rare event — a sign-in, or four reads when
/// a page comes up — and it removes an executor, a reactor and a connection
/// pool from a client that needs none of them.
#[cfg(not(target_arch = "wasm32"))]
struct Fetch {
    answer: std::sync::mpsc::Receiver<Result<Response, String>>,
    /// Set once the answer has been taken, so a second `poll` returns `None`
    /// rather than the channel's disconnect error.
    done: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl Request {
    fn send(self) -> Result<Fetch, String> {
        let parts = split_url(&self.url)?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("spaceships-api".to_owned())
            .spawn(move || {
                let _ = tx.send(exchange(&self, &parts));
            })
            .map_err(|e| format!("could not start the request thread: {e}"))?;
        Ok(Fetch {
            answer: rx,
            done: false,
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Fetch {
    /// Whatever has arrived. Never blocks.
    fn poll(&mut self) -> Option<Result<Response, String>> {
        if self.done {
            return None;
        }
        match self.answer.try_recv() {
            Ok(result) => {
                self.done = true;
                Some(result)
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            // The thread died without sending, which it cannot do without
            // panicking — report it rather than waiting forever.
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.done = true;
                Some(Err("the request thread stopped".to_owned()))
            }
        }
    }
}

/// One request and one response, synchronously, on the request's own thread.
#[cfg(not(target_arch = "wasm32"))]
fn exchange(request: &Request, parts: &Parts) -> Result<Response, String> {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};

    let addr = (parts.host.as_str(), parts.port)
        .to_socket_addrs()
        .map_err(|e| format!("could not resolve {}: {e}", parts.host))?
        .next()
        .ok_or_else(|| format!("could not resolve {}", parts.host))?;
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .map_err(|e| format!("could not reach {addr}: {e}"))?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(IO_TIMEOUT)))
        .map_err(|e| e.to_string())?;
    // A request is one small write followed by a wait, which is the case Nagle
    // delays.
    let _ = stream.set_nodelay(true);

    stream
        .write_all(wire_request(request, parts).as_bytes())
        .and_then(|()| {
            if request.body.is_empty() {
                Ok(())
            } else {
                stream.write_all(request.body.as_bytes())
            }
        })
        .and_then(|()| stream.flush())
        .map_err(|e| format!("could not send: {e}"))?;

    // `Connection: close` above, so the peer closing *is* the end of the
    // message and there is no framing to get wrong in the common case. The
    // chunked path below exists because a reverse proxy is entitled to re-frame
    // a response it is streaming, and Caddy sits in front of production.
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > MAX_BODY {
                    return Err("the response is implausibly large".to_owned());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("could not read: {e}")),
        }
    }
    parse_response(&raw)
}

/// The request line and headers.
///
/// Deliberately minimal and deliberately without `Accept-Encoding`: not asking
/// for compression is how this client is entitled to read the body as it
/// arrives, and it saves a decompressor.
#[cfg(not(target_arch = "wasm32"))]
fn wire_request(request: &Request, parts: &Parts) -> String {
    let mut head = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: spaceships-client\r\n\
         Accept: application/json\r\nConnection: close\r\n",
        request.method,
        parts.target,
        // The port belongs in `Host` unless it is the default, and Caddy routes
        // on it.
        if parts.port == 80 {
            parts.host.clone()
        } else {
            format!("{}:{}", parts.host, parts.port)
        }
    );
    if let Some(token) = &request.token {
        // A header value cannot contain CR or LF. A JWT is base64url and dots
        // and never could, but this value came off a text field by way of a
        // file, so the check is not theoretical.
        let clean: String = token.chars().filter(|c| !c.is_control()).collect();
        head.push_str(&format!("Authorization: Bearer {clean}\r\n"));
    }
    if !request.body.is_empty() {
        head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            request.body.len()
        ));
    } else if request.method != "GET" {
        // Express's JSON body parser is happy with an absent body, but a POST
        // with neither a length nor a chunked encoding is ambiguous framing.
        head.push_str("Content-Length: 0\r\n");
    }
    head.push_str("\r\n");
    head
}

/// Splits a raw HTTP/1.1 response into a status and a decoded body.
///
/// Kept as a free function over `&[u8]` so it can be tested against exact bytes
/// without a socket — which is the only way to be sure about the chunked path,
/// since a well-behaved server will not produce one on demand.
#[cfg(not(target_arch = "wasm32"))]
fn parse_response(raw: &[u8]) -> Result<Response, String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "the response has no header block".to_owned())?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let body = &raw[split + 4..];

    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("not an http response: {status_line}"))?;

    let chunked = lines.any(|line| {
        let (name, value) = line.split_once(':').unwrap_or(("", ""));
        name.eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().contains("chunked")
    });

    let body = if chunked {
        dechunk(body)?
    } else {
        body.to_vec()
    };
    Ok(Response {
        status,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

/// RFC 9112 §7.1, the part of it a JSON response can use: size line, data,
/// CRLF, repeat, terminated by a zero-length chunk. Extensions and trailers are
/// skipped rather than parsed.
#[cfg(not(target_arch = "wasm32"))]
fn dechunk(mut input: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(input.len());
    loop {
        let eol = input
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| "truncated chunk header".to_owned())?;
        let header = String::from_utf8_lossy(&input[..eol]);
        // `1a;ext=1` — the size ends at the first semicolon.
        let size_text = header.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| format!("bad chunk size {size_text:?}"))?;
        input = &input[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        if input.len() < size {
            return Err("truncated chunk body".to_owned());
        }
        out.extend_from_slice(&input[..size]);
        input = &input[size..];
        // The CRLF after the data. Tolerated as absent at the very end rather
        // than failing on a response that is otherwise complete.
        if input.starts_with(b"\r\n") {
            input = &input[2..];
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Web backend: window.fetch
// ─────────────────────────────────────────────────────────────────────────────

/// Browser `Fetch`.
///
/// No thread, no parser, no framing: the browser owns the connection. The
/// answer lands in an `Rc<RefCell<..>>` from a `spawn_local` future, which is
/// why this type can never be `Send` and why [`ApiLink`] is a non-send
/// resource.
#[cfg(target_arch = "wasm32")]
struct Fetch {
    slot: std::rc::Rc<std::cell::RefCell<Option<Result<Response, String>>>>,
}

#[cfg(target_arch = "wasm32")]
impl Request {
    fn send(self) -> Result<Fetch, String> {
        use wasm_bindgen::JsCast;

        // Not needed to build the request — the browser parses the URL — but it
        // is where `https://` is refused, and a build that accepted one here
        // and failed later would be harder to read.
        split_url(&self.url)?;

        let window = web_sys::window().ok_or_else(|| "no window".to_owned())?;
        let init = web_sys::RequestInit::new();
        init.set_method(self.method);
        // Same-origin by construction: `api_base_for` derives the base from the
        // page. Naming it means a mistake shows up as a clear CORS refusal
        // rather than as an opaque response with an unreadable body.
        init.set_mode(web_sys::RequestMode::Cors);
        let headers = web_sys::Headers::new().map_err(describe_js)?;
        headers
            .set("Accept", "application/json")
            .map_err(describe_js)?;
        if let Some(token) = &self.token {
            headers
                .set("Authorization", &format!("Bearer {token}"))
                .map_err(describe_js)?;
        }
        if !self.body.is_empty() {
            headers
                .set("Content-Type", "application/json")
                .map_err(describe_js)?;
            init.set_body(&wasm_bindgen::JsValue::from_str(&self.body));
        }
        init.set_headers(headers.as_ref());

        let request =
            web_sys::Request::new_with_str_and_init(&self.url, &init).map_err(describe_js)?;

        let slot = std::rc::Rc::new(std::cell::RefCell::new(None));
        let out = std::rc::Rc::clone(&slot);
        wasm_bindgen_futures::spawn_local(async move {
            let answer = async {
                let response =
                    wasm_bindgen_futures::JsFuture::from(window.fetch_with_request(&request))
                        .await
                        .map_err(describe_js)?;
                let response: web_sys::Response = response
                    .dyn_into()
                    .map_err(|_| "not a response".to_owned())?;
                let status = response.status();
                let text =
                    wasm_bindgen_futures::JsFuture::from(response.text().map_err(describe_js)?)
                        .await
                        .map_err(describe_js)?;
                Ok(Response {
                    status,
                    body: text.as_string().unwrap_or_default(),
                })
            }
            .await;
            *out.borrow_mut() = Some(answer);
        });
        Ok(Fetch { slot })
    }
}

#[cfg(target_arch = "wasm32")]
impl Fetch {
    fn poll(&mut self) -> Option<Result<Response, String>> {
        self.slot.borrow_mut().take()
    }
}

/// A `JsValue` as something printable. `fetch` rejects with a `TypeError` whose
/// message is "Failed to fetch" for every network-level failure, deliberately,
/// to avoid leaking cross-origin information — so this is as specific as the
/// web half can be, and [`offline_notice`] knows that string.
#[cfg(target_arch = "wasm32")]
fn describe_js(value: wasm_bindgen::JsValue) -> String {
    // `JsCast` is what puts `dyn_ref` on `JsValue`; it is a trait method, not an
    // inherent one, so it has to be in scope at the call site.
    use wasm_bindgen::JsCast;
    value
        .as_string()
        .or_else(|| {
            value
                .dyn_ref::<js_sys::Error>()
                .map(|e| String::from(e.message()))
        })
        .unwrap_or_else(|| format!("{value:?}"))
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::platform::collections::HashMap;

    // -- URLs ---------------------------------------------------------------

    #[test]
    fn a_url_splits_into_host_port_and_target() {
        assert_eq!(
            split_url("http://127.0.0.1:4000/api/login").unwrap(),
            Parts {
                host: "127.0.0.1".to_owned(),
                port: 4000,
                target: "/api/login".to_owned(),
            }
        );
        // No port is port 80, which is what production is.
        assert_eq!(
            split_url("http://gheat.net/spaceships/api/leaderboard").unwrap(),
            Parts {
                host: "gheat.net".to_owned(),
                port: 80,
                target: "/spaceships/api/leaderboard".to_owned(),
            }
        );
        // A bare authority still addresses the root.
        assert_eq!(split_url("http://h").unwrap().target, "/");
        // The query rides along in the target.
        assert_eq!(
            split_url("http://h/api/credits/history?limit=5")
                .unwrap()
                .target,
            "/api/credits/history?limit=5"
        );
    }

    /// The refusal names the reason rather than failing inside a socket, which
    /// is the same courtesy `net::Socket::connect` extends to `wss://`.
    #[test]
    fn https_is_refused_with_a_reason() {
        let err = split_url("https://gheat.net/api/login").unwrap_err();
        assert!(err.contains("http:// only"), "{err}");
        assert!(split_url("ftp://h/x").is_err());
        assert!(
            split_url("http:///x").is_err(),
            "an empty host is not a host"
        );
    }

    #[test]
    fn a_path_segment_is_percent_encoded() {
        assert_eq!(percent_encode("Maverick"), "Maverick");
        assert_eq!(percent_encode("a/b"), "a%2Fb");
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_encode("é"), "%C3%A9");
    }

    /// A password is arbitrary text and goes out inside JSON.
    #[test]
    fn credentials_escape_their_contents() {
        assert_eq!(
            credentials("ace", "p\"a\\s s"),
            r#"{"username":"ace","password":"p\"a\\s s"}"#
        );
        let v: Value = serde_json::from_str(&credentials("a", "\n\t")).unwrap();
        assert_eq!(v["password"], "\n\t");
    }

    // -- the wire -----------------------------------------------------------

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_get_carries_no_body_and_names_its_host() {
        let parts = split_url("http://127.0.0.1:4100/api/credits").unwrap();
        let head = wire_request(
            &Request::get(
                "http://127.0.0.1:4100/api/credits",
                Some("abc.def".to_owned()),
            ),
            &parts,
        );
        assert!(head.starts_with("GET /api/credits HTTP/1.1\r\n"), "{head}");
        assert!(head.contains("Host: 127.0.0.1:4100\r\n"), "{head}");
        assert!(head.contains("Authorization: Bearer abc.def\r\n"), "{head}");
        assert!(head.contains("Connection: close\r\n"), "{head}");
        assert!(!head.contains("Content-Length"), "{head}");
        assert!(head.ends_with("\r\n\r\n"), "{head}");
    }

    /// Port 80 is left out of `Host`, which is what every other client does and
    /// what a virtual-host match expects.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_default_port_is_left_out_of_the_host_header() {
        let parts = split_url("http://gheat.net/spaceships/api/login").unwrap();
        let head = wire_request(
            &Request::post(
                "http://gheat.net/spaceships/api/login",
                None,
                "{\"a\":1}".to_owned(),
            ),
            &parts,
        );
        assert!(head.contains("Host: gheat.net\r\n"), "{head}");
        assert!(head.contains("Content-Length: 7\r\n"), "{head}");
        assert!(
            head.contains("Content-Type: application/json\r\n"),
            "{head}"
        );
    }

    /// A token that somehow acquired a newline must not be able to inject a
    /// header.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_token_cannot_smuggle_a_header() {
        let parts = split_url("http://h/api/credits").unwrap();
        let head = wire_request(
            &Request::get("http://h/api/credits", Some("a\r\nX-Evil: 1".to_owned())),
            &parts,
        );
        assert!(
            head.contains("Authorization: Bearer aX-Evil: 1\r\n"),
            "{head}"
        );
        assert_eq!(head.matches("\r\n\r\n").count(), 1, "{head}");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_plain_response_parses() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                    Content-Length: 11\r\n\r\n{\"ok\":true}";
        let response = parse_response(raw).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "{\"ok\":true}");
        assert!(response.ok());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn an_error_status_is_kept_with_its_body() {
        let raw =
            b"HTTP/1.1 401 Unauthorized\r\n\r\n{\"ok\":false,\"error\":\"Not authenticated\"}";
        let response = parse_response(raw).unwrap();
        assert_eq!(response.status, 401);
        assert!(!response.ok());
        assert!(response.body.contains("Not authenticated"));
    }

    /// Caddy is entitled to re-frame a response it proxies, so this path is on
    /// the way to production even though the origin server never chunks.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_chunked_response_is_reassembled() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
                    5\r\n{\"ok\"\r\n6;x=1\r\n:true}\r\n0\r\n\r\n";
        let response = parse_response(raw).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "{\"ok\":true}");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_truncated_response_is_an_error_not_a_panic() {
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n").is_err());
        assert!(parse_response(b"garbage\r\n\r\n").is_err());
        assert!(dechunk(b"zz\r\n").is_err());
        assert!(dechunk(b"ff\r\nshort").is_err());
    }

    // -- decoding -----------------------------------------------------------

    /// The exact object `server/db.js`'s `loginPilot` returns, spread into the
    /// reply by `res.json({ ok: true, ...result })`.
    fn login_body() -> Value {
        serde_json::json!({
            "ok": true,
            "token": "header.payload.signature",
            "username": "Maverick",
            "rank": "Ace",
            "highScore": 12,
            "gamesPlayed": 24,
            "totalKills": 57,
            "totalDeaths": 31,
            "matchesWon": 9,
            "matchesLost": 7,
            "botsKilled": 140,
            "kdr": "1.84",
            "trial1Best": 92.44,
            "trial2Best": null,
            "trial3Best": null,
            "trial4Best": null,
            "credits": 1234,
            "unlockHull": true,
            "unlockAccent": false,
            "unlockTrail": false,
            "unlockTrailShape": true,
            "unlockAdminShip": false,
            "campaign1BestLives": 3,
            "campaign2BestLives": null,
            "campaign3BestLives": null,
            "campaignBossKills": 1,
            "campaignTotalCompletions": 1,
            "createdAt": 1_772_000_000
        })
    }

    #[test]
    fn a_login_payload_becomes_a_pilot_record() {
        let pilot = record_from(&login_body(), "Maverick");
        assert_eq!(pilot.callsign, "Maverick");
        assert_eq!(pilot.rank, "Ace");
        assert_eq!(pilot.kdr, "1.84");
        assert_eq!(pilot.credits, 1234);
        assert_eq!(pilot.kills, 57);
        assert_eq!(pilot.deaths, 31);
        assert_eq!(pilot.bots_killed, 140);
        // 24 matches at the nominal five minutes.
        assert_eq!(pilot.flight_minutes, 120);
        assert_eq!(pilot.trial_best[0], Some(92.44));
        assert_eq!(pilot.trial_best[1], None, "null is no time, not zero");
        assert_eq!(pilot.campaign_lives, [Some(3), None, None]);
        assert_eq!(pilot.campaign_boss_kills, 1);
    }

    /// The five booleans the database actually has, in the rungs `ui::ARMORY`
    /// gives them.
    #[test]
    fn the_unlock_flags_become_armory_rungs() {
        let owned = record_from(&login_body(), "Maverick").owned;
        assert_ne!(owned & 1, 0, "the issued airframe is always held");
        assert_ne!(owned & (1 << 3), 0, "unlockHull is the hull colour rung");
        assert_ne!(owned & (1 << 2), 0, "unlockTrailShape is the trail profile");
        assert_eq!(owned & (1 << 4), 0, "unlockAccent was false");
        assert_eq!(owned & (1 << 15), 0, "the admin ship was not bought");
        // Nothing outside the table and bit 0 can be set.
        let allowed = UNLOCKS.iter().fold(1u32, |m, (i, _, _)| m | (1 << i));
        assert_eq!(owned & !allowed, 0);
    }

    /// Pinned against `UNLOCK_COSTS` in `server/db.js`. A price that moves
    /// there and not here would show the pilot one number and charge another.
    #[test]
    fn the_unlock_prices_match_the_server() {
        let expected: HashMap<&str, u32> = [
            ("hull", 250),
            ("accent", 400),
            ("trail", 500),
            ("trail_shape", 200),
            ("admin_ship", 125_000),
        ]
        .into_iter()
        .collect();
        assert_eq!(UNLOCKS.len(), expected.len());
        for (_, key, cost) in UNLOCKS {
            assert_eq!(expected.get(key), Some(&cost), "{key}");
        }
        assert_eq!(unlock_key(3), Some("hull"));
        assert_eq!(unlock_key(0), None, "the issued airframe is not for sale");
        assert_eq!(unlock_key(8), None, "MK.III has no column on the server");
    }

    #[test]
    fn a_leaderboard_row_becomes_a_standing() {
        let row = serde_json::json!({
            "position": 1, "username": "Vandal", "pilotRank": "Grand Admiral",
            "totalKills": 4120, "totalDeaths": 900, "matchesWon": 611,
            "matchesLost": 40, "gamesPlayed": 651, "highScore": 30, "kdr": "4.58"
        });
        let s = standing_from(&row);
        assert_eq!(s.callsign, "Vandal");
        assert_eq!(s.rank, "Grand Admiral");
        assert_eq!(s.kills, 4120);
        assert_eq!(s.wins, 611);
        assert!(!s.you, "whose row it is is decided later");
    }

    #[test]
    fn the_local_pilots_row_is_flagged_case_insensitively() {
        let mut data = LobbyData::placeholder();
        data.standings = ["Vandal", "maverick"]
            .into_iter()
            .map(|c| Standing {
                callsign: c.to_owned(),
                rank: "Cadet".to_owned(),
                kills: 0,
                wins: 0,
                you: true,
            })
            .collect();
        let account = Account {
            username: Some("MAVERICK".to_owned()),
            ..Account::default()
        };
        mark_you(&mut data, &account);
        assert!(!data.standings[0].you);
        assert!(data.standings[1].you);
    }

    #[test]
    fn a_missing_field_reads_as_absent_rather_than_zero() {
        let pilot = record_from(&serde_json::json!({}), "Nobody");
        assert_eq!(pilot.callsign, "Nobody");
        assert_eq!(pilot.rank, "CADET");
        assert_eq!(pilot.credits, 0);
        assert_eq!(pilot.trial_best, [None; 4]);
        assert_eq!(pilot.enlisted, "-");
        assert_eq!(pilot.owned, 1);
    }

    #[test]
    fn dates_come_out_iso() {
        assert_eq!(iso_date(0), "1970-01-01");
        assert_eq!(iso_date(1_772_000_000), "2026-02-25");
        // A leap day, and the day after.
        assert_eq!(iso_date(1_709_164_800), "2024-02-29");
        assert_eq!(iso_date(1_709_251_200), "2024-03-01");
    }

    // -- behaviour ----------------------------------------------------------

    #[test]
    fn the_feed_lamp_names_every_source() {
        assert_eq!(DataSource::Placeholder.label(), "CACHED");
        assert_eq!(DataSource::Public.label(), "PUBLIC");
        assert_eq!(DataSource::Live.label(), "LIVE");
        assert_eq!(DataSource::Stale.label(), "STALE");
        assert!(DataSource::Placeholder.is_suspect());
        assert!(DataSource::Stale.is_suspect());
        assert!(!DataSource::Live.is_suspect());
        assert!(!DataSource::Public.is_suspect());
    }

    /// Signing out has to leave a *guest*, not a pilot with somebody else's
    /// numbers still on the page.
    #[test]
    fn signing_out_empties_the_record_but_keeps_the_name() {
        let mut account = Account {
            token: Some("t".to_owned()),
            username: Some("Maverick".to_owned()),
            ..Account::default()
        };
        let mut data = LobbyData::placeholder();
        data.pilot = record_from(&login_body(), "Maverick");
        data.source = DataSource::Live;
        // No `store::clear` side effect to worry about: with no
        // SPACESHIPS_STATE_DIR and no file, removing it is a no-op.
        sign_out(&mut account, &mut data, "SIGNED OUT");
        assert!(!account.signed_in());
        assert_eq!(account.username, None);
        assert_eq!(
            data.pilot.callsign, "Maverick",
            "the aircraft keeps its name"
        );
        assert_eq!(data.pilot.credits, 0);
        assert_eq!(data.pilot.kills, 0);
        assert_eq!(data.pilot.owned, 1);
        assert!(data.standings.iter().all(|s| !s.you));
        assert_eq!(
            data.source,
            DataSource::Public,
            "the standings are still real"
        );
    }

    /// A failed read must not throw away numbers that were true a minute ago —
    /// but it must stop claiming they are current.
    #[test]
    fn a_failed_refresh_goes_stale_rather_than_blank() {
        let mut data = LobbyData::placeholder();
        data.source = DataSource::Live;
        data.pilot.credits = 4_000;
        mark_stale(&mut data);
        assert_eq!(data.source, DataSource::Stale);
        assert_eq!(data.pilot.credits, 4_000);

        // Nothing real was ever shown, so there is nothing to go stale.
        let mut fresh = LobbyData::placeholder();
        mark_stale(&mut fresh);
        assert_eq!(fresh.source, DataSource::Placeholder);
    }

    #[test]
    fn transport_errors_read_as_english() {
        assert_eq!(
            offline_notice("could not reach 127.0.0.1:4100: Connection refused (os error 61)"),
            "SERVER NOT ANSWERING"
        );
        assert_eq!(
            offline_notice("TypeError: Failed to fetch"),
            "SERVER NOT ANSWERING"
        );
        assert_eq!(
            offline_notice("could not read: timed out"),
            "SERVER TIMED OUT"
        );
        assert_eq!(
            offline_notice("could not resolve gheat.net: nodename nor servname provided"),
            "SERVER NOT FOUND"
        );
        assert_eq!(
            offline_notice("something else entirely"),
            "DATA LINK FAILED"
        );
    }

    /// A 401 anywhere that needed a token means the seven days ran out. A 401
    /// from a public endpoint does not, because it cannot.
    #[test]
    fn only_authenticated_jobs_treat_a_401_as_an_expiry() {
        assert!(Job::Credits.is_authenticated());
        assert!(Job::Unlocks.is_authenticated());
        assert!(Job::Unlock.is_authenticated());
        assert!(Job::Report("solo-result").is_authenticated());
        assert!(Job::SignIn.is_authenticated());
        assert!(!Job::Profile.is_authenticated());
        assert!(!Job::Leaderboard.is_authenticated());
    }

    #[test]
    fn the_placeholder_is_honest_about_being_one() {
        let data = LobbyData::placeholder();
        assert_eq!(data.source, DataSource::Placeholder);
        assert!(data.source.is_suspect());
        assert_eq!(data.standings.len(), 8);
    }

    /// The guest record is zero everywhere, which is the truth: nothing a guest
    /// does is written down.
    #[test]
    fn a_guest_has_nothing_but_a_name() {
        let g = PilotRecord::guest("ROOKIE".to_owned());
        assert_eq!(g.callsign, "ROOKIE");
        assert_eq!(g.credits, 0);
        assert_eq!(g.owned, 1);
        assert_eq!(g.trial_best, [None; 4]);
        assert_eq!(g.enlisted, "-");
    }

    // -- the credential store ----------------------------------------------

    /// Round-trips through a real file, in a directory this test owns.
    ///
    /// Deliberately through `load_from`/`save_to` rather than through `load`
    /// and `save`: those resolve the path from the environment, and
    /// `std::env::set_var` is process-global while cargo runs tests on threads.
    /// A test that moved `SPACESHIPS_STATE_DIR` raced every other test that
    /// reads an environment variable, which is why it is split.
    #[cfg(all(unix, not(target_arch = "wasm32")))]
    #[test]
    fn a_credential_round_trips_and_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "spaceships-cred-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("credentials.json");

        assert_eq!(store::load_from(&path), None, "nothing saved yet");
        store::save_to(&path, "head.body.sig", "Maverick").unwrap();
        assert_eq!(
            store::load_from(&path),
            Some(("head.body.sig".to_owned(), Some("Maverick".to_owned())))
        );

        // The whole point of putting it in a file rather than beside the
        // binary: nobody else on the machine can read the bearer token.
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the token is a bearer credential");
        let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);

        store::clear_at(&path);
        assert_eq!(store::load_from(&path), None);
        // Clearing twice is what a guest pressing sign out does, and it is not
        // an error.
        store::clear_at(&path);

        // Garbage in the file is a guest, not a panic — a half-written
        // credential must not stop the client starting.
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(store::load_from(&path), None);
        std::fs::write(&path, r#"{"username":"x"}"#).unwrap();
        assert_eq!(store::load_from(&path), None, "a record with no token");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Where the credential goes, and the two things it must not be: beside the
    /// executable, or in the repository.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_credential_lives_under_the_users_own_config_directory() {
        let Some(path) = store::path() else {
            return; // No HOME: a CI container, and there is nowhere to put it.
        };
        assert_eq!(path.file_name().unwrap(), "credentials.json");
        let shown = path.display().to_string();
        assert!(
            shown.contains("Spaceships") || shown.contains("spaceships"),
            "{shown}"
        );
        assert!(
            !shown.contains("spaceships-rs/crates"),
            "the token must not land in the checkout: {shown}"
        );
    }

    // -- against a real server ----------------------------------------------

    /// The whole REST half, end to end, against a running server.
    ///
    /// **Skipped unless `SPACESHIPS_TEST_SERVER` names one**, so `cargo test`
    /// on a machine with no server is silent rather than red — and so this
    /// never becomes a test that needs the network to pass.
    ///
    /// ```text
    /// SPACESHIPS_TEST_SERVER=http://127.0.0.1:4100 cargo test -p spaceships-client
    /// ```
    ///
    /// It exists because the hand-rolled HTTP/1.1 client in this file is the
    /// part that unit tests cannot really prove: `parse_response` against bytes
    /// I wrote myself will always agree with `wire_request` that I also wrote.
    /// What matters is whether **Express** accepts the request and whether its
    /// reply parses — the headers, the framing, the `Connection: close`, the
    /// JSON shape — and the only way to know that is to ask it.
    ///
    /// It registers a fresh account each run rather than using anyone's. Point
    /// it at a scratch database; `pilots.db` holds real pilots.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_rest_half_works_against_a_real_server() {
        let Ok(base) = std::env::var("SPACESHIPS_TEST_SERVER") else {
            return;
        };
        let base = base.trim_end_matches('/');

        /// Blocks until the request lands. Only a test may do this; nothing in
        /// the client ever waits on a reply.
        fn wait(request: Request) -> Response {
            let mut fetch = request.send().expect("the request should start");
            for _ in 0..2_000 {
                if let Some(result) = fetch.poll() {
                    return result.expect("the request should complete");
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            panic!("the request never came back");
        }
        fn body(response: &Response) -> Value {
            serde_json::from_str(&response.body).expect("the reply should be JSON")
        }

        // A name this run owns. `[A-Za-z0-9]{3,20}` is the server's rule.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let name = format!("rstest{}", now % 100_000_000);
        let password = "correcthorse";

        // -- enlist ---------------------------------------------------------
        let registered = wait(Request::post(
            &format!("{base}/api/register"),
            None,
            credentials(&name, password),
        ));
        assert_eq!(registered.status, 201, "{}", registered.body);

        // The same callsign twice is the error a pilot will actually hit.
        let again = wait(Request::post(
            &format!("{base}/api/register"),
            None,
            credentials(&name, password),
        ));
        assert_eq!(again.status, 409);
        assert_eq!(body(&again)["error"], "Callsign already taken");

        // -- sign in --------------------------------------------------------
        let refused = wait(Request::post(
            &format!("{base}/api/login"),
            None,
            credentials(&name, "wrongpassword"),
        ));
        assert_eq!(refused.status, 401);

        let signed_in = wait(Request::post(
            &format!("{base}/api/login"),
            None,
            credentials(&name, password),
        ));
        assert!(signed_in.ok(), "{}", signed_in.body);
        let payload = body(&signed_in);
        let token = payload["token"].as_str().expect("a token").to_owned();
        assert!(token.split('.').count() == 3, "a JWT has three parts");

        // The login reply alone is a complete dossier.
        let pilot = record_from(&payload, &name);
        assert_eq!(pilot.callsign, name);
        assert_eq!(pilot.rank, "Cadet", "a fresh pilot");
        assert_eq!(pilot.kills, 0);
        assert_eq!(pilot.owned, 1, "nothing unlocked yet");

        // -- the authenticated reads ----------------------------------------
        let credits = wait(Request::get(
            &format!("{base}/api/credits"),
            Some(token.clone()),
        ));
        assert!(credits.ok(), "{}", credits.body);
        let opening_balance = number(body(&credits).get("credits")).unwrap();

        let unlocks = wait(Request::get(
            &format!("{base}/api/unlocks"),
            Some(token.clone()),
        ));
        assert!(unlocks.ok());
        assert_eq!(owned_bits(&body(&unlocks)), 1);

        // **The expired-token path**, which is the one failure mode that is
        // otherwise only reachable by waiting seven days.
        let rejected = wait(Request::get(
            &format!("{base}/api/credits"),
            Some("not.a.token".to_owned()),
        ));
        assert_eq!(rejected.status, 401, "a bad token must 401, not 500");

        // -- the public reads -----------------------------------------------
        let profile = wait(Request::get(
            &format!("{base}/api/profile/{}", percent_encode(&name)),
            None,
        ));
        assert!(profile.ok(), "{}", profile.body);
        assert_eq!(
            record_from(&body(&profile)["profile"], &name).callsign,
            name
        );

        let board = wait(Request::get(&format!("{base}/api/leaderboard"), None));
        assert!(board.ok());
        let rows = body(&board);
        let rows = rows["leaderboard"].as_array().expect("an array");
        assert!(
            rows.iter().map(standing_from).any(|s| s.callsign == name),
            "the new pilot should be on the ladder"
        );

        // -- and a match that records something ------------------------------
        let reported = wait(Request::post(
            &format!("{base}/api/solo-result"),
            Some(token.clone()),
            r#"{"kills":3,"deaths":1,"botsKilled":3,"won":true}"#.to_owned(),
        ));
        assert!(reported.ok(), "{}", reported.body);
        let result = body(&reported);
        let earned = number(result.get("creditsEarned")).unwrap();
        assert!(earned > 0.0, "a won match is worth something");
        assert!(number(result.get("totalCredits")).unwrap() > opening_balance);

        // The point of all of it: it is on the *pilot's* record afterwards.
        let after = wait(Request::get(
            &format!("{base}/api/profile/{}", percent_encode(&name)),
            None,
        ));
        let after = record_from(&body(&after)["profile"], &name);
        assert_eq!(after.kills, 3, "the kills did not persist");
        assert_eq!(after.deaths, 1);
        assert_eq!(after.matches_won, 1);
        assert_eq!(after.bots_killed, 3);

        // A guest reporting the same match is refused, which is what makes the
        // token load-bearing rather than decorative.
        let anonymous = wait(Request::post(
            &format!("{base}/api/solo-result"),
            None,
            r#"{"kills":99,"deaths":0,"botsKilled":0,"won":true}"#.to_owned(),
        ));
        assert_eq!(anonymous.status, 401);
    }

    // -- results ------------------------------------------------------------

    #[test]
    fn the_solo_result_is_tri_state_about_winning() {
        use sim::world::{Frame, ShipFlags, ShipView};
        let mut frame = Frame::new();
        // No local ship: a match nobody here was in is a draw, not a loss.
        assert_eq!(outcome(Some(Team::Zero), &frame), None);

        frame.ships.push(ShipView {
            id: LOCAL_ID,
            team: 0,
            flags: ShipFlags::LOCAL,
            ..ShipView::default()
        });
        assert_eq!(outcome(Some(Team::Zero), &frame), Some(true));
        assert_eq!(outcome(Some(Team::One), &frame), Some(false));
        assert_eq!(outcome(None, &frame), None, "a draw is null");

        // An unteamed pilot never lost to a side they were not on.
        frame.ships[0].team = -1;
        assert_eq!(outcome(Some(Team::Zero), &frame), None);
    }
}
