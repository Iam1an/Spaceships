//! The WebSocket transport, on native and in the browser.
//!
//! `crates/protocol` already says *what* goes on the wire — 35 serde types that
//! are byte-compatible with the shipped JS. This module is only about *how* the
//! bytes move, and its whole job is to make that question disappear for the rest
//! of the client.
//!
//! # One interface, two backends
//!
//! Everything above [`Socket`] is platform-independent: the same state machine,
//! the same reconnect policy, the same 20 Hz cadence, the same JSON codec. The
//! platform split is one small type with four methods:
//!
//! ```text
//!            connect(url) -> Socket
//!            send(&str) -> bool          drain(&mut Vec<SocketEvent>)
//!                    │                              ▲
//!  ┌─────────────────┴──────────────────────────────┴─────────────────┐
//!  │                                                                  │
//!  │  native                              wasm32                      │
//!  │  ──────                              ──────                      │
//!  │  one OS thread                       four `Closure`s hung off    │
//!  │  + current-thread tokio runtime      `window.WebSocket`          │
//!  │  + tokio-tungstenite                                             │
//!  │                                                                  │
//!  │  events cross via tokio mpsc         events cross via            │
//!  │  (thread -> main thread)             Rc<RefCell<VecDeque>>       │
//!  └──────────────────────────────────────────────────────────────────┘
//! ```
//!
//! [`SocketEvent`] is the contract: `Open`, `Text`, and exactly one terminal
//! event (`Closed` or `Failed`). Normalising the two very different failure
//! shapes into that one sequence is most of the value — the browser fires
//! `error` *then* `close` for a connection that never opened, and tungstenite
//! returns a single `Err` from the handshake, and neither of those leaks past
//! [`Socket::drain`].
//!
//! `Socket` is a concrete `cfg`-selected type, not a `dyn` trait. Only one
//! backend is ever compiled, so a trait object would buy nothing but a vtable
//! and would force `Box`ing on the receive path.
//!
//! # What the rest of the client sees
//!
//! - [`NetStatus`] — a plain `Resource`. Connection state, the assigned player
//!   id, counters, last error. This is what `hud.rs` reads; it never touches the
//!   socket. [`NetStatus::label`] is a ready-to-draw string.
//! - [`FromServer`] / [`ToServer`] — Bevy messages. Receiving is
//!   `MessageReader<FromServer>`, sending is `MessageWriter<ToServer>`. No
//!   locking, no `ResMut` contention, and gameplay code stays testable without
//!   a socket.
//! - [`NetCommand`] — connect, disconnect, reconnect. Lifecycle only.
//! - [`NetConfig`] — where to connect and whether to retry.
//!
//! The socket handle itself ([`NetLink`]) is a **non-send** resource and is
//! private. That is deliberate: the wasm backend is `Rc`/`RefCell` all the way
//! down and can never be `Send`, and rather than paper over that with an
//! `unsafe impl Send` on one platform, both platforms use `NonSendMut` and the
//! pump systems run on the main thread. They cost microseconds. The consequence
//! that matters is the good one — nothing outside this module *can* reach the
//! socket, so the public surface stays the four items above.
//!
//! # Queues, and not blocking the render loop
//!
//! Nothing in a Bevy system ever waits on the network.
//!
//! - Outgoing: `MessageWriter<ToServer>` -> [`flush_outbox`] in `Last` ->
//!   serialize -> unbounded channel (native) or `WebSocket.send` (wasm). The
//!   native channel push is a lock-free enqueue; the actual `write(2)` happens
//!   on the socket thread.
//! - Incoming: the socket thread/callback enqueues, [`service`] in `PreUpdate`
//!   drains whatever has arrived and republishes it as [`FromServer`]. A frame
//!   that arrives mid-frame is simply seen next frame.
//!
//! Sends while the socket is not open are **dropped**, exactly as the JS does
//! (`if (ws && ws.readyState === WebSocket.OPEN)` guards every one of the ~20
//! `ws.send` call sites in `public/src/main.js`). Unlike the JS, the drops are
//! counted — see [`NetStatus::dropped`] — so "my hits aren't registering" has an
//! answer that is not "add a console.log".
//!
//! # Cadence
//!
//! `main.js` sends its own `state` at `STATE_INTERVAL = 1 / 20` while the
//! simulation runs at [`sim::world::TICK_HZ`]. [`broadcast_local_state`] keeps
//! that: it is a frame-rate system with an accumulator, not a `FixedUpdate`
//! system, because the JS cadence is measured in wall-clock frames and driving
//! it off the sim tick would change the rate the moment the tick rate does.
//!
//! # Trap: Bevy 0.19 cancels a dropped `Task`
//!
//! `bevy_tasks::Task` cancels on drop ("Dropping the task will attempt to
//! cancel it"), which on wasm — where the pool is
//! `wasm_bindgen_futures::spawn_local` behind a `LocalExecutor` — means a socket
//! future that is spawned and not `.detach()`ed dies silently and instantly.
//! Neither backend here is exposed to it: native owns a real OS thread, and
//! wasm uses browser callbacks rather than a future. The wasm backend has the
//! *analogous* hazard, and it is handled in [`Socket::drop`]: the `Closure`s
//! must be unhooked from the `WebSocket` before they are freed, or the browser
//! keeps calling into deallocated wasm.

use bevy::prelude::*;
use spaceships_protocol::{
    Asteroid as WireAsteroid, AsteroidTier as WireTier, BotAssignment, ClientMessage, PlayerId,
    PlayerInfo, RoomSummary, ServerMessage, Shot, Spawn, WeaponKind as WireWeapon,
};
use std::collections::BTreeMap;

use crate::sim_bridge::{MatchSetup, Roster, SimFrame, SimWorld, LOCAL_ID};
use sim::math::{Quat as SimQuat, Vec3 as SimVec3};
use sim::rules::Rules;
use sim::world::{
    Asteroid, AsteroidTier, EntityId, Frame, MapKind, Mode, NetEvent, NetIntent, Score, Ship,
    ShipKind, Team, WeaponKind, World as SimWorldState,
};
use spaceships_sim as sim;

// ─────────────────────────────────────────────────────────────────────────────
// Tuning
// ─────────────────────────────────────────────────────────────────────────────

/// Reconnect backoff, indexed by consecutive-failure count and saturating at
/// the last entry.
///
/// The JS has no reconnect at all — `lobby/net.js` just sets "Disconnected from
/// server" and stops — so there is no existing behaviour to match here.
const RETRY_BACKOFF: [f32; 5] = [0.5, 1.0, 2.0, 4.0, 8.0];

/// How long a clean shutdown waits for the close handshake to leave the
/// machine. Only ever paid on `AppExit`, never mid-frame.
#[cfg(not(target_arch = "wasm32"))]
const CLOSE_GRACE: std::time::Duration = std::time::Duration::from_millis(250);

/// Where a native build connects when `SPACESHIPS_SERVER` is unset: the live
/// game.
///
/// # Why the shipped default is production and not localhost
///
/// The binary is handed to someone as a `.dmg`. They will not set an
/// environment variable, they do not have a Node server, and a client whose
/// out-of-the-box multiplayer button fails to connect is indistinguishable from
/// a broken one. So the compiled-in answer is the server that is actually up,
/// and a developer opts *out* with [`ENDPOINT_ENV`].
///
/// `ws://`, not `wss://`: `deploy/README.md`'s Caddy site block is `gheat.net:80`
/// with no TLS listener at all, so there is nothing to negotiate. That is also
/// what keeps the native transport free of a TLS stack — see [`Socket::connect`],
/// which refuses `wss://` with the exact Cargo feature that would fix it.
///
/// The lobby can still move it at runtime without a restart: `Screen::Net`'s
/// `SERVER` row walks [`ENDPOINTS`], which is the no-environment-variable
/// escape hatch for a friend on a LAN party as well as for a developer.
#[cfg(not(target_arch = "wasm32"))]
const DEFAULT_URL: &str = "ws://gheat.net/ws";

/// The endpoints the lobby's `SERVER` row cycles through, in order, starting at
/// whatever [`default_endpoint`] chose.
///
/// Deliberately two: the live game and a server on this machine. Anything else
/// is what [`ENDPOINT_ENV`] is for.
#[cfg(not(target_arch = "wasm32"))]
pub const ENDPOINTS: [&str; 2] = [DEFAULT_URL, "ws://127.0.0.1:4000/ws"];

/// The browser derives its endpoint from the page it was served by, so there is
/// nothing to cycle.
#[cfg(target_arch = "wasm32")]
pub const ENDPOINTS: [&str; 0] = [];

/// Overrides [`DEFAULT_URL`]. Named once so the message that mentions it and
/// the read that honours it cannot drift.
#[cfg(not(target_arch = "wasm32"))]
const ENDPOINT_ENV: &str = "SPACESHIPS_SERVER";

/// Seed for a networked world.
///
/// The asteroid field arrives on the wire, so the only thing the RNG still
/// drives is bot scatter and missile delays for balance bots this client
/// happens to host. The JS server sends no seed
/// ([`ServerMessage::Start::seed`] is `None` from it), and a fixed value keeps
/// two Rust clients in the same match agreeing about the parts of the world
/// nobody transmits.
const ONLINE_SEED: u64 = 0x5EED_0B0E;

/// RFC 6455 §7.4.1 "normal closure".
const CLOSE_NORMAL: u16 = 1000;
/// RFC 6455 §7.4.1 "abnormal closure" — the peer vanished without a close
/// frame.
///
/// Native only. The browser hands this code to `onclose` itself (it is what a
/// connection that never opened reports), so the wasm backend reads it off the
/// event rather than synthesising it, and a `const` it never names is a
/// warning.
#[cfg(not(target_arch = "wasm32"))]
const CLOSE_ABNORMAL: u16 = 1006;

// ─────────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────────

/// Registers the transport: one status resource, one config, three message
/// types, and the four systems that move bytes.
pub struct NetPlugin;

impl Plugin for NetPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NetStatus>()
            .init_resource::<NetConfig>()
            .init_resource::<NetSession>()
            .init_resource::<NetInbox>()
            .add_message::<FromServer>()
            .add_message::<ToServer>()
            .add_message::<NetCommand>()
            // Non-send: see the module docs. `NetLink` owns the `Socket`, which
            // on wasm is `Rc`-based and cannot be `Send` at any price.
            // (`init_non_send`, not `init_non_send_resource` — 0.19 deprecated
            // the longer name.)
            .init_non_send::<NetLink>()
            .add_systems(Startup, auto_connect)
            // Receive first, so a system in `Update` reads this frame's traffic
            // rather than last frame's. `ingest` is chained onto the end so it
            // sees the frames `service` published this frame, not last frame's.
            .add_systems(
                PreUpdate,
                (run_commands, service, ingest)
                    .chain()
                    .in_set(NetSet::Receive),
            )
            // Once per *tick*, not once per rendered frame: `Frame::net_out` is
            // produced by the fixed step and would otherwise be sent twice on a
            // frame that ran no step at all. `after(SimSet)` is what makes the
            // frame this reads the one the step just wrote.
            .add_systems(
                FixedUpdate,
                publish_intents.after(crate::sim_bridge::SimSet),
            )
            // `Last`, so anything written up to and including `PostUpdate`
            // leaves on the same frame it was produced. Bevy double-buffers
            // messages, so a writer that runs *after* this is delayed by one
            // frame and never dropped.
            .add_systems(
                Last,
                (flush_outbox, close_on_exit).chain().in_set(NetSet::Send),
            );
    }
}

/// Ordering handles, so a later module can say `.after(NetSet::Receive)`
/// without knowing which system does the work.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NetSet {
    /// `PreUpdate`: drain the socket, publish [`FromServer`].
    Receive,
    /// `Last`: serialize [`ToServer`] and hand it to the socket.
    Send,
}

/// A decoded frame from the server. Read it with `MessageReader<FromServer>`.
///
/// `dead_code` is allowed on the payload because this module is the *producer*:
/// nothing consumes a `ServerMessage` until the lobby and match code land, and
/// a transport that decoded frames and then discarded them so the compiler
/// stayed quiet would be worse than a warning.
#[derive(Message, Debug, Clone)]
#[allow(dead_code)]
pub struct FromServer(pub ServerMessage);

/// A frame to send. Write it with `MessageWriter<ToServer>`.
///
/// Dropped if the socket is not open, matching the JS `readyState` guard.
#[derive(Message, Debug, Clone)]
pub struct ToServer(pub ClientMessage);

/// Lifecycle control. Everything else about the connection is [`NetConfig`].
///
/// Only `Connect` has a caller so far ([`auto_connect`]); the other two are the
/// half of the lifecycle a lobby drives, and are implemented and tested here so
/// that wiring a "Leave" button later is one `write` call.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum NetCommand {
    /// Open a socket using the current [`NetConfig`]. Ignored if one is already
    /// open or opening.
    Connect,
    /// Close cleanly and stop retrying. Sending this is how you opt out of
    /// [`NetConfig::reconnect`].
    Disconnect,
    /// Drop the current socket and open a new one immediately, resetting the
    /// backoff. For "the server restarted, stop waiting 8 seconds".
    Reconnect,
}

