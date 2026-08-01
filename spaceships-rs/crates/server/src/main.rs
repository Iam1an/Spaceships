//! Placeholder binary for the Rust port of the Spaceships game server.
//!
//! Nothing is served yet. This crate exists so the workspace has the right
//! shape, and so the wire contract in `spaceships-protocol` has a consumer that
//! keeps it honest at compile time.
//!
//! # What this will eventually replace
//!
//! All of it currently lives in `server/index.js` (1004 lines) and
//! `server/db.js`.
//!
//! ## 1. The Express HTTP layer (`server/index.js:17-190`)
//!
//! A JSON API plus static file serving. The static handler sets
//! `Cache-Control: no-store` and disables `etag`/`lastModified` so dev reloads
//! always fetch fresh assets — worth preserving.
//!
//! | Method | Route                     | Purpose                                     |
//! |--------|---------------------------|---------------------------------------------|
//! | POST   | `/api/register`           | Create a pilot                              |
//! | POST   | `/api/login`              | Issue a JWT                                 |
//! | PUT    | `/api/colors`             | Persist ship/accent colors (bearer auth)    |
//! | GET    | `/api/profile/:username`  | Public pilot profile                        |
//! | GET    | `/api/leaderboard`        | Global leaderboard                          |
//! | GET    | `/api/unlocks`            | Owned customization unlocks + cost table    |
//! | POST   | `/api/unlock/:feature`    | Buy an unlock (402 on insufficient credits) |
//! | GET    | `/api/credits`            | Balance                                     |
//! | GET    | `/api/credits/history`    | Transaction log (`limit`, clamped 1..100)   |
//! | POST   | `/api/credits/spend`      | Debit credits                               |
//! | POST   | `/api/solo-result`        | Record a solo match                         |
//! | POST   | `/api/trial-result`       | Record a time trial (trials 1-4)            |
//! | POST   | `/api/campaign-result`    | Record a campaign mission (1-3)             |
//! |        | `GET /*`                  | Static files from `public/`                 |
//!
//! ## 2. The hand-rolled WebSocket implementation (`server/index.js:198-398`)
//!
//! A minimal RFC 6455 server written from scratch because the `ws` library
//! mis-parses text frames as compressed under Node 25. It implements the
//! handshake (SHA-1 of `key + WS_GUID`, base64), unfragmented text/binary/close/
//! ping frames, a 1 MiB payload cap, and `TCP_NODELAY` so 20 Hz state updates
//! are not coalesced by Nagle.
//!
//! It also carries a workaround that a Rust port should **not** reimplement: a
//! two-tier rotating `Set` that deduplicates frames by their 4-byte mask, added
//! because a Node 25 upgrade regression re-delivers frame bytes (sometimes with
//! the original HTTP request prepended). That is a Node bug, not a protocol
//! requirement. Deleting the dedup cache and the `parse()` HTTP-prefix skip is
//! one of the concrete wins of moving off Node.
//!
//! Connections upgrade at `/ws`, optionally with `?token=<jwt>`. An invalid or
//! expired token must degrade to guest, not reject the socket.
//!
//! ## 3. Room and lobby state (`server/index.js:401-621`, `623-997`)
//!
//! In-process, non-persistent: a `Map<code, Room>` plus a monotonic connection
//! counter. Per room: a 4-letter code, host id, privacy flag, map, the socket
//! set, the player map, started flag, generated asteroid field, team kill
//! totals, match deadline, and two timers (a 5-minute end timer and a 1 Hz tick).
//!
//! Behaviours worth porting deliberately rather than by accident:
//!
//! - Host migration on leave, but only to a **human** — a room of only bots is
//!   torn down, because nobody would be left to drive them.
//! - When the bot host leaves, their bots are evicted with them.
//! - Team balancing spawns one hard bot on the smaller team, driven by the host
//!   client and relayed through `bot-state` / `bot-fire`.
//! - Hit resolution is client-authoritative and only sanity-checked: rate limits
//!   (40 ms guns, 400 ms missiles, keyed per shooter and weapon), spawn
//!   protection, no friendly fire, no self-damage without a `fromBotId`, and an
//!   exception letting an in-flight missile still land after its shooter dies.
//! - Asteroid HP is server-side; clients report hits and receive `asteroid-hp` /
//!   `asteroid-destroyed`.
//!
//! ## 4. SQLite persistence (`server/db.js`, `pilots.db`)
//!
//! Pilots and password hashes, JWT issuing and verification, per-pilot stats,
//! a large `ACHIEVEMENT_DEFS` table with rewards, a credits ledger with a
//! transaction log, customization unlocks with a cost table, the leaderboard
//! query, and startup backfill migrations that award and credit achievements
//! earned before those systems existed.
//!
//! # What this will *not* replace
//!
//! Rendering. Three.js in `public/` stays exactly where it is. See
//! `spaceships-sim` for the boundary: simulation produces numbers, `public/`
//! draws them.

use spaceships_protocol::{ClientMessage, ServerMessage};

fn main() {
    println!("spaceships-server: placeholder — no listener yet.");
    println!();
    println!("The JS server at server/index.js is still the one serving the game.");
    println!("This binary exists so the workspace compiles and so the wire contract");
    println!("in spaceships-protocol has a consumer.");
    println!();
    println!("Still to build:");
    println!("  - HTTP API + static files      (replaces Express, server/index.js:17-190)");
    println!("  - WebSocket handshake + frames (replaces server/index.js:198-398)");
    println!("  - Room / lobby state machine   (replaces server/index.js:401-997)");
    println!("  - Pilot + credits persistence  (replaces server/db.js, pilots.db)");
    println!();
    println!("Wire contract loaded:");
    println!("  client -> server: {}", CLIENT_TAGS.len());
    println!("  server -> client: {}", SERVER_TAGS.len());
}

/// Every `type` tag the browser can send, for reference while porting
/// `handleConnection`.
const CLIENT_TAGS: &[&str] = &[
    "name",
    "list-rooms",
    "create",
    "join",
    "start",
    "leave",
    "state",
    "fire",
    "flare",
    "hit",
    "self-damage",
    "colors",
    "ship-model",
    "asteroid-hit",
    "bot-state",
    "bot-fire",
];

/// Every `type` tag the server can send.
const SERVER_TAGS: &[&str] = &[
    "room",
    "players",
    "rooms-list",
    "start",
    "state",
    "fire",
    "flare",
    "colors",
    "ship-model",
    "hp",
    "death",
    "respawn",
    "disconnect",
    "asteroid-hp",
    "asteroid-destroyed",
    "match-state",
    "match-end",
    "match-credits",
    "error",
];

/// Keeps the protocol types linked into this binary so a breaking change to the
/// wire contract fails this crate's build too, not just the protocol tests.
#[allow(dead_code)]
fn _protocol_is_wired_up(msg: ClientMessage) -> Option<ServerMessage> {
    match msg {
        ClientMessage::Join { code } => Some(ServerMessage::Room {
            code,
            host: false,
            you: 0,
            private: false,
        }),
        _ => None,
    }
}
