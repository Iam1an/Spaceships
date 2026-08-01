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
use spaceships_protocol::{ClientMessage, PlayerId, ServerMessage};

use crate::sim_bridge::SimFrame;
use spaceships_sim::world::ShipFlags;

// ─────────────────────────────────────────────────────────────────────────────
// Tuning
// ─────────────────────────────────────────────────────────────────────────────

/// Seconds between `state` broadcasts.
///
/// `STATE_INTERVAL = 1 / 20` in `public/src/main.js:978`. The simulation ticks
/// faster; this is the wire rate and is deliberately not tied to it.
const STATE_INTERVAL: f32 = 1.0 / 20.0;

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

/// Where a native build connects when `SPACESHIPS_SERVER` is unset.
#[cfg(not(target_arch = "wasm32"))]
const DEFAULT_URL: &str = "ws://127.0.0.1:4000/ws";

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
            // rather than last frame's.
            .add_systems(
                PreUpdate,
                (run_commands, service).chain().in_set(NetSet::Receive),
            )
            .add_systems(Update, broadcast_local_state)
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
    /// Connect at startup without waiting for [`NetCommand::Connect`].
    pub auto_connect: bool,
    /// Retry with [`RETRY_BACKOFF`] after an unrequested close.
    pub reconnect: bool,
}

impl Default for NetConfig {
    fn default() -> Self {
        let (url, token) = default_endpoint();
        Self {
            // Auto-connecting only when the endpoint was named explicitly. The
            // client has no lobby yet, so the alternative is every `cargo run`
            // logging a connection refusal to a server nobody started.
            auto_connect: endpoint_was_explicit(),
            url,
            token,
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
) {
    for ToServer(msg) in outbox.read() {
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

/// Broadcasts the local ship's position at [`STATE_INTERVAL`].
///
/// Reads [`SimFrame`], which is the same flat view the renderer reads — the
/// network never sees `sim::World`.
fn broadcast_local_state(
    time: Res<Time>,
    frame: Res<SimFrame>,
    status: Res<NetStatus>,
    mut acc: Local<f32>,
    mut outbox: MessageWriter<ToServer>,
) {
    if !status.is_online() {
        // Hold the accumulator at zero so the first frame after reconnecting
        // does not fire immediately with a stale position.
        *acc = 0.0;
        return;
    }
    if !advance_cadence(&mut acc, time.delta_secs(), STATE_INTERVAL) {
        return;
    }

    // `myAlive` in main.js gates the same send.
    let Some(me) = frame
        .0
        .ships
        .iter()
        .find(|s| s.flags.contains(ShipFlags::LOCAL) && s.flags.contains(ShipFlags::ALIVE))
    else {
        return;
    };

    outbox.write(ToServer(ClientMessage::State {
        pos: [me.pos[0] as f64, me.pos[1] as f64, me.pos[2] as f64],
        quat: [
            me.quat[0] as f64,
            me.quat[1] as f64,
            me.quat[2] as f64,
            me.quat[3] as f64,
        ],
        boost: me.flags.contains(ShipFlags::BOOSTING),
    }));
}

/// One step of the JS send timer.
///
/// `main.js` writes `stateTimer += dt; if (stateTimer >= INTERVAL) { stateTimer
/// = 0; send(); }` — note the reset to **zero**, not `-= INTERVAL`. That makes
/// the real rate `1 / (ceil(INTERVAL / dt) * dt)`, i.e. at or just below 20 Hz
/// and frame-rate dependent (20 Hz at 60 fps, ~18 Hz at 144 fps). Reproduced
/// rather than corrected: the brief is to keep the existing cadence, and the
/// drift-free form would be a behaviour change on the wire. Subtracting
/// `interval` instead is the one-character fix if that is ever wanted.
fn advance_cadence(acc: &mut f32, dt: f32, interval: f32) -> bool {
    *acc += dt;
    if *acc >= interval {
        *acc = 0.0;
        true
    } else {
        false
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
    let url = std::env::var("SPACESHIPS_SERVER").unwrap_or_else(|_| DEFAULT_URL.to_owned());
    let token = std::env::var("SPACESHIPS_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());
    (url, token)
}

/// Whether the operator named a server, which is what native auto-connect keys
/// off.
#[cfg(not(target_arch = "wasm32"))]
fn endpoint_was_explicit() -> bool {
    std::env::var_os("SPACESHIPS_SERVER").is_some()
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

/// The page always names the server, so the browser build always auto-connects
/// once something asks it to. Kept `false` because the lobby has not been
/// ported yet and there is nothing to do with an open socket.
#[cfg(target_arch = "wasm32")]
fn endpoint_was_explicit() -> bool {
    false
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

    /// The rate the JS actually achieves, not the one it nominally asks for.
    #[test]
    fn the_state_cadence_is_20_hz_at_60_fps() {
        let mut acc = 0.0f32;
        let sends = (0..60)
            .filter(|_| advance_cadence(&mut acc, 1.0 / 60.0, STATE_INTERVAL))
            .count();
        assert_eq!(sends, 20);
    }

    /// Reset-to-zero (which is what `main.js` does) makes the rate sag below
    /// 20 Hz as the frame rate rises. Pinned so that changing it is a decision
    /// rather than an accident.
    #[test]
    fn a_faster_frame_rate_sends_no_more_often() {
        for fps in [60.0f32, 120.0, 144.0, 240.0] {
            let mut acc = 0.0f32;
            let sends = (0..fps as u32)
                .filter(|_| advance_cadence(&mut acc, 1.0 / fps, STATE_INTERVAL))
                .count();
            assert!(
                (15..=20).contains(&sends),
                "{fps} fps produced {sends} sends/s, expected 15..=20"
            );
        }
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
    /// [`the_state_cadence_is_20_hz_at_60_fps`] proves the timer. This is the
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