/// Where to connect, and what to do when the connection dies.
///
/// A plain `Resource`, so a future lobby can rewrite `url`/`token` after a login
/// and then send [`NetCommand::Reconnect`].
#[derive(Resource, Debug, Clone)]
pub struct NetConfig {
    /// Socket endpoint *without* a query string, e.g. `ws://127.0.0.1:4000/ws`.
    pub url: String,
    /// JWT for an authenticated pilot. `None` is a guest, which the server
    /// explicitly supports — `ws.rs` treats an absent, malformed, or expired
    /// token as guest rather than refusing the upgrade.
    pub token: Option<String>,
    /// The callsign announced with `name` the moment the socket opens.
    ///
    /// It lives here rather than in the lobby because *when* it is sent is a
    /// transport fact: `lobby/net.js` sends it from its `onopen` handler, and
    /// anything written before then is dropped by [`flush_outbox`]'s
    /// `readyState` guard. The lobby's job is only to keep this field equal to
    /// the pilot's name.
    pub callsign: String,
    /// Connect at startup without waiting for [`NetCommand::Connect`].
    pub auto_connect: bool,
    /// Retry with [`RETRY_BACKOFF`] after an unrequested close.
    pub reconnect: bool,
}

impl Default for NetConfig {
    fn default() -> Self {
        let (url, token) = default_endpoint();
        Self {
            // The lobby connects when the player opens the network page. A
            // client that dialled at startup would spend its first eight
            // seconds backing off against a server the player never asked for,
            // and would hold a socket open through an entire solo campaign.
            auto_connect: false,
            url,
            token,
            // Overwritten by the lobby from the pilot record before the first
            // connection; the server sanitizes and truncates whatever arrives.
            callsign: "PILOT".to_owned(),
            reconnect: true,
        }
    }
}

impl NetConfig {
    /// The full URL including `?token=`, which is how the server reads it
    /// (`ws.rs` `WsQuery`) and how `lobby/net.js` writes it.
    pub fn socket_url(&self) -> String {
        socket_url(&self.url, self.token.as_deref())
    }

    /// A short label for the endpoint, for a lobby row that has one line to say
    /// it in. `ws://gheat.net/ws` reads as `gheat.net`.
    #[must_use]
    pub fn endpoint_label(&self) -> &str {
        let rest = self
            .url
            .strip_prefix("ws://")
            .or_else(|| self.url.strip_prefix("wss://"))
            .unwrap_or(&self.url);
        rest.split('/').next().unwrap_or(rest)
    }

    /// Where [`ENDPOINTS`] currently sits, so the lobby's `SERVER` row can step
    /// on from it. `None` when the URL is not one of them — an explicit
    /// `SPACESHIPS_SERVER`, or the browser's own origin.
    #[must_use]
    pub fn endpoint_index(&self) -> Option<usize> {
        ENDPOINTS.iter().position(|e| *e == self.url)
    }

    /// Steps to the next entry of [`ENDPOINTS`], starting from whichever one is
    /// selected — or from the first, when the current URL is not in the list.
    ///
    /// Returns `false` when there is nothing to cycle, which is the web build:
    /// there the endpoint is the page's own origin and offering to change it
    /// would only ever break the connection.
    pub fn cycle_endpoint(&mut self) -> bool {
        if ENDPOINTS.is_empty() {
            return false;
        }
        let next = self
            .endpoint_index()
            .map_or(0, |i| (i + 1) % ENDPOINTS.len());
        self.url = ENDPOINTS[next].to_owned();
        true
    }
}

/// Where the connection is. Everything a HUD needs, and nothing that borrows
/// the socket.
#[derive(Resource, Debug, Clone, Default)]
pub struct NetStatus {
    /// Coarse lifecycle state.
    pub state: ConnState,
    /// Player id the server assigned, from `ServerMessage::Room { you }`.
    pub you: Option<PlayerId>,
    /// Consecutive failed attempts; `0` once connected.
    pub attempts: u32,
    /// Seconds until the next retry, while [`ConnState::Retrying`].
    pub retry_in: f32,
    /// Why the last connection ended. Kept across a retry so a HUD can show it.
    pub last_error: Option<String>,
    /// Frames sent since process start.
    pub sent: u64,
    /// Frames received and decoded since process start.
    pub received: u64,
    /// Frames dropped because the socket was not open.
    pub dropped: u64,
    /// Frames received that `protocol` could not decode. Non-zero means the
    /// server is speaking something this client does not model — a `protocol`
    /// gap, not a transport bug.
    pub undecodable: u64,
}

impl NetStatus {
    /// Whether a `ToServer` written right now would actually go out.
    pub fn is_online(&self) -> bool {
        self.state == ConnState::Online
    }

    /// One word for the HUD. Unused until `hud.rs` draws it — that module is
    /// being written in parallel and this is the string it is meant to reach
    /// for, so it lives here rather than being invented twice.
    #[allow(dead_code)]
    pub fn label(&self) -> &'static str {
        match self.state {
            ConnState::Offline => "OFFLINE",
            ConnState::Connecting => "CONNECTING",
            ConnState::Online => "ONLINE",
            ConnState::Retrying => "RECONNECTING",
            ConnState::Failed => "FAILED",
        }
    }
}

/// Coarse connection lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnState {
    /// No socket, and none wanted.
    #[default]
    Offline,
    /// Socket created, handshake not finished.
    Connecting,
    /// Open. `ToServer` goes out.
    Online,
    /// Waiting out [`NetStatus::retry_in`] before trying again.
    Retrying,
    /// Gave up: [`NetConfig::reconnect`] is off and the connection died.
    Failed,
}

// ─────────────────────────────────────────────────────────────────────────────
// The socket handle (private, non-send)
// ─────────────────────────────────────────────────────────────────────────────

/// Owns the live socket and the reconnect timer. Private on purpose — see the
/// module docs on why this is a non-send resource.
#[derive(Default)]
struct NetLink {
    socket: Option<Socket>,
    /// Reused across frames so the receive path does not allocate.
    inbox: Vec<SocketEvent>,
    /// Set once the app has asked to be connected; cleared by
    /// [`NetCommand::Disconnect`]. This is what separates "the socket died and
    /// we should retry" from "we hung up on purpose".
    wanted: bool,
}

impl NetLink {
    /// Tears down the socket, optionally waiting for the close handshake.
    fn hang_up(&mut self, grace_ms: u64) {
        if let Some(socket) = self.socket.take() {
            socket.close(grace_ms);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Systems
// ─────────────────────────────────────────────────────────────────────────────

/// Honours [`NetConfig::auto_connect`] once, at startup.
fn auto_connect(config: Res<NetConfig>, mut cmds: MessageWriter<NetCommand>) {
    if config.auto_connect {
        cmds.write(NetCommand::Connect);
    }
}

/// Applies [`NetCommand`]s. Split from [`service`] so the two concerns —
/// "what did the app ask for" and "what did the wire do" — stay legible.
fn run_commands(
    mut cmds: MessageReader<NetCommand>,
    mut link: NonSendMut<NetLink>,
    mut status: ResMut<NetStatus>,
    config: Res<NetConfig>,
) {
    for cmd in cmds.read() {
        match cmd {
            NetCommand::Connect => {
                link.wanted = true;
                if link.socket.is_none() {
                    open(&mut link, &mut status, &config);
                }
            }
            NetCommand::Disconnect => {
                link.wanted = false;
                link.hang_up(0);
                *status = NetStatus {
                    state: ConnState::Offline,
                    // Counters are process-lifetime, not connection-lifetime.
                    sent: status.sent,
                    received: status.received,
                    dropped: status.dropped,
                    undecodable: status.undecodable,
                    ..Default::default()
                };
                info!("net: disconnected");
            }
            NetCommand::Reconnect => {
                link.wanted = true;
                link.hang_up(0);
                status.attempts = 0;
                open(&mut link, &mut status, &config);
            }
        }
    }
}

/// Opens a socket and moves the status to [`ConnState::Connecting`].
fn open(link: &mut NetLink, status: &mut NetStatus, config: &NetConfig) {
    let url = config.socket_url();
    match Socket::connect(&url) {
        Ok(socket) => {
            link.socket = Some(socket);
            status.state = ConnState::Connecting;
            status.retry_in = 0.0;
            // The token is a bearer credential; log the endpoint, never the
            // query string.
            info!("net: connecting to {}", config.url);
        }
        Err(err) => {
            // A synchronous failure (a malformed URL, a `wss://` native build,
            // no `window`) is still a failed attempt and still backs off, so
            // that a bad config does not spin.
            fail(status, err, config);
        }
    }
}

/// Records why a connection ended and decides whether to retry.
fn fail(status: &mut NetStatus, err: String, config: &NetConfig) {
    status.you = None;
    status.attempts = status.attempts.saturating_add(1);
    if config.reconnect {
        status.retry_in = backoff(status.attempts);
        status.state = ConnState::Retrying;
        warn!(
            "net: {err} — retry {} in {:.1}s",
            status.attempts, status.retry_in
        );
    } else {
        status.retry_in = 0.0;
        status.state = ConnState::Failed;
        warn!("net: {err}");
    }
    status.last_error = Some(err);
}

/// Backoff for the n-th consecutive failure (1-based), saturating at the last
/// entry of [`RETRY_BACKOFF`].
fn backoff(attempts: u32) -> f32 {
    let idx = (attempts.max(1) as usize - 1).min(RETRY_BACKOFF.len() - 1);
    RETRY_BACKOFF[idx]
}

/// Drains the socket, publishes [`FromServer`], and runs the retry timer.
///
/// This is the only system that touches the socket's receive side, and it never
/// blocks: [`Socket::drain`] moves whatever has already arrived and returns.
fn service(
    time: Res<Time>,
    mut link: NonSendMut<NetLink>,
    mut status: ResMut<NetStatus>,
    config: Res<NetConfig>,
    mut inbox: MessageWriter<FromServer>,
) {
    // ── Retry timer ─────────────────────────────────────────────────────────
    if status.state == ConnState::Retrying && link.wanted {
        status.retry_in -= time.delta_secs();
        if status.retry_in <= 0.0 {
            open(&mut link, &mut status, &config);
        }
        // `open` either connected or called `fail`, which reset the timer.
        if status.state != ConnState::Connecting {
            return;
        }
    }

    if link.socket.is_none() {
        return;
    }

    // ── Drain ───────────────────────────────────────────────────────────────
    // `inbox` is a field rather than a `Local` so it can be reused without
    // fighting the borrow of `link.socket`.
    let mut events = core::mem::take(&mut link.inbox);
    events.clear();
    if let Some(socket) = link.socket.as_mut() {
        socket.drain(&mut events);
    }

    let mut terminal = None;
    for event in events.drain(..) {
        // Everything after a terminal event belongs to a socket that no longer
        // exists. Both backends promise the terminal event comes last, but the
        // browser also fires `error` *then* `close`, so this is what collapses
        // that pair into one transition.
        if terminal.is_some() {
            continue;
        }
        match event {
            SocketEvent::Open => {
                status.state = ConnState::Online;
                status.attempts = 0;
                status.last_error = None;
                info!("net: connected");
            }
            SocketEvent::Text(text) => match serde_json::from_str::<ServerMessage>(&text) {
                Ok(msg) => {
                    status.received += 1;
                    // The one field of connection identity the server owns.
                    if let ServerMessage::Room { you, .. } = &msg {
                        status.you = Some(*you);
                    }
                    inbox.write(FromServer(msg));
                }
                Err(err) => {
                    status.undecodable += 1;
                    // Truncated: a `start` frame is kilobytes of asteroids.
                    let head: String = text.chars().take(160).collect();
                    warn!("net: undecodable frame ({err}): {head}");
                }
            },
            SocketEvent::Closed { code, reason } => {
                terminal = Some(if reason.is_empty() {
                    format!("closed ({code})")
                } else {
                    format!("closed ({code}): {reason}")
                });
            }
            SocketEvent::Failed(err) => terminal = Some(err),
        }
    }
    link.inbox = events;

    if let Some(err) = terminal {
        link.hang_up(0);
        if link.wanted {
            fail(&mut status, err, &config);
        } else {
            status.state = ConnState::Offline;
        }
    }
}

/// Serializes [`ToServer`] and hands it to the socket.
fn flush_outbox(
    mut outbox: MessageReader<ToServer>,
    // `NonSend`, not `NonSendMut`: sending only reads the handle. The queue
    // behind it is owned by the socket thread (native) or by the browser
    // (wasm), which is what makes a shared-reference `send` sound.
    link: NonSend<NetLink>,
    mut status: ResMut<NetStatus>,
    mut session: ResMut<NetSession>,
) {
    for ToServer(msg) in outbox.read() {
        // `leave` is the one message the server answers with silence — it drops
        // the socket out of `room.players` and sends nothing back — so this is
        // the only place that can know the room is gone. Applied on the way out
        // rather than optimistically at the button, so a `leave` that was
        // dropped for want of a socket does not desync the two.
        if *msg == ClientMessage::Leave && status.is_online() {
            session.reset(Phase::Idle);
        }

        let online = status.state == ConnState::Online;
        let Some(socket) = link.socket.as_ref().filter(|_| online) else {
            // The JS drops these silently. Counting them is the one place this
            // deliberately does more than the JS did.
            status.dropped += 1;
            continue;
        };
        // `ClientMessage` is a plain serde enum with no borrowed data and no
        // map keys that can fail, so this cannot realistically error; treat a
        // failure as a dropped frame rather than a panic.
        let Ok(text) = serde_json::to_string(msg) else {
            status.dropped += 1;
            error!("net: could not encode {msg:?}");
            continue;
        };
        if socket.send(&text) {
            status.sent += 1;
        } else {
            status.dropped += 1;
        }
    }
}

/// Turns the tick's [`NetIntent`]s into wire messages.
///
/// # Why the simulation owns the cadence
///
/// This used to be a frame-rate system with its own 20 Hz accumulator,
/// reproducing `main.js`'s `stateTimer`. It no longer is, because `sim::tick`
/// already emits [`NetIntent::State`] at
/// [`sim::rules::MatchRules::state_send_interval`] — derived from `World::time`
/// crossing a multiple of the interval, so it needs no state, cannot drift, and
/// is identical on every client. Keeping a second timer here would have sent
/// every pose twice.
///
/// The tick emits nothing at all under [`sim::world::Authority::Local`], so a
/// solo match costs this system one empty iteration.
fn publish_intents(
    frame: Res<SimFrame>,
    session: Res<NetSession>,
    mut outbox: MessageWriter<ToServer>,
) {
    for intent in &frame.0.net_out {
        outbox.write(ToServer(encode_intent(*intent, session.ids)));
    }
}

/// One [`NetIntent`] as the message `server/index.js` expects.
///
/// Each intent is one shot, so `shots` is always a one-element array. The JS
/// client batches its two muzzles into one `fire`; the server relays `shots`
/// verbatim and every reader iterates it, so a message per muzzle is the same
/// thing on screen and costs one extra frame per volley.
fn encode_intent(intent: NetIntent, ids: IdSwap) -> ClientMessage {
    match intent {
        NetIntent::State { pos, quat, boost } => ClientMessage::State {
            pos: wire_vec(pos),
            quat: wire_quat(quat),
            boost,
        },
        NetIntent::Fire {
            weapon,
            origin,
            dir,
            target,
        } => ClientMessage::Fire {
            kind: wire_weapon(weapon),
            shots: vec![shot(weapon, origin, dir, target, ids)],
        },
        NetIntent::Flare { pos, quat } => ClientMessage::Flare {
            pos: wire_vec(pos),
            quat: wire_quat(quat),
        },
        NetIntent::Hit {
            target,
            weapon,
            from_bot,
        } => ClientMessage::Hit {
            target_id: ids.to_wire(target),
            kind: wire_weapon(weapon),
            from_bot_id: from_bot.map(|b| ids.to_wire(b)),
        },
        NetIntent::AsteroidHit { id } => ClientMessage::AsteroidHit { id },
        NetIntent::SelfDamage { amount } => ClientMessage::SelfDamage {
            dmg: f64::from(amount),
        },
        NetIntent::BotState { id, pos, quat } => ClientMessage::BotState {
            bot_id: ids.to_wire(id),
            pos: wire_vec(pos),
            quat: wire_quat(quat),
        },
    }
}

/// One entry of a `fire` message's `shots` array.
///
/// The three weapons put three different shapes under one field name, and this
/// is the table from `spaceships_protocol::Shot`'s docs, in code: a bullet
/// carries `dir`, a beam carries its already-resolved `end`, and a missile
/// carries `dir` plus the lock. The simulation packs a beam's endpoint into the
/// intent's `dir` (see `bullets::fire_gun`), which is why the beam arm reads
/// the same field as the others.
fn shot(
    weapon: WeaponKind,
    origin: SimVec3,
    dir: SimVec3,
    target: Option<EntityId>,
    ids: IdSwap,
) -> Shot {
    match weapon {
        WeaponKind::Beam => Shot {
            pos: wire_vec(origin),
            dir: None,
            end: Some(wire_vec(dir)),
            target_id: None,
        },
        WeaponKind::Bullet | WeaponKind::Missile => Shot {
            pos: wire_vec(origin),
            dir: Some(wire_vec(dir)),
            end: None,
            target_id: target.map(|t| ids.to_wire(t)),
        },
    }
}

/// Closes the socket properly on the way out.
///
/// Without this the process can exit before the close frame leaves, and the
/// server only learns the player is gone when the TCP connection resets — which
/// it does handle, but as an abnormal close rather than a clean `leave`.
fn close_on_exit(
    mut exit: MessageReader<AppExit>,
    mut link: NonSendMut<NetLink>,
    mut status: ResMut<NetStatus>,
) {
    if exit.read().next().is_none() {
        return;
    }
    if link.socket.is_some() {
        info!("net: closing");
        link.wanted = false;
        // The only place a grace period is paid. Bounded, and only on the last
        // frame the app will ever run.
        link.hang_up(close_grace_ms());
        status.state = ConnState::Offline;
    }
}

/// Milliseconds [`close_on_exit`] will wait for the close handshake.
const fn close_grace_ms() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        CLOSE_GRACE.as_millis() as u64
    }
    // The browser flushes a `close()` itself; the page is not going anywhere
    // synchronously and there is nothing to wait on.
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The lobby session, and the handover into a networked match
// ─────────────────────────────────────────────────────────────────────────────

/// Where this client is in the room lifecycle.
///
/// Exactly the four states `public/src/lobby/rooms.js` moves through, named:
/// the JS keeps them as three booleans and a nullable room code, and the
/// combinations that cannot occur are the ones it has bugs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    /// No usable socket.
    #[default]
    Offline,
    /// Connected, in no room. `list-rooms`, `create` and `join` are legal here.
    Idle,
    /// In a room, waiting for the host to press start.
    Room,
    /// The match is running and [`SimWorld`] is the server's.
    Playing,
}

/// Everything the lobby knows that came off the socket.
///
/// **This is the seam `ui.rs` reads.** It is deliberately protocol types rather
/// than a second set of view structs: the server's `players` and `rooms-list`
/// rows are already exactly what a roster and a room browser want to draw, and
/// a translation layer here would be a second place for a field to go stale.
///
/// [`NetSession::rev`] is what makes it cheap to watch. `ui.rs` reduces its
/// whole screen to one `Copy` model and compares it whole; a `Vec<PlayerInfo>`
/// cannot go in that model, so the revision counter stands in for it. Every
/// mutation below goes through [`NetSession::bump`].
#[derive(Resource, Debug, Clone, Default)]
pub struct NetSession {
    /// Bumped on every change. Watch this, not the fields.
    pub rev: u32,
    /// Where in the lifecycle this client is.
    pub phase: Phase,
    /// Four-letter room code, empty outside a room.
    pub code: String,
    /// Whether the room is hidden from the browser.
    pub private: bool,
    /// Whether this client is the host — the only one the server accepts a
    /// `start` from, and the one that must drive any balance bot.
    pub host: bool,
    /// The id the server assigned this connection.
    pub you: Option<PlayerId>,
    /// The wire id <-> entity id exchange for this connection. See [`IdSwap`].
    pub ids: IdSwap,
    /// The roster, bots included, newest `players` broadcast.
    pub players: Vec<PlayerInfo>,
    /// The room browser, newest `rooms-list` reply.
    pub rooms: Vec<RoomSummary>,
    /// The last `error` the server sent, for the lobby's footer.
    pub notice: Option<String>,
    /// Credit balance after the last `match-credits`. `None` until one arrives,
    /// which for a guest is never.
    pub credits: Option<i64>,
    /// The callsign this client sent with `name`, so the handover into a match
    /// can label the local ship before the first `players` broadcast lands.
    pub callsign: String,
}

impl NetSession {
    /// Marks the session changed. Every write goes through here.
    fn bump(&mut self) {
        self.rev = self.rev.wrapping_add(1);
    }

