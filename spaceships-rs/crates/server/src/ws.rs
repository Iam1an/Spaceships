//! The WebSocket endpoint.
//!
//! # ~200 lines that no longer exist
//!
//! `server/index.js:203-407` hand-rolls RFC 6455: the SHA-1-of-key-plus-magic-
//! GUID handshake, a frame parser with the three length encodings, unmasking,
//! a 1 MiB payload cap, ping/pong, and close handling. All of it is replaced by
//! [`axum::extract::ws::WebSocketUpgrade`], which is `tokio-tungstenite` under
//! the hood — axum's `ws` feature is literally `dep:tokio-tungstenite`.
//!
//! Two workarounds in the JS are deliberately **not** ported, because they were
//! compensating for Node bugs rather than implementing the protocol:
//!
//! - The **frame dedup cache**: a two-tier rotating `Set` of 4-byte frame
//!   masks, added because a Node 25 upgrade regression re-delivers frame bytes
//!   on later `data` events. Keeping it here would silently drop legitimate
//!   duplicate frames — two identical `state` updates in a row are normal
//!   traffic — so it is gone.
//! - The **HTTP-prefix skip** in `parse()`, which searches the frame buffer for
//!   `\r\n\r\n` when it looks like an ASCII request method, because the same
//!   regression sometimes prepends the original HTTP request to the first
//!   frame.
//!
//! # Auth degrades, never rejects
//!
//! Clients append `?token=<jwt>`. An absent, malformed, or expired token means
//! *guest*, not a refused upgrade — the JS wraps `verifyToken` in a bare
//! `try/catch` and carries on with `pilotId = null`. Guests can play; they just
//! get no stats and no `match-credits` payout.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::http::AppState;

/// `?token=<jwt>` on the upgrade request.
#[derive(Debug, Deserialize)]
pub struct WsQuery {
    /// Optional JWT. Anything unparseable is treated as absent.
    pub token: Option<String>,
}

/// `GET /ws` — the upgrade handler.
pub async fn ws_route(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
) -> Response {
    let (pilot_id, pilot_username) = match query.token.as_deref() {
        Some(token) if !token.is_empty() => match state.auth.verify(token) {
            Ok(claims) => (Some(claims.id), Some(claims.username)),
            // Expired or tampered — treat as guest.
            Err(_) => (None, None),
        },
        _ => (None, None),
    };
    ws.on_upgrade(move |socket| handle_socket(socket, state, pilot_id, pilot_username))
}

/// Drives one connection for its whole life.
///
/// The socket is split so that broadcasts (which arrive from other
/// connections' tasks, and from the match timer) never block on the read side.
/// The unbounded channel is the direct analogue of the JS's fire-and-forget
/// `socket.write` — the lobby only ever hands it already-serialized JSON, and a
/// send to a closed channel is dropped the same way `if (c.open)` drops one to
/// a dead socket.
async fn handle_socket(
    socket: WebSocket,
    state: AppState,
    pilot_id: Option<i64>,
    pilot_username: Option<String>,
) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    let id = state.lobby.connect(tx, pilot_id, pilot_username);

    let writer = tokio::spawn(async move {
        while let Some(text) = rx.recv().await {
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
        // Best-effort close; the peer may already be gone.
        let _ = sink.close().await;
    });

    while let Some(frame) = stream.next().await {
        match frame {
            // The JS handles opcodes 0x1 and 0x2 identically, decoding both as
            // UTF-8 text.
            Ok(Message::Text(text)) => state.lobby.on_message(id, &text).await,
            Ok(Message::Binary(bytes)) => {
                if let Ok(text) = std::str::from_utf8(&bytes) {
                    state.lobby.on_message(id, text).await;
                }
            }
            Ok(Message::Close(_)) => break,
            // Ping/pong are answered by the tungstenite layer.
            Ok(_) => {}
            Err(_) => break,
        }
    }

    state.lobby.disconnect(id);
    writer.abort();
}
