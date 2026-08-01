//! Shared test harness: a real server on an ephemeral port, a minimal HTTP/1.1
//! client, and WebSocket helpers.
//!
//! # Why a hand-rolled HTTP client
//!
//! These tests assert on the **exact bytes** of the response body, because key
//! order and number formatting are part of the contract with the browser client
//! and with the Node server being replaced. A client that hands back a parsed
//! `serde_json::Value` would sort the keys and hide precisely the bugs worth
//! catching. Sixty lines of `TcpStream` gets the raw string and adds no
//! dependency.
//!
//! # The database is always a copy
//!
//! `pilots.db` in the repo root is live. Every test copies it to a unique
//! temporary path first and points `PILOTS_DB` at the copy, so the real file is
//! never opened for writing — which matters because opening it at all runs the
//! achievement backfill, and the backfill writes.

// Each test binary that includes this module uses a different subset of the
// helpers, so anything unused *here* is used next door.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A running server plus the scratch database it owns.
pub struct TestServer {
    /// Where it is listening.
    pub addr: SocketAddr,
    /// Room and connection state, so a test can drive `end_match` directly
    /// instead of waiting out the 300-second match clock.
    pub lobby: std::sync::Arc<spaceships_server::lobby::Lobby>,
    db_path: PathBuf,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.db_path);
    }
}

/// The repo root, derived from this crate's manifest directory.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

/// Boots a server on an ephemeral port against a fresh copy of `pilots.db`.
///
/// Falls back to an empty database when the real file is absent, so the suite
/// still runs on a checkout that has never started the JS server.
pub async fn start() -> TestServer {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let db_path =
        std::env::temp_dir().join(format!("spaceships-test-{}-{}.db", std::process::id(), n));
    let _ = std::fs::remove_file(&db_path);
    let real = repo_root().join("pilots.db");
    if real.exists() {
        std::fs::copy(&real, &db_path).expect("copy pilots.db");
    }

    let config = spaceships_server::Config {
        db_path: db_path.clone(),
        jwt_secret: spaceships_server::auth::DEV_JWT_SECRET.to_string(),
        dist_dir: repo_root().join("dist"),
        public_dir: repo_root().join("public"),
    };
    let built = spaceships_server::build(&config).expect("build server");
    let lobby = std::sync::Arc::clone(&built.lobby);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = spaceships_server::serve(listener, built.router).await;
    });
    // Give the accept loop a moment to be ready.
    tokio::time::sleep(Duration::from_millis(30)).await;
    TestServer {
        addr,
        lobby,
        db_path,
    }
}

/// One HTTP response, kept as raw text.
#[derive(Debug)]
pub struct HttpResponse {
    /// Status code.
    pub status: u16,
    /// Header block, lowercased names, as received.
    pub headers: Vec<(String, String)>,
    /// Body bytes as a UTF-8 string.
    pub body: String,
}

impl HttpResponse {
    /// Parses the body as JSON. Panics with the body text on failure, which is
    /// far more useful in a test than a serde error alone.
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("body is not JSON ({e}): {:?}", self.body))
    }

    /// First value of a header, if present.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Sends one HTTP/1.1 request and reads the whole response.