    /// Back to square one, keeping only the callsign — which is a setting, not
    /// session state.
    fn reset(&mut self, phase: Phase) {
        let callsign = std::mem::take(&mut self.callsign);
        let rev = self.rev;
        *self = NetSession {
            rev,
            phase,
            callsign,
            ..NetSession::default()
        };
        self.bump();
    }

    /// Takes the pending server error, if there is one.
    ///
    /// Taking rather than reading: an `error` frame is a one-shot notice for
    /// the lobby's footer, and one that stayed set would keep re-announcing
    /// "Room not found" every time the page was repainted.
    pub fn take_notice(&mut self) -> Option<String> {
        let notice = self.notice.take();
        if notice.is_some() {
            self.bump();
        }
        notice
    }

    /// The roster row for a given id.
    #[must_use]
    pub fn player(&self, id: PlayerId) -> Option<&PlayerInfo> {
        self.players.iter().find(|p| p.id == id)
    }
}

/// Authoritative events waiting for the next simulation step.
///
/// Filled by [`ingest`] in `PreUpdate`, drained by `sim_bridge::fixed_tick`,
/// which hands the slice straight to `sim::tick`. Order within the batch is the
/// order the server sent it and is significant — an `hp` after a `death` means
/// something different from the reverse — so this is a `Vec` and never a set.
///
/// It is deliberately *not* a Bevy message. Messages are double-buffered and
/// read by generation, and a frame that runs no fixed step would drop the batch
/// into the previous generation; a resource the tick empties has exactly the
/// semantics wanted, which is "whatever arrived since the last tick".
#[derive(Resource, Debug, Default)]
pub struct NetInbox(pub Vec<NetEvent>);

/// Consumes the server's frames: updates [`NetSession`], fills [`NetInbox`],
/// and builds the match when `start` lands.
///
/// The one system in this module that reaches outside the transport, and the
/// reason it is here rather than in `sim_bridge` is that everything it touches
/// arrives as a [`ServerMessage`]. `sim_bridge` owns *building a solo match*;
/// this owns *receiving one*, and the two share `new_match`'s output types
/// rather than its code because a networked world takes its ships, its teams
/// and its asteroid field off the wire instead of from a seed.
#[allow(
    clippy::too_many_arguments,
    reason = "the handover writes four simulation resources at once; splitting \
              it would mean a second system that can only run interleaved with \
              this one and would have to re-read the same message stream."
)]
fn ingest(
    mut incoming: MessageReader<FromServer>,
    status: Res<NetStatus>,
    config: Res<NetConfig>,
    mut outbox: MessageWriter<ToServer>,
    mut session: ResMut<NetSession>,
    mut inbox: ResMut<NetInbox>,
    mut world: ResMut<SimWorld>,
    mut roster: ResMut<Roster>,
    mut setup: ResMut<MatchSetup>,
    mut frame: ResMut<SimFrame>,
) {
    // A socket that went away takes the room with it. The server has already
    // dropped us from `room.players`, so pretending otherwise would leave the
    // lobby offering a `start` nobody would answer.
    if !status.is_online() && session.phase != Phase::Offline {
        session.reset(Phase::Offline);
    }
    if status.is_online() && session.phase == Phase::Offline {
        session.phase = Phase::Idle;
        session.callsign.clone_from(&config.callsign);
        session.bump();
        // `lobby/net.js` sends this from `onopen`, and so does this: the server
        // keeps the name on the connection and every later `create`/`join`
        // inherits it, so it has to be the first thing out of the socket.
        outbox.write(ToServer(ClientMessage::Name {
            name: config.callsign.clone(),
        }));
    }

    for FromServer(msg) in incoming.read() {
        match msg {
            ServerMessage::Room {
                code,
                host,
                you,
                private,
            } => {
                session.code.clone_from(code);
                session.host = *host;
                session.you = Some(*you);
                session.ids = IdSwap::new(Some(*you));
                session.private = *private;
                session.phase = Phase::Room;
                session.notice = None;
                session.bump();
                info!("net: in room {code} as {you} (host: {host})");
            }
            ServerMessage::Players { players } => {
                session.players.clone_from(players);
                session.bump();
                // The roster is names, which `sim` deliberately does not carry.
                for p in players {
                    if let Some(id) = session.ids.to_entity(p.id) {
                        roster.name(id, p.name.clone());
                    }
                }
                // Teams and scores are simulation state and go through the
                // tick, one row at a time — `NetEvent` is `Copy` by contract.
                if session.phase == Phase::Playing {
                    for p in players {
                        let Some(id) = session.ids.to_entity(p.id) else {
                            continue;
                        };
                        inbox.0.push(NetEvent::PlayerRow {
                            id,
                            team: p.team.and_then(team_of),
                            kills: p.kills,
                            deaths: p.deaths,
                        });
                    }
                }
            }
            ServerMessage::RoomsList { rooms } => {
                session.rooms.clone_from(rooms);
                session.bump();
            }
            ServerMessage::Start {
                spawns,
                asteroids,
                map,
                bot_assignments,
                seed,
            } => {
                if session.you.is_none() {
                    warn!("net: `start` arrived before `room`; ignoring it");
                    continue;
                }
                let map = sim_map(*map);
                let bots: &[BotAssignment] = if session.host { bot_assignments } else { &[] };
                let (built, names) = build_online_world(
                    spawns,
                    asteroids,
                    map,
                    bots,
                    seed.unwrap_or(ONLINE_SEED),
                    &session,
                );
                info!(
                    "net: match start on {:?} — {} ships, {} rocks, {} bot(s) to drive",
                    map,
                    built.ships.len(),
                    built.asteroids.len(),
                    bots.len(),
                );
                inbox.0.clear();
                // One `Respawn` per ship, so the first tick of the match
                // announces every pose as *placed* rather than moved.
                // `scene.rs` snaps its interpolator on `ShipRespawned` and
                // blends on everything else, so without this a ship whose
                // entity was reused from the solo world that was on screen a
                // moment ago streaks a thousand units across the map over one
                // tick on its way to the spawn.
                for s in &built.ships {
                    inbox.0.push(NetEvent::Respawn {
                        id: s.id,
                        pos: s.pos,
                        quat: s.quat,
                    });
                }
                world.0 = built;
                *roster = names;
                frame.0 = Frame::new();
                *setup = MatchSetup {
                    mode: Mode::Multiplayer,
                    map,
                    seed: seed.unwrap_or(ONLINE_SEED),
                    hard_mode: false,
                    callsign: session.callsign.clone(),
                };
                session.phase = Phase::Playing;
                session.bump();
            }
            ServerMessage::MatchCredits {
                total_credits,
                credits_earned,
                ..
            } => {
                session.credits = Some(*total_credits);
                session.bump();
                info!("net: match credits +{credits_earned} (total {total_credits})");
            }
            ServerMessage::Error { message } => {
                session.notice = Some(message.clone());
                session.bump();
                warn!("net: server says: {message}");
            }
            // Painting a remote hull the colours its pilot chose is `scene.rs`'s
            // registry to grow; the simulation has no opinion about it, so
            // these two carry no `NetEvent` and are dropped rather than
            // half-applied.
            ServerMessage::Colors { .. } | ServerMessage::ShipModel { .. } => {}
            other => push_events(other, session.ids, &mut inbox.0),
        }
    }
}

/// The in-match half of the message set, as the simulation's own vocabulary.
///
/// Returns `None` for anything that is lobby state rather than world state —
/// those are handled above, where the session lives.
fn as_net_event(msg: &ServerMessage, ids: IdSwap) -> Option<NetEvent> {
    Some(match msg {
        ServerMessage::State {
            id,
            pos,
            quat,
            boost,
        } => NetEvent::RemoteState {
            id: ids.to_entity(*id)?,
            pos: sim_vec(*pos),
            quat: sim_quat(*quat),
            boost: *boost,
        },
        ServerMessage::Hp { id, hp } => NetEvent::Hp {
            id: ids.to_entity(*id)?,
            hp: *hp,
        },
        ServerMessage::Death { id, killer_id } => NetEvent::Death {
            id: ids.to_entity(*id)?,
            // `and_then`, not `map`: a killer id that does not fit an
            // `EntityId` is an unattributed kill, not a reason to drop the
            // death entirely.
            killer: killer_id.and_then(|k| ids.to_entity(k)),
        },
        ServerMessage::Respawn { id, pos, quat } => NetEvent::Respawn {
            id: ids.to_entity(*id)?,
            pos: sim_vec(*pos),
            quat: sim_quat(*quat),
        },
        // One event per shot: `NetEvent` is `Copy` with a fixed payload so a
        // batch is one contiguous slice, which is why the wire's `shots` array
        // is unrolled here rather than carried.
        ServerMessage::Fire { id, kind, shots } => {
            let shot = shots.first()?;
            NetEvent::Fired {
                id: ids.to_entity(*id)?,
                weapon: sim_weapon(*kind),
                origin: sim_vec(shot.pos),
                dir: sim_vec(shot_direction(shot)),
                target: shot.target_id.and_then(|t| ids.to_entity(t)),
            }
        }
        ServerMessage::Flare { id, pos, quat } => NetEvent::FlareBurst {
            id: ids.to_entity(*id)?,
            pos: sim_vec(*pos),
            quat: sim_quat(*quat),
        },
        ServerMessage::Disconnect { id } => NetEvent::Disconnect {
            id: ids.to_entity(*id)?,
        },
        ServerMessage::AsteroidHp { id, hp } => NetEvent::AsteroidHp { id: *id, hp: *hp },
        ServerMessage::AsteroidDestroyed { id } => NetEvent::AsteroidDestroyed { id: *id },
        ServerMessage::MatchState { timer, team_kills } => NetEvent::MatchState {
            timer: *timer,
            team_kills: *team_kills,
        },
        ServerMessage::MatchEnd { winner, team_kills } => NetEvent::MatchEnd {
            // `-1` is a draw (`server/index.js:450`).
            winner: u8::try_from(*winner).ok().and_then(team_of),
            team_kills: *team_kills,
        },
        _ => return None,
    })
}

/// Every event one message produces.
///
/// [`as_net_event`] answers for one, which is all any message but `fire`
/// carries. The JS client puts both muzzles of a bullet volley in one message's
/// `shots` array, and [`NetEvent`] is `Copy` with a fixed payload precisely so
/// that a batch is one contiguous slice — so the array is unrolled here.
fn push_events(msg: &ServerMessage, ids: IdSwap, out: &mut Vec<NetEvent>) {
    if let Some(event) = as_net_event(msg, ids) {
        out.push(event);
    }
    let ServerMessage::Fire { id, kind, shots } = msg else {
        return;
    };
    let Some(id) = ids.to_entity(*id) else { return };
    for shot in shots.iter().skip(1) {
        out.push(NetEvent::Fired {
            id,
            weapon: sim_weapon(*kind),
            origin: sim_vec(shot.pos),
            dir: sim_vec(shot_direction(shot)),
            target: shot.target_id.and_then(|t| ids.to_entity(t)),
        });
    }
}

/// A shot's direction as the simulation reads it.
///
/// A beam's `end` *is* what `sim` wants in `dir` — see
/// [`NetEvent::Fired::dir`], which documents the same overload — so `end` wins
/// where both are present. A shot carrying neither is malformed; nose-forward
/// keeps it a visual glitch rather than a `NaN` that propagates into the
/// simulation.
fn shot_direction(shot: &Shot) -> [f64; 3] {
    shot.end.or(shot.dir).unwrap_or([0.0, 0.0, 1.0])
}

/// Builds the world the server just described.
///
/// The networked counterpart of `sim_bridge::new_match`, and deliberately not a
/// branch inside it: that function `debug_assert!`s its mode is solo, decides
/// how many bots a skirmish has, and generates its own asteroid field from a
/// seed. None of those is true here. What the two share is their *output* —
/// a [`SimWorld`] and a [`Roster`] — which is the whole interface the renderer
/// has.
///
/// Four things come off the wire that a solo match invents:
///
/// - **The field.** The JS server's `generateAsteroidField` is unseeded
///   `Math.random`, so it cannot be reproduced; every rock is transcribed. A
///   Rust server sends [`ServerMessage::Start::seed`] instead and this list is
///   empty, in which case the field is generated the way solo does.
/// - **Spawns and teams**, keyed by player id, including any balance bot.
/// - **Who is real.** Every id in `spawns` that is not this client and not a
///   bot this client drives becomes a [`ShipKind::Remote`], so it exists — with
///   its team — before its first pose arrives. Letting `ingest_remote_state`
///   create it on demand would leave it team-less until the next `players`
///   broadcast, and `World::can_damage` reads a missing team as "hostile":
///   a window in which friendly fire lands.
/// - **The scoreboard rows.** `sim::tick`'s `credit_kill` books a kill by
///   *finding* the two rows and silently does nothing when they are absent.
fn build_online_world(
    spawns: &BTreeMap<PlayerId, Spawn>,
    asteroids: &[WireAsteroid],
    map: MapKind,
    bots: &[BotAssignment],
    seed: u64,
    session: &NetSession,
) -> (SimWorldState, Roster) {
    let rules = Rules::DEFAULT;
    let mut world = SimWorldState::new(seed, rules, Mode::Multiplayer, map);

    if asteroids.is_empty() {
        // Either the terrain map, which has no field, or a server that sent a
        // seed instead of sixteen kilobytes of records.
        if map == MapKind::Space {
            sim::asteroids::populate(&mut world);
        }
    } else {
        for a in asteroids {
            world.asteroids.push(Asteroid {
                id: a.id,
                pos: sim_vec(a.pos),
                size: a.size,
                radius: a.size * rules.world.asteroid_field.collision_radius_scale,
                hp: a.hp,
                tier: sim_tier(a.tier),
                variant: a.variant,
                rot: sim_vec(a.rot),
                spin: sim_vec(a.spin),
                hit_flash: 0.0,
            });
        }
        world.next_asteroid_id = world.asteroids.iter().map(|a| a.id).max().unwrap_or(0) + 1;
    }

    let mut roster = Roster::default();
    for (&wire_id, spawn) in spawns {
        let Some(id) = session.ids.to_entity(wire_id) else {
            warn!("net: spawn for id {wire_id}, which is not an entity id");
            continue;
        };
        let mine = bots.iter().any(|b| b.id == wire_id);
        // `LOCAL_ID` *is* the local ship, by construction: `IdSwap` put this
        // connection's server id there. Nine other modules depend on that.
        let kind = if id == LOCAL_ID {
            ShipKind::Local
        } else if mine {
            ShipKind::Bot
        } else {
            ShipKind::Remote
        };
        let mut ship = Ship::spawn(id, kind, sim_vec(spawn.pos), sim_quat(spawn.quat), &rules);
        ship.team = team_of(spawn.team);
        if kind == ShipKind::Bot {
            // `server/index.js:753` names it "Bot [Hard]" and means it.
            // `Ship::spawn` leaves `Ship::bot` at its default, which is a bot
            // with no missiles that would fire one immediately if it had any.
            sim::bot::init(&mut ship, true, false, &rules, &mut world.rng.bots);
        }
        world.match_state.scores.push(Score {
            id,
            team: ship.team,
            kills: 0,
            deaths: 0,
        });
        world.ships.push(ship);

        let name = session
            .player(wire_id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| format!("PILOT {wire_id}"));
        roster.name(id, name);
    }
    world.local_id = Some(LOCAL_ID);
    if !session.callsign.is_empty() {
        // The `players` broadcast that names everyone follows `start`, so until
        // it lands this is the only thing that knows what to call the pilot in
        // the seat.
        roster.name(LOCAL_ID, session.callsign.clone());
    }

    (world, roster)
}

// ─────────────────────────────────────────────────────────────────────────────
// Wire <-> simulation conversions
// ─────────────────────────────────────────────────────────────────────────────

/// Which entity id is "the ship this machine flies", on both sides of the wire.
///
/// # The bug this exists to stop
///
/// The rest of the client does not ask the simulation which ship is the
/// player's. It asks for **[`LOCAL_ID`], which is the constant `1`** —
/// `camera.rs`'s `follow`, every readout in `cockpit.rs`, `hud.rs`'s reticle and
/// killfeed, `terrain.rs`'s ground clearance, `warp.rs`, `scene.rs`'s paint, and
/// — the one that really matters — `input.rs`, which stamps `Input::id =
/// LOCAL_ID` on every frame of stick and throttle.
///
/// The server does not know about that. It allocates ids from a process-wide
/// counter (`nextId++`), so on a server that has seen a few connections the
/// pilot is id 26, not 1. Feeding those ids straight into the world produced a
/// match in which:
///
/// - `sim::tick`'s `integrate_players` matched the input's id `1` against no
///   ship at all, so the throttle and the stick did nothing;
/// - `camera.rs` found no ship `1` and left the camera at its startup pose, on
///   team 0's spawn — so the view was a fixed camera watching *the other
///   player* fly around. That is exactly what it looked like: spectating.
///
/// # The fix, and why it is a swap
///
/// Rather than teach nine modules to read [`sim::world::World::local_id`], the
/// two ids are exchanged at this boundary: whatever the server calls this
/// connection becomes `LOCAL_ID` inside the simulation, and whoever actually
/// holds `LOCAL_ID` on the wire — a real possibility, the first connection a
/// freshly started server accepts is id 1 — takes this connection's number in
/// its place.
///
/// A swap and not a renumbering, because a swap is a **bijection and its own
/// inverse**: one function serves incoming and outgoing traffic, ids never
/// collide, and a frame that round-trips through the server comes back
/// unchanged. Every id on the wire goes through it — `spawns` keys,
/// `botAssignments`, `targetId`, `fromBotId`, `killerId`, roster rows — so
/// there is no path by which a raw server id reaches the world.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IdSwap {
    /// This connection's server id, as an entity id. `None` before the `room`
    /// message, when the swap is the identity.
    mine: Option<EntityId>,
}

impl IdSwap {
    /// The swap for a connection the server calls `you`.
    fn new(you: Option<PlayerId>) -> IdSwap {
        IdSwap {
            mine: you.and_then(|y| EntityId::try_from(y).ok()),
        }
    }

    /// Exchanges [`LOCAL_ID`] and this connection's id, leaving everything else
    /// alone. Self-inverse, which is why it can be applied in both directions.
    fn apply(self, id: EntityId) -> EntityId {
        match self.mine {
            Some(mine) if id == mine => LOCAL_ID,
            Some(mine) if id == LOCAL_ID => mine,
            _ => id,
        }
    }

    /// A wire player id as a simulation entity id.
    ///
    /// [`PlayerId`] is `i64` because the server allocates from an unbounded
    /// counter; [`EntityId`] is `i32`. Every id the live server has ever issued
    /// fits, and one that does not is dropped rather than wrapped into somebody
    /// else's ship.
    fn to_entity(self, id: PlayerId) -> Option<EntityId> {
        EntityId::try_from(id).ok().map(|e| self.apply(e))
    }

    /// A simulation entity id as the wire's.
    fn to_wire(self, id: EntityId) -> PlayerId {
        PlayerId::from(self.apply(id))
    }
}

/// A team index as the simulation's team. Anything but 0 or 1 is unassigned,
/// which is what the wire's `null` means.
fn team_of(index: u8) -> Option<Team> {
    match index {
        0 => Some(Team::Zero),
        1 => Some(Team::One),
        _ => None,
    }
}

fn sim_vec(v: [f64; 3]) -> SimVec3 {
    SimVec3::new(v[0], v[1], v[2])
}

fn wire_vec(v: SimVec3) -> [f64; 3] {
    [v.x, v.y, v.z]
}

/// Both sides are `[x, y, z, w]` — `THREE.Quaternion.toArray()` order, w last.
fn sim_quat(q: [f64; 4]) -> SimQuat {
    SimQuat::new(q[0], q[1], q[2], q[3])
}

fn wire_quat(q: SimQuat) -> [f64; 4] {
    [q.x, q.y, q.z, q.w]
}

fn sim_weapon(kind: WireWeapon) -> WeaponKind {
    match kind {
        WireWeapon::Bullet => WeaponKind::Bullet,
        WireWeapon::Beam => WeaponKind::Beam,
        WireWeapon::Missile => WeaponKind::Missile,
    }
}

fn wire_weapon(kind: WeaponKind) -> WireWeapon {
    match kind {
        WeaponKind::Bullet => WireWeapon::Bullet,
        WeaponKind::Beam => WireWeapon::Beam,
        WeaponKind::Missile => WireWeapon::Missile,
    }
}

fn sim_tier(tier: WireTier) -> AsteroidTier {
    match tier {
        WireTier::Small => AsteroidTier::Small,
        WireTier::Medium => AsteroidTier::Medium,
        WireTier::Big => AsteroidTier::Big,
        WireTier::Huge => AsteroidTier::Huge,
    }
}