///
/// Uses `Connection: close` so the body is simply "everything until EOF" — no
/// chunked decoding, no keep-alive bookkeeping.
pub async fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> HttpResponse {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(b) = body {
        req.push_str("Content-Type: application/json\r\n");
        req.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    req.push_str("\r\n");
    if let Some(b) = body {
        req.push_str(b);
    }
    stream.write_all(req.as_bytes()).await.expect("write");
    stream.flush().await.expect("flush");

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let split = text.find("\r\n\r\n").expect("header terminator");
    let head = &text[..split];
    let body = text[split + 4..].to_string();

    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
        .collect();
    HttpResponse {
        status,
        headers,
        body,
    }
}

/// `GET` with an optional bearer token.
pub async fn get(addr: SocketAddr, path: &str, token: Option<&str>) -> HttpResponse {
    let auth = token.map(|t| format!("Bearer {t}"));
    let headers: Vec<(&str, &str)> = match &auth {
        Some(a) => vec![("Authorization", a.as_str())],
        None => vec![],
    };
    request(addr, "GET", path, &headers, None).await
}

/// `POST` a JSON body with an optional bearer token.
pub async fn post(addr: SocketAddr, path: &str, token: Option<&str>, body: &str) -> HttpResponse {
    let auth = token.map(|t| format!("Bearer {t}"));
    let headers: Vec<(&str, &str)> = match &auth {
        Some(a) => vec![("Authorization", a.as_str())],
        None => vec![],
    };
    request(addr, "POST", path, &headers, Some(body)).await
}

/// `PUT` a JSON body with an optional bearer token.
pub async fn put(addr: SocketAddr, path: &str, token: Option<&str>, body: &str) -> HttpResponse {
    let auth = token.map(|t| format!("Bearer {t}"));
    let headers: Vec<(&str, &str)> = match &auth {
        Some(a) => vec![("Authorization", a.as_str())],
        None => vec![],
    };
    request(addr, "PUT", path, &headers, Some(body)).await
}

/// Registers a pilot and logs in, returning `(callsign, token)`.
pub async fn register_and_login(addr: SocketAddr, name: &str) -> (String, String) {
    let body = format!(r#"{{"username":"{name}","password":"testpassword"}}"#);
    let res = post(addr, "/api/register", None, &body).await;
    assert_eq!(res.status, 201, "register failed: {}", res.body);
    let res = post(addr, "/api/login", None, &body).await;
    assert_eq!(res.status, 200, "login failed: {}", res.body);
    let token = res.json()["token"].as_str().expect("token").to_string();
    (name.to_string(), token)
}

/// A connected WebSocket client.
pub struct WsClient {
    inner: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl WsClient {
    /// Opens `/ws`, optionally authenticated.
    pub async fn connect(addr: SocketAddr, token: Option<&str>) -> WsClient {
        let url = match token {
            Some(t) => format!("ws://{addr}/ws?token={t}"),
            None => format!("ws://{addr}/ws"),
        };
        let (inner, _) = connect_async(&url).await.expect("ws connect");
        WsClient { inner }
    }

    /// Sends a raw JSON string, exactly as the browser client would.
    pub async fn send(&mut self, json: &str) {
        self.inner
            .send(Message::Text(json.into()))
            .await
            .expect("ws send");
    }

    /// Next text frame, as its raw string, or `None` on timeout.
    pub async fn next_raw(&mut self, ms: u64) -> Option<String> {
        let fut = async {
            while let Some(msg) = self.inner.next().await {
                match msg {
                    Ok(Message::Text(t)) => return Some(t.to_string()),
                    Ok(Message::Close(_)) | Err(_) => return None,
                    Ok(_) => continue,
                }
            }
            None
        };
        tokio::time::timeout(Duration::from_millis(ms), fut)
            .await
            .ok()
            .flatten()
    }

    /// Waits for the next frame whose `type` is `tag`, returning it raw.
    ///
    /// Frames of other types are consumed and discarded, which is what makes
    /// assertions robust against the `players` broadcasts that interleave with
    /// everything.
    pub async fn expect(&mut self, tag: &str, ms: u64) -> String {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
        let mut seen: Vec<String> = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                panic!("timed out waiting for {tag:?}; saw: {seen:?}");
            }
            match self.next_raw(left.as_millis() as u64).await {
                Some(raw) => {
                    let v: Value = serde_json::from_str(&raw).expect("server sent invalid JSON");
                    if v["type"] == tag {
                        return raw;
                    }
                    seen.push(v["type"].as_str().unwrap_or("?").to_string());
                }
                None => panic!("socket closed or timed out waiting for {tag:?}; saw: {seen:?}"),
            }
        }
    }

    /// Asserts nothing of type `tag` arrives within `ms`.
    pub async fn expect_none(&mut self, tag: &str, ms: u64) {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return;
            }
            match self.next_raw(left.as_millis() as u64).await {
                Some(raw) => {
                    let v: Value = serde_json::from_str(&raw).expect("invalid JSON");
                    assert_ne!(v["type"], tag, "unexpected {tag:?} frame: {raw}");
                }
                None => return,
            }
        }
    }

    /// Closes the socket, which drives the server's `close` handler.
    pub async fn close(mut self) {
        let _ = self.inner.close(None).await;
    }
}