/// The wire's map as the simulation's.
pub fn sim_map(map: spaceships_protocol::MapKind) -> MapKind {
    match map {
        spaceships_protocol::MapKind::Terrain => MapKind::Terrain,
        spaceships_protocol::MapKind::Space => MapKind::Space,
    }
}

/// The simulation's map as the wire's.
pub fn wire_map(map: MapKind) -> spaceships_protocol::MapKind {
    match map {
        MapKind::Terrain => spaceships_protocol::MapKind::Terrain,
        MapKind::Space => spaceships_protocol::MapKind::Space,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// URL construction
// ─────────────────────────────────────────────────────────────────────────────

/// Appends `?token=` the way `lobby/net.js` does, or returns `base` unchanged
/// for a guest.
fn socket_url(base: &str, token: Option<&str>) -> String {
    match token {
        Some(t) if !t.is_empty() => {
            let sep = if base.contains('?') { '&' } else { '?' };
            format!("{base}{sep}token={}", encode_uri_component(t))
        }
        _ => base.to_owned(),
    }
}

/// `encodeURIComponent`, byte for byte.
///
/// A JWT is base64url plus dots and needs no escaping in practice, but the JS
/// escapes it and "in practice" is how wire formats drift. The unreserved set
/// is the one from ECMA-262: `A-Z a-z 0-9 - _ . ! ~ * ' ( )`.
fn encode_uri_component(s: &str) -> String {
    const UNRESERVED: &[u8] = b"-_.!~*'()";
    let mut out = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        if byte.is_ascii_alphanumeric() || UNRESERVED.contains(byte) {
            out.push(*byte as char);
        } else {
            out.push('%');
            out.push(
                char::from_digit((byte >> 4) as u32, 16)
                    .unwrap()
                    .to_ascii_uppercase(),
            );
            out.push(
                char::from_digit((byte & 0xf) as u32, 16)
                    .unwrap()
                    .to_ascii_uppercase(),
            );
        }
    }
    out
}

/// The endpoint and token this platform defaults to.
#[cfg(not(target_arch = "wasm32"))]
fn default_endpoint() -> (String, Option<String>) {
    let url = std::env::var(ENDPOINT_ENV).unwrap_or_else(|_| DEFAULT_URL.to_owned());
    let token = std::env::var("SPACESHIPS_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());
    (url, token)
}

/// Derives the endpoint from the page, exactly as `lobby/net.js` does: same
/// origin, `wss:` under `https:`, and the JWT out of
/// `localStorage['spaceships:token']`.
#[cfg(target_arch = "wasm32")]
fn default_endpoint() -> (String, Option<String>) {
    let Some(window) = web_sys::window() else {
        return (String::new(), None);
    };
    let location = window.location();
    let secure = location.protocol().is_ok_and(|p| p == "https:");
    let scheme = if secure { "wss:" } else { "ws:" };
    let host = location.host().unwrap_or_default();
    // Matches the JS guard for `file://`, which produces an empty host and a
    // `WebSocket` constructor that throws.
    let url = if host.is_empty() {
        String::new()
    } else {
        format!("{scheme}//{host}/ws")
    };

    let token = window
        .local_storage()
        .ok()
        .flatten()
        .and_then(|store| store.get_item("spaceships:token").ok().flatten())
        .filter(|t| !t.is_empty());

    (url, token)
}

// ─────────────────────────────────────────────────────────────────────────────
// The transport contract
// ─────────────────────────────────────────────────────────────────────────────

/// What a backend reports. Both produce `Open`, then zero or more `Text`, then
/// exactly one of `Closed` / `Failed`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SocketEvent {
    /// Handshake finished; sends will go out.
    Open,
    /// One text frame. Binary frames are dropped — the protocol is JSON text
    /// and the JS never sends anything else.
    Text(String),
    /// Terminal: the connection closed.
    Closed {
        /// RFC 6455 close code.
        code: u16,
        /// Close reason, often empty.
        reason: String,
    },
    /// Terminal: the connection failed. Includes a handshake that never
    /// completed.
    Failed(String),
}

// ─────────────────────────────────────────────────────────────────────────────
// Native backend: one thread, a current-thread tokio runtime, tokio-tungstenite
// ─────────────────────────────────────────────────────────────────────────────

/// Native `Socket`.
///
/// # Why a thread and not `IoTaskPool`
///
/// `tokio-tungstenite` needs a tokio reactor in task-local scope to drive its
/// `TcpStream`. Bevy's pools run on `async_executor`, which provides no
/// reactor, so a `connect_async` future spawned there would hang on the first
/// poll with "there is no reactor running". Options were: a second socket crate
/// with a smol-flavoured runtime (two async ecosystems in one binary), or one
/// thread hosting the runtime that the *server crate in this same workspace*
/// already pins. This is the second.
///
/// The thread costs ~8 KiB of stack and is idle in `epoll`/`kqueue` whenever
/// nothing is on the wire.
#[cfg(not(target_arch = "wasm32"))]
struct Socket {
    /// Main thread -> socket thread. Unbounded, so `send` is a lock-free
    /// enqueue and never blocks a Bevy system.
    outgoing: tokio::sync::mpsc::UnboundedSender<Outgoing>,
    /// Socket thread -> main thread.
    incoming: tokio::sync::mpsc::UnboundedReceiver<SocketEvent>,
    /// Drops when the socket thread returns; the close grace period waits on
    /// this and on nothing else.
    finished: std::sync::mpsc::Receiver<()>,
}

/// What the main thread asks the socket thread to do.
#[cfg(not(target_arch = "wasm32"))]
enum Outgoing {
    Text(String),
    Close,
}

#[cfg(not(target_arch = "wasm32"))]
impl Socket {
    fn connect(url: &str) -> Result<Self, String> {
        if url.starts_with("wss://") {
            // Better than failing four layers down inside the handshake with
            // "TLS support not compiled in".
            return Err(format!(
                "native build cannot speak wss:// ({url}); add \
                 features = [\"rustls-tls-webpki-roots\"] to the \
                 tokio-tungstenite dependency in crates/client/Cargo.toml"
            ));
        }
        if !url.starts_with("ws://") {
            return Err(format!("not a websocket url: {url}"));
        }

        let (out_tx, out_rx) = tokio::sync::mpsc::unbounded_channel();
        let (in_tx, in_rx) = tokio::sync::mpsc::unbounded_channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();

        let url = url.to_owned();
        std::thread::Builder::new()
            .name("spaceships-net".to_owned())
            .spawn(move || {
                // `done_tx` is moved in and never used; the receiver learns the
                // thread is finished from the disconnect.
                let _done = done_tx;
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt.block_on(run(url, out_rx, in_tx)),
                    Err(err) => {
                        let _ = in_tx.send(SocketEvent::Failed(format!("no runtime: {err}")));
                    }
                }
            })
            .map_err(|err| format!("could not start the socket thread: {err}"))?;

        Ok(Self {
            outgoing: out_tx,
            incoming: in_rx,
            finished: done_rx,
        })
    }

    /// Enqueues a frame. `false` means the socket thread is gone.
    fn send(&self, text: &str) -> bool {
        self.outgoing.send(Outgoing::Text(text.to_owned())).is_ok()
    }

    /// Moves everything that has arrived. Never blocks.
    fn drain(&mut self, out: &mut Vec<SocketEvent>) {
        while let Ok(event) = self.incoming.try_recv() {
            out.push(event);
        }
    }

    /// Asks for a clean close and optionally waits up to `grace_ms` for the
    /// thread to finish the handshake.
    fn close(self, grace_ms: u64) {
        let _ = self.outgoing.send(Outgoing::Close);
        if grace_ms > 0 {
            // `Disconnected` means the thread already returned, so this returns
            // immediately in the common case and only ever waits when the peer
            // is being slow about the close echo.
            let _ = self
                .finished
                .recv_timeout(std::time::Duration::from_millis(grace_ms));
        }
    }
}

/// The socket thread's whole life.
///
/// Split into a writer task and a reader loop, the same shape as
/// `crates/server/src/ws.rs`. A single `select!` over both would have to
/// `await` a send inside a branch body, which stops reading for the duration —
/// harmless at this traffic volume, but the split costs six lines and removes
/// the question.
#[cfg(not(target_arch = "wasm32"))]
async fn run(
    url: String,
    mut outgoing: tokio::sync::mpsc::UnboundedReceiver<Outgoing>,
    incoming: tokio::sync::mpsc::UnboundedSender<SocketEvent>,
) {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
    use tokio_tungstenite::tungstenite::protocol::CloseFrame;
    use tokio_tungstenite::tungstenite::{Message as Frame, Utf8Bytes};

    // `disable_nagle = true`. A 20 Hz position update is a small write followed
    // by a long silence, which is precisely the case Nagle delays by up to
    // 40 ms while it waits for more to coalesce.
    let stream = match tokio_tungstenite::connect_async_with_config(url.as_str(), None, true).await
    {
        Ok((stream, _response)) => stream,
        Err(err) => {
            let _ = incoming.send(SocketEvent::Failed(err.to_string()));
            return;
        }
    };
    if incoming.send(SocketEvent::Open).is_err() {
        return;
    }

    let (mut sink, mut source) = stream.split();

    let writer = tokio::spawn(async move {
        while let Some(cmd) = outgoing.recv().await {
            match cmd {
                Outgoing::Text(text) => {
                    if sink.send(Frame::Text(text.into())).await.is_err() {
                        return;
                    }
                }
                Outgoing::Close => break,
            }
        }
        // Reached on an explicit `Close` and on the channel being dropped,
        // which is what a dropped `Socket` looks like from here.
        let _ = sink
            .send(Frame::Close(Some(CloseFrame {
                code: CloseCode::Normal,
                reason: Utf8Bytes::from_static(""),
            })))
            .await;
        let _ = sink.flush().await;
    });

    let terminal = loop {
        match source.next().await {
            Some(Ok(Frame::Text(text))) => {
                if incoming.send(SocketEvent::Text(text.to_string())).is_err() {
                    // The main thread dropped the `Socket`; nobody is listening.
                    break None;
                }
            }
            Some(Ok(Frame::Close(frame))) => {
                break Some(match frame {
                    Some(f) => SocketEvent::Closed {
                        code: u16::from(f.code),
                        reason: f.reason.to_string(),
                    },
                    None => SocketEvent::Closed {
                        code: CLOSE_NORMAL,
                        reason: String::new(),
                    },
                })
            }
            // Ping/Pong are answered by the `WebSocketStream` itself, and the
            // protocol has no binary frames.
            Some(Ok(_)) => {}
            Some(Err(err)) => break Some(SocketEvent::Failed(err.to_string())),
            None => {
                break Some(SocketEvent::Closed {
                    code: CLOSE_ABNORMAL,
                    reason: "connection lost".to_owned(),
                })
            }
        }
    };

    if let Some(event) = terminal {
        let _ = incoming.send(event);
    }
    writer.abort();
    let _ = writer.await;
}

// ─────────────────────────────────────────────────────────────────────────────
// Web backend: `window.WebSocket` and four callbacks
// ─────────────────────────────────────────────────────────────────────────────

/// Browser `Socket`.
///
/// No task, no executor, no framing code: the browser owns the connection and
/// calls us. That also means it sidesteps the `Task`-cancelled-on-drop trap
/// described in the module docs, because there is no `Task`.
///
/// The `Closure`s are fields rather than `.forget()`-ed. `forget` leaks them for
/// the life of the page, which for a socket that reconnects is a slow leak of
/// closures *and* of the queues they capture; owning them means a dropped
/// `Socket` frees everything. The cost is that [`Socket::drop`] has to unhook
/// them from the `WebSocket` first — see there.
#[cfg(target_arch = "wasm32")]
struct Socket {
    ws: web_sys::WebSocket,
    /// Filled by the callbacks, emptied by [`Socket::drain`]. Single-threaded
    /// by construction: wasm has one thread and the callbacks run on it,
    /// between frames, never re-entrantly with `drain`.
    queue: std::rc::Rc<std::cell::RefCell<std::collections::VecDeque<SocketEvent>>>,
    _on_open: wasm_bindgen::closure::Closure<dyn FnMut()>,
    _on_message: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::MessageEvent)>,
    _on_error: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::Event)>,
    _on_close: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::CloseEvent)>,
}

#[cfg(target_arch = "wasm32")]
impl Socket {
    fn connect(url: &str) -> Result<Self, String> {
        use std::cell::RefCell;
        use std::collections::VecDeque;
        use std::rc::Rc;
        use wasm_bindgen::closure::Closure;
        use wasm_bindgen::JsCast;

        if url.is_empty() {
            // Mirrors the `lobby/net.js` guard, which refuses `file:` origins
            // with the same advice.
            return Err("no server origin — open the page over http(s)".to_owned());
        }

        let ws = web_sys::WebSocket::new(url).map_err(describe_js)?;
        // The protocol is text-only, but a stray binary frame should arrive as
        // something inspectable rather than a `Blob` that needs an async read.
        ws.set_binary_type(web_sys::BinaryType::Arraybuffer);

        let queue = Rc::new(RefCell::new(VecDeque::new()));

        let q = Rc::clone(&queue);
        let on_open = Closure::<dyn FnMut()>::new(move || {
            q.borrow_mut().push_back(SocketEvent::Open);
        });
        ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));

        let q = Rc::clone(&queue);
        let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(
            move |event: web_sys::MessageEvent| {
                // `as_string` is `None` for a binary frame, which is dropped.
                if let Some(text) = event.data().as_string() {
                    q.borrow_mut().push_back(SocketEvent::Text(text));
                }
            },
        );
        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        let q = Rc::clone(&queue);
        let on_error = Closure::<dyn FnMut(web_sys::Event)>::new(move |_: web_sys::Event| {
            // The spec deliberately gives no detail here, to avoid leaking
            // cross-origin information. A `close` always follows; the state
            // machine in `service` keeps whichever arrives first and ignores
            // the other.
            q.borrow_mut()
                .push_back(SocketEvent::Failed("websocket error".to_owned()));
        });
        ws.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        let q = Rc::clone(&queue);
        let on_close =
            Closure::<dyn FnMut(web_sys::CloseEvent)>::new(move |event: web_sys::CloseEvent| {
                q.borrow_mut().push_back(SocketEvent::Closed {
                    code: event.code(),
                    reason: event.reason(),
                });
            });
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        Ok(Self {
            ws,
            queue,
            _on_open: on_open,
            _on_message: on_message,
            _on_error: on_error,
            _on_close: on_close,
        })
    }

    fn send(&self, text: &str) -> bool {
        // `send` on a CONNECTING socket throws `InvalidStateError`; on a
        // CLOSING/CLOSED one it is a silent no-op that still counts the bytes.
        // Check first so the caller's return value means what it says.
        self.ws.ready_state() == web_sys::WebSocket::OPEN && self.ws.send_with_str(text).is_ok()
    }

    fn drain(&mut self, out: &mut Vec<SocketEvent>) {
        out.extend(self.queue.borrow_mut().drain(..));
    }

    /// `grace_ms` is ignored: the browser flushes the close frame itself and
    /// there is no thread to wait for. Taking the argument anyway keeps the two
    /// backends interchangeable at the call site.
    fn close(self, _grace_ms: u64) {
        drop(self);
    }
}

#[cfg(target_arch = "wasm32")]
impl Drop for Socket {
    fn drop(&mut self) {
        // Order matters and this is the wasm-side version of the "detach it or
        // it dies" trap. A `Closure` frees the wasm-side function table entry
        // when it drops; if the `WebSocket` still holds a reference the browser
        // will call a freed slot on the next event — which for a socket being
        // closed is *guaranteed*, because `close()` fires `onclose`.
        self.ws.set_onopen(None);
        self.ws.set_onmessage(None);
        self.ws.set_onerror(None);
        self.ws.set_onclose(None);
        let _ = self.ws.close_with_code(CLOSE_NORMAL);
    }
}

/// Best effort at a readable message out of a `JsValue`.
#[cfg(target_arch = "wasm32")]
fn describe_js(value: wasm_bindgen::JsValue) -> String {
    value
        .as_string()
        .unwrap_or_else(|| format!("javascript error: {value:?}"))
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guest_url_carries_no_query() {
        assert_eq!(
            socket_url("ws://127.0.0.1:4000/ws", None),
            "ws://127.0.0.1:4000/ws"
        );
        assert_eq!(
            socket_url("ws://127.0.0.1:4000/ws", Some("")),
            "ws://127.0.0.1:4000/ws"
        );
    }

    #[test]
    fn a_token_becomes_a_query_param() {
        assert_eq!(
            socket_url("wss://example.com/ws", Some("abc.def.ghi")),
            "wss://example.com/ws?token=abc.def.ghi"
        );
        // An endpoint that already has a query keeps it.
        assert_eq!(
            socket_url("ws://h/ws?debug=1", Some("t")),
            "ws://h/ws?debug=1&token=t"
        );
    }

    /// The escaping has to match `encodeURIComponent`, because the server reads
    /// the value with a standard query parser and the JS client is the
    /// reference implementation.
    #[test]
    fn the_token_is_encoded_like_javascript() {
        // Unreserved set: nothing changes.
        assert_eq!(encode_uri_component("AZaz09-_.!~*'()"), "AZaz09-_.!~*'()");
        // Everything else does.
        assert_eq!(encode_uri_component("a b"), "a%20b");
        assert_eq!(encode_uri_component("a+b/c=d&e?f"), "a%2Bb%2Fc%3Dd%26e%3Ff");
        // Multi-byte characters go out as UTF-8 octets.
        assert_eq!(encode_uri_component("é"), "%C3%A9");
    }

    #[test]
    fn the_backoff_climbs_and_then_holds() {
        assert_eq!(backoff(0), 0.5, "a zeroth attempt is treated as the first");
        assert_eq!(backoff(1), 0.5);
        assert_eq!(backoff(2), 1.0);
        assert_eq!(backoff(5), 8.0);
        assert_eq!(backoff(500), 8.0, "saturates rather than panicking");
    }

    /// The cadence this module used to own now belongs to `sim::tick`, and the
    /// rate it produces is a `sim` rule rather than a constant here. Pinned so
    /// that a change to it is a decision: the JS sends at
    /// `STATE_INTERVAL = 1 / 20` (`main.js:978`), and a client sending faster
    /// than that would flood a room of browser players.
    #[test]
    fn the_state_cadence_is_still_20_hz() {
        assert!(
            (Rules::DEFAULT.match_rules.state_send_interval - 1.0 / 20.0).abs() < 1e-12,
            "the wire rate moved away from the JS client's 20 Hz"
        );
    }

    /// The codec both directions of the transport use, exercised against the
    /// exact bytes the JS server emits and accepts.
    #[test]
    fn the_codec_matches_the_wire() {
        let encoded = serde_json::to_string(&ClientMessage::Name {
            name: "Maverick".to_owned(),
        })
        .unwrap();
        assert_eq!(encoded, r#"{"type":"name","name":"Maverick"}"#);

        let encoded = serde_json::to_string(&ClientMessage::Create {
            private: false,
            map: spaceships_protocol::MapKind::Space,
            allow_bot: false,
        })
        .unwrap();
        assert_eq!(
            encoded,
            r#"{"type":"create","private":false,"map":"space","allowBot":false}"#
        );

        let decoded: ServerMessage = serde_json::from_str(
            r#"{"type":"room","code":"ABCD","host":true,"you":1,"private":false}"#,
        )
        .unwrap();
        let ServerMessage::Room { code, you, .. } = decoded else {
            panic!("expected a room message");
        };
        assert_eq!(code, "ABCD");
        assert_eq!(you, 1);
    }

    // -- outbound: NetIntent -> the exact bytes server/index.js reads --------

    /// A connection the server calls `1`, so the swap is the identity and the
    /// expected bytes below are the raw ids. `the_local_ship_is_always_id_one`
    /// is where the exchange itself is pinned.
    const PLAIN: IdSwap = IdSwap { mine: Some(1) };

    fn json(intent: NetIntent) -> String {
        serde_json::to_string(&encode_intent(intent, PLAIN)).unwrap()
    }

    /// The whole outbound half, against the shapes `handleConnection` matches
    /// on. Every one of these is a message the JS *server* parses and the JS
    /// *client* renders, so a field renamed here is a Rust player who is
    /// invisible rather than a test that fails somewhere obvious.
    #[test]
    fn every_intent_encodes_to_the_shape_the_js_server_reads() {
        assert_eq!(
            json(NetIntent::State {
                pos: SimVec3::new(1.0, 2.0, 3.0),
                quat: SimQuat::IDENTITY,
                boost: true,
            }),
            r#"{"type":"state","pos":[1.0,2.0,3.0],"quat":[0.0,0.0,0.0,1.0],"boost":true}"#
        );

        // A bullet carries `dir`, and no `end`/`targetId` keys at all — the JS
        // omits absent fields rather than sending `null`.
        assert_eq!(
            json(NetIntent::Fire {
                weapon: WeaponKind::Bullet,
                origin: SimVec3::new(0.0, 0.0, 0.0),
                dir: SimVec3::new(0.0, 0.0, 1.0),
                target: None,
            }),
            r#"{"type":"fire","kind":"bullet","shots":[{"pos":[0.0,0.0,0.0],"dir":[0.0,0.0,1.0]}]}"#
        );

        // A beam carries `end` instead, which is the endpoint the shooter
        // already resolved. `sim` packs it into the intent's `dir`.
        assert_eq!(
            json(NetIntent::Fire {
                weapon: WeaponKind::Beam,
                origin: SimVec3::new(0.0, 1.0, 2.0),
                dir: SimVec3::new(0.0, 1.0, 900.0),
                target: None,
            }),
            r#"{"type":"fire","kind":"beam","shots":[{"pos":[0.0,1.0,2.0],"end":[0.0,1.0,900.0]}]}"#
        );

        assert_eq!(
            json(NetIntent::Fire {
                weapon: WeaponKind::Missile,
                origin: SimVec3::ZERO,
                dir: SimVec3::new(0.0, 0.0, 1.0),
                target: Some(7),
            }),
            r#"{"type":"fire","kind":"missile","shots":[{"pos":[0.0,0.0,0.0],"dir":[0.0,0.0,1.0],"targetId":7}]}"#
        );

        assert_eq!(
            json(NetIntent::Hit {
                target: 4,
                weapon: WeaponKind::Missile,
                from_bot: None,
            }),
            r#"{"type":"hit","targetId":4,"kind":"missile"}"#
        );
        // `fromBotId` is what `server/index.js:917` credits a locally driven
        // bot's kill to, and it is only accepted from the room's `botHostId`.
        assert_eq!(
            json(NetIntent::Hit {
                target: 4,
                weapon: WeaponKind::Bullet,
                from_bot: Some(-3),
            }),
            r#"{"type":"hit","targetId":4,"kind":"bullet","fromBotId":-3}"#
        );

        assert_eq!(
            json(NetIntent::SelfDamage { amount: 22 }),
            r#"{"type":"self-damage","dmg":22.0}"#
        );
        assert_eq!(
            json(NetIntent::AsteroidHit { id: 41 }),
            r#"{"type":"asteroid-hit","id":41}"#
        );
        assert_eq!(
            json(NetIntent::BotState {
                id: -2,
                pos: SimVec3::new(5.0, 0.0, -5.0),
                quat: SimQuat::FLIP_Y,
            }),
            r#"{"type":"bot-state","botId":-2,"pos":[5.0,0.0,-5.0],"quat":[0.0,1.0,0.0,0.0]}"#
        );
        assert_eq!(
            json(NetIntent::Flare {
                pos: SimVec3::ZERO,
                quat: SimQuat::IDENTITY,
            }),
            r#"{"type":"flare","pos":[0.0,0.0,0.0],"quat":[0.0,0.0,0.0,1.0]}"#
        );
    }

    // -- inbound: ServerMessage -> NetEvent ---------------------------------

    fn decode(text: &str) -> Vec<NetEvent> {
        decode_as(text, PLAIN)
    }

    fn decode_as(text: &str, ids: IdSwap) -> Vec<NetEvent> {
        let msg: ServerMessage = serde_json::from_str(text).unwrap();
        let mut out = Vec::new();
        push_events(&msg, ids, &mut out);
        out
    }

    #[test]
    fn a_relayed_pose_becomes_a_remote_state() {
        let events =
            decode(r#"{"type":"state","id":3,"pos":[1,2,3],"quat":[0,0,0,1],"boost":true}"#);
        assert_eq!(
            events,
            vec![NetEvent::RemoteState {
                id: 3,
                pos: SimVec3::new(1.0, 2.0, 3.0),
                quat: SimQuat::IDENTITY,
                boost: true,
            }]
        );
    }

    /// The JS client puts both muzzles of a volley in one message. `NetEvent`
    /// is one shot, so the array has to be unrolled or the second bolt of every
    /// remote burst is silently dropped.
    #[test]
    fn a_two_muzzle_volley_becomes_two_events() {
        let events = decode(
            r#"{"type":"fire","id":2,"kind":"bullet","shots":[
                 {"pos":[1,0,0],"dir":[0,0,1]},
                 {"pos":[-1,0,0],"dir":[0,0,1]}]}"#,
        );
        assert_eq!(events.len(), 2);
        for (i, x) in [1.0, -1.0].into_iter().enumerate() {
            let NetEvent::Fired { id, origin, .. } = events[i] else {
                panic!("expected a shot, got {:?}", events[i]);
            };
            assert_eq!(id, 2);
            assert_eq!(origin.x, x);
        }
    }

    /// A beam's endpoint arrives as `end` and `sim` reads it out of `dir`. Get
    /// this wrong and every remote beam fires along `+z` from the muzzle.
    #[test]
    fn a_beams_endpoint_lands_in_dir() {
        let events = decode(
            r#"{"type":"fire","id":9,"kind":"beam","shots":[{"pos":[0,0,0],"end":[0,0,900]}]}"#,
        );
        let [NetEvent::Fired { weapon, dir, .. }] = events[..] else {
            panic!("expected one shot, got {events:?}");
        };
        assert_eq!(weapon, WeaponKind::Beam);
        assert_eq!(dir, SimVec3::new(0.0, 0.0, 900.0));
    }

    /// `killerId` is an explicit `null` for a self-inflicted death, never
    /// omitted (`server/index.js:894`).
    #[test]
    fn an_unattributed_death_survives() {
        assert_eq!(
            decode(r#"{"type":"death","id":5,"killerId":null}"#),
            vec![NetEvent::Death {
                id: 5,
                killer: None
            }]
        );
        assert_eq!(
            decode(r#"{"type":"death","id":5,"killerId":2}"#),
            vec![NetEvent::Death {
                id: 5,
                killer: Some(2)
            }]
        );
    }

    /// `-1` is a draw, and must not become team 1.
    #[test]
    fn a_draw_has_no_winner() {
        assert_eq!(
            decode(r#"{"type":"match-end","winner":-1,"teamKills":[3,3]}"#),
            vec![NetEvent::MatchEnd {
                winner: None,
                team_kills: [3, 3],
            }]
        );
        assert_eq!(
            decode(r#"{"type":"match-end","winner":1,"teamKills":[3,5]}"#),
            vec![NetEvent::MatchEnd {
                winner: Some(Team::One),
                team_kills: [3, 5],
            }]
        );
    }

    /// Lobby traffic is session state, not world state, and must not reach the
    /// simulation as an event.
    #[test]
    fn lobby_frames_produce_no_events() {
        for text in [
            r#"{"type":"room","code":"ABCD","host":true,"you":1,"private":false}"#,
            r#"{"type":"rooms-list","rooms":[]}"#,
            r#"{"type":"error","message":"Room not found"}"#,
        ] {
            assert!(decode(text).is_empty(), "{text} produced events");
        }
    }

    /// The server allocates ids from an unbounded counter and `EntityId` is
    /// `i32`. One that does not fit is dropped rather than wrapped into
    /// somebody else's ship.
    #[test]
    fn an_out_of_range_id_is_dropped() {
        assert_eq!(PLAIN.to_entity(1), Some(1));
        assert_eq!(PLAIN.to_entity(-3), Some(-3));
        assert_eq!(PLAIN.to_entity(i64::from(i32::MAX) + 1), None);
        assert!(decode(r#"{"type":"hp","id":99999999999,"hp":50}"#).is_empty());
    }

    /// The whole client asks for [`LOCAL_ID`] — the constant `1` — when it
    /// means "my ship": `input.rs` stamps it on every frame of stick and
    /// throttle, and `camera.rs` follows it. The server does not know that and
    /// hands out `nextId++`.
    ///
    /// Without the exchange, a pilot the server calls `26` flew nothing (the
    /// input matched no ship) and watched a camera parked on team 0's spawn
    /// while the *other* player flew around in front of it — which is exactly
    /// what it looked like from the seat.
    #[test]
    fn the_local_ship_is_always_id_one() {
        let ids = IdSwap::new(Some(26));

        // Me <-> 1, both directions, and the swap is its own inverse.
        assert_eq!(ids.to_entity(26), Some(LOCAL_ID));
        assert_eq!(ids.to_wire(LOCAL_ID), 26);
        // ...and whoever really holds 1 on the wire — the first connection a
        // freshly started server accepts — takes my number rather than
        // colliding with me.
        assert_eq!(ids.to_entity(1), Some(26));
        assert_eq!(ids.to_wire(26), 1);
        // Everyone else is untouched, bots included.
        for other in [2, 7, -3] {
            assert_eq!(ids.to_entity(other), Some(other as EntityId));
            assert_eq!(ids.to_wire(other as EntityId), other);
        }

        // It is a bijection: no two wire ids land on the same entity.
        let mapped: Vec<EntityId> = (1..=30).filter_map(|w| ids.to_entity(w)).collect();
        let mut sorted = mapped.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), mapped.len(), "ids collided: {mapped:?}");

        // And it applies to traffic in both directions, not just to bare ids.
        assert_eq!(
            decode_as(r#"{"type":"hp","id":26,"hp":70}"#, ids),
            vec![NetEvent::Hp {
                id: LOCAL_ID,
                hp: 70
            }]
        );
        assert_eq!(
            serde_json::to_string(&encode_intent(
                NetIntent::Hit {
                    target: 2,
                    weapon: WeaponKind::Bullet,
                    from_bot: None,
                },
                ids,
            ))
            .unwrap(),
            r#"{"type":"hit","targetId":2,"kind":"bullet"}"#
        );

        // Before `room` lands there is nothing to swap, and the identity is the
        // only safe answer.
        let unknown = IdSwap::new(None);
        assert_eq!(unknown.to_entity(26), Some(26));
        assert_eq!(unknown.to_wire(LOCAL_ID), PlayerId::from(LOCAL_ID));
    }

    // -- the handover -------------------------------------------------------

    fn spawn_at(team: u8, z: f64) -> Spawn {
        Spawn {
            team,
            pos: [0.0, 0.0, z],
            quat: if team == 0 {
                [0.0, 0.0, 0.0, 1.0]
            } else {
                [0.0, 1.0, 0.0, 0.0]
            },
        }
    }

    fn three_player_start() -> BTreeMap<PlayerId, Spawn> {
        BTreeMap::from([
            (1, spawn_at(0, -540.0)),
            (2, spawn_at(1, 540.0)),
            (-3, spawn_at(0, -540.0)),
        ])
    }

    #[test]
    fn the_start_message_builds_the_match() {
        let rocks = vec![WireAsteroid {
            id: 0,
            pos: [10.0, 0.0, 20.0],
            rot: [0.0, 0.0, 0.0],
            size: 12.0,
            hp: 10,
            tier: WireTier::Medium,
            variant: 3,
            spin: [0.1, 0.2, 0.3],
        }];
        let bots = [BotAssignment {
            id: -3,
            team: 0,
            pos: [0.0, 0.0, -540.0],
            quat: [0.0, 0.0, 0.0, 1.0],
        }];
        let session = NetSession {
            host: true,
            callsign: "RUSTY".to_owned(),
            you: Some(1),
            ids: IdSwap::new(Some(1)),
            ..NetSession::default()
        };

        let (world, roster) = build_online_world(
            &three_player_start(),
            &rocks,
            MapKind::Space,
            &bots,
            7,
            &session,
        );

        // The server owns hit points; that is the whole reason this world is
        // built differently from a solo one.
        assert_eq!(world.authority, sim::world::Authority::Server);
        assert_eq!(world.mode, Mode::Multiplayer);
        assert_eq!(world.local_id, Some(1));

        // One ship per spawn, and the *kind* is what decides who is flown from
        // here and who is chased toward a relayed pose.
        assert_eq!(world.ships.len(), 3);
        assert_eq!(world.ship(1).unwrap().kind, ShipKind::Local);
        assert_eq!(world.ship(2).unwrap().kind, ShipKind::Remote);
        assert_eq!(world.ship(-3).unwrap().kind, ShipKind::Bot);

        // Teams come off the wire, not from an id parity trick — and they are
        // present *before* the first pose, which is what stops `can_damage`
        // reading a team-less ship as hostile.
        assert_eq!(world.ship(1).unwrap().team, Some(Team::Zero));
        assert_eq!(world.ship(2).unwrap().team, Some(Team::One));
        // Everyone spawns invulnerable, so `can_damage` is asked against a
        // world where the two-second window has run out — the teams are the
        // thing under test, not the spawn protection.
        let mut armed = world.clone();
        for s in &mut armed.ships {
            s.invuln_timer = 0.0;
        }
        assert!(!armed.can_damage(1, armed.ship(-3).unwrap()), "same team");
        assert!(armed.can_damage(1, armed.ship(2).unwrap()), "other team");

        // The balance bot has to be armed or it flies as a disarmed statue.
        assert!(world.ship(-3).unwrap().bot.missiles_left > 0);

        // The field is transcribed, not regenerated: the JS server's
        // `generateAsteroidField` is unseeded `Math.random`.
        assert_eq!(world.asteroids.len(), 1);
        let rock = world.asteroids[0];
        assert_eq!(rock.id, 0);
        assert_eq!(rock.hp, 10);
        assert_eq!(rock.variant, 3);
        assert_eq!(rock.tier, AsteroidTier::Medium);
        assert!(
            (rock.radius - 12.0 * Rules::DEFAULT.world.asteroid_field.collision_radius_scale).abs()
                < 1e-9
        );
        assert_eq!(world.next_asteroid_id, 1);

        // `credit_kill` books a kill by *finding* these rows and does nothing
        // at all when they are absent.
        assert_eq!(world.match_state.scores.len(), 3);
        assert_eq!(roster.callsign(1), "RUSTY");
    }

    /// `botAssignments` is broadcast to the whole room but only the host may
    /// drive them — `server/index.js` rejects a `bot-state` from anyone else,
    /// and two clients flying the same bot would fight over its pose.
    #[test]
    fn only_the_host_drives_the_balance_bot() {
        let bots = [BotAssignment {
            id: -3,
            team: 0,
            pos: [0.0, 0.0, -540.0],
            quat: [0.0, 0.0, 0.0, 1.0],
        }];
        let guest = NetSession {
            you: Some(2),
            ids: IdSwap::new(Some(2)),
            ..NetSession::default()
        };
        let (world, _) = build_online_world(
            &three_player_start(),
            &[],
            MapKind::Space,
            // What `ingest` passes a non-host: the empty slice.
            &[],
            7,
            &guest,
        );
        assert_eq!(world.ship(-3).unwrap().kind, ShipKind::Remote);
        assert_eq!(bots.len(), 1, "the wire still carried one");
    }

    /// An empty `asteroids` array on the space map means a server that sent a
    /// seed instead — a Rust one. The field is then generated rather than left
    /// empty, which is what makes the seed worth sending.
    #[test]
    fn a_seeded_start_generates_its_own_field() {
        let session = NetSession {
            you: Some(1),
            ids: IdSwap::new(Some(1)),
            ..NetSession::default()
        };
        let (with_rocks, _) =
            build_online_world(&three_player_start(), &[], MapKind::Space, &[], 7, &session);
        assert!(!with_rocks.asteroids.is_empty());

        // The terrain map has no field in either direction.
        let (terrain, _) = build_online_world(
            &three_player_start(),
            &[],
            MapKind::Terrain,
            &[],
            7,
            &session,
        );
        assert!(terrain.asteroids.is_empty());
    }

    // -- the endpoint -------------------------------------------------------

    /// A packaged build has to reach a server without anyone typing a URL, and
    /// the lobby has to be able to move it without an environment variable.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_endpoint_cycles_and_reads_short() {
        let mut config = NetConfig {
            url: DEFAULT_URL.to_owned(),
            ..NetConfig::default()
        };
        assert_eq!(config.endpoint_label(), "gheat.net");
        assert_eq!(config.endpoint_index(), Some(0));

        assert!(config.cycle_endpoint());
        assert_eq!(config.url, "ws://127.0.0.1:4000/ws");
        assert_eq!(config.endpoint_label(), "127.0.0.1:4000");

        assert!(config.cycle_endpoint());
        assert_eq!(config.url, DEFAULT_URL, "the cycle wraps");

        // An endpoint nobody listed still labels, and cycling from it starts at
        // the top rather than panicking.
        config.url = "ws://10.0.0.4:4000/ws".to_owned();
        assert_eq!(config.endpoint_index(), None);
        assert_eq!(config.endpoint_label(), "10.0.0.4:4000");
        assert!(config.cycle_endpoint());
        assert_eq!(config.url, ENDPOINTS[0]);
    }

    /// A frame the client cannot decode must be counted and skipped, never
    /// fatal — the JS ignores unknown tags and a Rust client that disconnects
    /// on one would be a regression.
    #[test]
    fn an_unknown_tag_does_not_decode() {
        assert!(serde_json::from_str::<ServerMessage>(r#"{"type":"not-a-real-tag"}"#).is_err());
    }

    /// `NetStatus` is what `hud.rs` will draw; keep it drawable.
    #[test]
    fn the_status_has_a_label_for_every_state() {
        for state in [
            ConnState::Offline,
            ConnState::Connecting,
            ConnState::Online,
            ConnState::Retrying,
            ConnState::Failed,
        ] {
            let status = NetStatus {
                state,
                ..Default::default()
            };
            assert!(!status.label().is_empty());
            assert_eq!(status.is_online(), state == ConnState::Online);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_native_socket_refuses_a_url_it_cannot_speak() {
        let Err(err) = Socket::connect("wss://example.com/ws") else {
            panic!("a native build has no TLS and must say so up front");
        };
        assert!(err.contains("rustls-tls-webpki-roots"), "{err}");
        assert!(Socket::connect("http://example.com/ws").is_err());
    }

    /// A connection refused has to arrive as a `Failed` event rather than a
    /// panic or a hang — this is the path every `cargo run` without a server
    /// takes.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_refused_connection_reports_failed() {
        // Port 1 is reserved and nothing listens on it.
        let mut socket = Socket::connect("ws://127.0.0.1:1/ws").expect("thread spawns");
        let mut events = Vec::new();
        for _ in 0..200 {
            socket.drain(&mut events);
            if !events.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            matches!(events.first(), Some(SocketEvent::Failed(_))),
            "expected a Failed event, got {events:?}"
        );
    }

    /// A real round trip against a real server.
    ///
    /// Ignored by default because it needs a listener; this is not a hermetic
    /// test and pretending otherwise would make CI flaky. Run it with:
    ///
    /// ```text
    /// cp pilots.db /tmp/pilots-test.db
    /// PILOTS_DB=/tmp/pilots-test.db PORT=4100 cargo run -p spaceships-server &
    /// SPACESHIPS_TEST_SERVER=ws://127.0.0.1:4100/ws \
    ///   cargo test -p spaceships-client -- --ignored --nocapture
    /// ```
    ///
    /// It drives the same `Socket` the game does — no test-only transport — so
    /// a pass means the shipped path works.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    #[ignore = "needs a live server; see the doc comment for the command"]
    fn a_live_server_answers_a_create() {
        let url = std::env::var("SPACESHIPS_TEST_SERVER")
            .unwrap_or_else(|_| "ws://127.0.0.1:4100/ws".to_owned());
        let mut socket = Socket::connect(&url).expect("thread spawns");

        let mut events = Vec::new();
        let mut seen: Vec<ServerMessage> = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);

        // Same order as `lobby/rooms.js`: `name` (which has no reply), then
        // `create` (which answers with `room` and broadcasts `players`).
        let send = |socket: &Socket, msg: ClientMessage| {
            let text = serde_json::to_string(&msg).unwrap();
            println!("-> {text}");
            assert!(socket.send(&text), "send failed");
        };

        while std::time::Instant::now() < deadline {
            socket.drain(&mut events);
            for event in events.drain(..) {
                match event {
                    SocketEvent::Open => {
                        println!("<- open");
                        send(
                            &socket,
                            ClientMessage::Name {
                                name: "RustPilot".to_owned(),
                            },
                        );
                        send(
                            &socket,
                            ClientMessage::Create {
                                private: false,
                                map: spaceships_protocol::MapKind::Space,
                                allow_bot: false,
                            },
                        );
                    }
                    SocketEvent::Text(text) => {
                        println!("<- {text}");
                        seen.push(
                            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}")),
                        );
                    }
                    other => panic!("socket died: {other:?}"),
                }
            }
            let got_room = seen.iter().any(|m| matches!(m, ServerMessage::Room { .. }));
            let got_players = seen
                .iter()
                .any(|m| matches!(m, ServerMessage::Players { .. }));
            if got_room && got_players {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let room = seen
            .iter()
            .find_map(|m| match m {
                ServerMessage::Room {
                    code, host, you, ..
                } => Some((code.clone(), *host, *you)),
                _ => None,
            })
            .expect("no `room` message; is the server running?");
        assert_eq!(room.0.len(), 4, "room code is four letters: {}", room.0);
        assert!(room.1, "the creator is the host");
        assert!(room.2 > 0, "a human gets a positive id");

        let players = seen
            .iter()
            .find_map(|m| match m {
                ServerMessage::Players { players } => Some(players.clone()),
                _ => None,
            })
            .expect("no `players` message");
        assert_eq!(players.len(), 1);
        assert_eq!(players[0].name, "RustPilot");
        assert!(players[0].host);

        socket.close(500);
    }

    /// The other half of the lifecycle: two sockets in one room, a started
    /// match, and a `state` frame that actually reaches the other player.
    ///
    /// [`a_live_server_answers_a_create`] proves the lobby handshake;
    /// [`the_state_cadence_is_still_20_hz`] proves the rate. This is the
    /// one that proves the *in-match* path, which is where every frame of the
    /// game's traffic actually goes. Same command as the other live test.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    #[ignore = "needs a live server; see `a_live_server_answers_a_create`"]
    fn a_started_match_relays_state_between_two_clients() {
        let url = std::env::var("SPACESHIPS_TEST_SERVER")
            .unwrap_or_else(|_| "ws://127.0.0.1:4100/ws".to_owned());

        let mut host = LiveClient::connect(&url, "HostPilot");
        let code = host
            .wait_for(|m| match m {
                ServerMessage::Room { code, .. } => Some(code.clone()),
                _ => None,
            })
            .expect("host got no `room`; is the server running?");

        // The guest creates its own room first (that is what `connect` does),
        // then leaves it by joining the host's — the same thing the lobby does
        // when you back out of "Create" and use a code instead.
        let mut guest = LiveClient::connect(&url, "GuestPilot");
        guest.send(ClientMessage::Leave);
        guest.send(ClientMessage::Join { code });
        let guest_id = guest
            .wait_for(|m| match m {
                // Two `room` messages arrive: one for the guest's own room and
                // one for the host's. `you` is the connection id and is the
                // same in both.
                ServerMessage::Room { you, .. } => Some(*you),
                _ => None,
            })
            .expect("guest got no `room`");

        // The host sees the roster grow to two before it may start.
        host.wait_for(|m| match m {
            ServerMessage::Players { players } if players.len() == 2 => Some(()),
            _ => None,
        })
        .expect("host never saw the guest join");

        host.send(ClientMessage::Start);
        let spawns = host
            .wait_for(|m| match m {
                ServerMessage::Start { spawns, .. } => Some(spawns.clone()),
                _ => None,
            })
            .expect("no `start` broadcast");
        assert_eq!(spawns.len(), 2, "both players get a spawn");

        // One `state` frame, byte-identical to what `broadcast_local_state`
        // produces at 20 Hz.
        let pos = [11.0, 22.0, 33.0];
        let quat = [0.0, 0.0, 0.0, 1.0];
        guest.send(ClientMessage::State {
            pos,
            quat,
            boost: true,
        });

        let relayed = host
            .wait_for(|m| match m {
                ServerMessage::State { id, .. } if *id == guest_id => Some(m.clone()),
                _ => None,
            })
            .expect("the host never received the guest's position");

        let ServerMessage::State {
            id,
            pos: got_pos,
            quat: got_quat,
            boost,
        } = relayed
        else {
            unreachable!()
        };
        assert_eq!(id, guest_id);
        assert_eq!(got_pos, pos, "position survives the round trip exactly");
        assert_eq!(got_quat, quat);
        assert!(boost, "the boost flag is relayed, not dropped");

        guest.send(ClientMessage::Leave);
        guest.socket.close(500);
        host.socket.close(500);
    }

    /// The whole of cross-play, headless: two peers, one live JS server, a
    /// world built from the server's own `start` frame, and a kill.
    ///
    /// This is the test that says the Rust client can play against a browser,
    /// and it says it without a window, a renderer, or a keystroke — the peers
    /// are `Socket`s speaking `crates/protocol`, and the server on the other
    /// end is the unmodified `server/index.js` a browser player is connected
    /// to. Everything it exercises is shipped code:
    ///
    /// - [`build_online_world`] builds the match from the real `start` frame,
    ///   with the real 60-rock field and the real team assignment;
    /// - [`encode_intent`] writes the `hit` frames, byte for byte as the
    ///   in-flight simulation would emit them;
    /// - [`push_events`] reads the server's answers back into the simulation's
    ///   own `NetEvent`s.
    ///
    /// What it proves that a screenshot cannot: that the server *validated*
    /// and *applied* the damage. `hp` and `death` are broadcast by
    /// `server/index.js`, not asserted locally, so a client that merely thinks
    /// it hit something fails here.
    ///
    /// Run it the same way as the other live tests:
    ///
    /// ```text
    /// npm start &                       # or a Rust server on 4100
    /// SPACESHIPS_TEST_SERVER=ws://127.0.0.1:4000/ws \
    ///   cargo test -p spaceships-client -- --ignored --nocapture
    /// ```
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    #[ignore = "needs a live server; see `a_live_server_answers_a_create`"]
    fn a_rust_peer_kills_a_browser_peer_through_the_js_server() {
        let url = std::env::var("SPACESHIPS_TEST_SERVER")
            .unwrap_or_else(|_| "ws://127.0.0.1:4000/ws".to_owned());

        // The room is created with no balance bot, which is the empty-lobby
        // recipe in CLAUDE.md: the only two ships on the map are the peers.
        let mut host = LiveClient::connect(&url, "HostPilot");
        let (code, host_id) = host
            .wait_for(|m| match m {
                ServerMessage::Room { code, you, .. } => Some((code.clone(), *you)),
                _ => None,
            })
            .expect("host got no `room`; is the server running?");

        let mut guest = LiveClient::connect(&url, "RustPilot");
        guest.send(ClientMessage::Leave);
        guest.send(ClientMessage::Join { code });
        let guest_id = guest
            .wait_for(|m| match m {
                ServerMessage::Room { you, .. } => Some(*you),
                _ => None,
            })
            .expect("guest got no `room`");
        host.wait_for(|m| match m {
            ServerMessage::Players { players } if players.len() == 2 => Some(()),
            _ => None,
        })
        .expect("host never saw the guest join");

        host.send(ClientMessage::Start);
        let (spawns, asteroids, map, bots, seed) = guest
            .wait_for(|m| match m {
                ServerMessage::Start {
                    spawns,
                    asteroids,
                    map,
                    bot_assignments,
                    seed,
                } => Some((
                    spawns.clone(),
                    asteroids.clone(),
                    *map,
                    bot_assignments.clone(),
                    *seed,
                )),
                _ => None,
            })
            .expect("the guest never received `start`");

        // The handover, exactly as `ingest` performs it.
        let players = guest
            .wait_for(|m| match m {
                ServerMessage::Players { players } if players.len() == 2 => Some(players.clone()),
                _ => None,
            })
            .expect("no roster");
        let ids = IdSwap::new(Some(guest_id));
        let session = NetSession {
            phase: Phase::Playing,
            you: Some(guest_id),
            ids,
            host: false,
            players,
            callsign: "RustPilot".to_owned(),
            ..NetSession::default()
        };
        // Whatever the server calls this connection, the simulation calls it
        // `LOCAL_ID` — that is the whole point of `IdSwap`, and it is what
        // `input.rs` and `camera.rs` will look for.
        let me = LOCAL_ID;
        let them = ids.to_entity(host_id).expect("a live id fits an EntityId");
        let (world, roster) = build_online_world(
            &spawns,
            &asteroids,
            sim_map(map),
            &bots,
            seed.unwrap_or(ONLINE_SEED),
            &session,
        );

        assert_eq!(world.ships.len(), 2, "two humans, no balance bot");
        assert_eq!(world.local_id, Some(me));
        assert_eq!(world.ship(them).unwrap().kind, ShipKind::Remote);
        assert_ne!(
            world.ship(me).unwrap().team,
            world.ship(them).unwrap().team,
            "two humans are put on opposite teams, or nothing can be shot"
        );
        assert_eq!(roster.callsign(them), "HostPilot");
        // `generateAsteroidField(60, 400)`, transcribed rather than generated.
        assert_eq!(world.asteroids.len(), 60);
        assert!(world.asteroids.iter().all(|a| a.hp > 0 && a.radius > 0.0));

        // Spawn protection: `invulnUntil = Date.now() + 2000`. A hit inside it
        // is rejected, which is the server doing its job and not a failure.
        std::thread::sleep(std::time::Duration::from_millis(2200));

        // Ten bullets at 10 damage each, spaced past the server's 40 ms
        // per-shooter rate limit. Every frame is written by the shipped
        // encoder from the intent the simulation emits.
        let mut hp_seen: Vec<i32> = Vec::new();
        for _ in 0..10 {
            guest.send(encode_intent(
                NetIntent::Hit {
                    target: them,
                    weapon: WeaponKind::Bullet,
                    from_bot: None,
                },
                ids,
            ));
            std::thread::sleep(std::time::Duration::from_millis(60));
            guest.pump(|_| None::<()>);
        }

        // Read the answers back the way the client does, and let them be the
        // only source of truth about the target's hit points.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut died = false;
        while std::time::Instant::now() < deadline {
            guest.pump(|_| None::<()>);
            let mut events = Vec::new();
            for msg in &guest.seen {
                push_events(msg, ids, &mut events);
            }
            hp_seen.clear();
            for event in &events {
                match event {
                    NetEvent::Hp { id, hp } if *id == them => hp_seen.push(*hp),
                    NetEvent::Death { id, killer } if *id == them => {
                        assert_eq!(*killer, Some(me), "the kill is credited to us");
                        died = true;
                    }
                    _ => {}
                }
            }
            if died {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        assert!(
            hp_seen.first().copied() == Some(90),
            "the first accepted hit takes exactly 10 hp, got {hp_seen:?}"
        );
        assert!(
            hp_seen.windows(2).all(|w| w[1] < w[0]),
            "hit points only ever fall: {hp_seen:?}"
        );
        assert!(
            died,
            "ten bullets did not kill: the server reported {hp_seen:?}"
        );

        guest.send(ClientMessage::Leave);
        guest.socket.close(500);
        host.socket.close(500);
    }

    /// A blocking wrapper around [`Socket`] for the two live tests. Test-only:
    /// the game never waits on the network, but a test that did not would be a
    /// sleep-and-hope.
    #[cfg(not(target_arch = "wasm32"))]
    struct LiveClient {
        who: &'static str,
        socket: Socket,
        opened: bool,
        /// Every message this client has decoded. `wait_for` searches the whole
        /// history rather than only new arrivals, because the server
        /// interleaves replies and broadcasts — a `players` broadcast can and
        /// does overtake the `room` reply it was triggered by.
        seen: Vec<ServerMessage>,
    }

    #[cfg(not(target_arch = "wasm32"))]
    impl LiveClient {
        /// Connects, waits for the handshake, and creates a room.
        fn connect(url: &str, name: &'static str) -> Self {
            let mut client = Self {
                who: name,
                socket: Socket::connect(url).expect("thread spawns"),
                opened: false,
                seen: Vec::new(),
            };
            client.pump(|c| c.opened.then_some(()));
            assert!(client.opened, "{name} never opened; is the server running?");
            client.send(ClientMessage::Name {
                name: name.to_owned(),
            });
            client.send(ClientMessage::Create {
                private: false,
                map: spaceships_protocol::MapKind::Space,
                // No balance bot: it would add a third `players` row and a
                // negative id, and this test is about two humans.
                allow_bot: false,
            });
            client
        }

        fn send(&self, msg: ClientMessage) {
            let text = serde_json::to_string(&msg).unwrap();
            println!("{:>5} -> {text}", self.who);
            assert!(self.socket.send(&text), "{}: send failed", self.who);
        }

        /// Polls until `pick` matches something this client has received, or
        /// gives up after five seconds.
        fn wait_for<T>(&mut self, pick: impl Fn(&ServerMessage) -> Option<T>) -> Option<T> {
            self.pump(move |c| c.seen.iter().find_map(&pick))
        }

        fn pump<T>(&mut self, done: impl Fn(&Self) -> Option<T>) -> Option<T> {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut events = Vec::new();
            loop {
                self.socket.drain(&mut events);
                for event in events.drain(..) {
                    match event {
                        SocketEvent::Open => self.opened = true,
                        SocketEvent::Text(text) => {
                            println!("{:>5} <- {text}", self.who);
                            self.seen.push(
                                serde_json::from_str(&text)
                                    .unwrap_or_else(|e| panic!("{e}: {text}")),
                            );
                        }
                        dead => panic!("{}: socket died: {dead:?}", self.who),
                    }
                }
                if let Some(found) = done(self) {
                    return Some(found);
                }
                if std::time::Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
}
