//! Rooms, matchmaking, the match clock, and message relay.
//!
//! This is the port of `server/index.js` lines 401–1005: the `rooms` map, the
//! room helpers (`genCode`, `roomSnapshot`, `broadcast`, `broadcastRoom`,
//! `leaveRoom`, `endMatch`, `spawnForTeam`), and every `msg.type` arm of
//! `handleConnection`.
//!
//! # Ordering is load-bearing
//!
//! The JS uses `Map` and `Set`, which iterate in **insertion order**, and three
//! behaviours depend on it:
//!
//! - Team assignment at match start walks `room.players` in insertion order and
//!   alternates `humanIdx++ % 2`, so who lands on which team is a function of
//!   join order.
//! - Host migration picks the *first* remaining non-bot player.
//! - `players` and `rooms-list` are rendered in the order they arrive.
//!
//! A `HashMap` would scramble all three, so players, sockets, and rooms are all
//! `Vec`s here. They are tiny — a room holds a handful of players — so linear
//! lookup costs nothing.
//!
//! # What is authoritative and what is not
//!
//! Almost nothing. The server owns hit points, kills, deaths, team assignment,
//! spawn points, the asteroid field, the match clock, and respawn timing. It
//! does **not** own whether a shot connected: a client asserts "I hit player 7
//! with a missile" and the server believes it, subject only to the sanity
//! filters in [`Lobby::on_hit`]. See that function's docs for exactly which
//! checks exist and which do not.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use spaceships_protocol::consts::{
    ASTEROID_HIT_DAMAGE, GUN_DAMAGE, GUN_HIT_MIN_INTERVAL_MS, MATCH_DURATION_MS, MAX_NAME_LEN,
    MISSILE_DAMAGE, MISSILE_HIT_MIN_INTERVAL_MS, RESPAWN_DELAY_MS, ROOM_CODE_LEN, SHIP_MAX_HP,
    SPAWN_INVULN_MS,
};
use spaceships_protocol::{
    Asteroid, BotAssignment, ClientMessage, MapKind, PlayerId, PlayerInfo, Quat, RoomSummary,
    ServerMessage, Spawn, Vec3, WeaponKind,
};
use spaceships_sim::rng::Rng;

use crate::db::Db;

// ─────────────────────────────────────────────────────────────────────────────
// Spawn geometry
// ─────────────────────────────────────────────────────────────────────────────

/// Team spawn anchors, per map.
///
/// Only the *facing* lives here. The offsets — how far out from the structure,
/// how high, and how wide the scatter is — come from
/// [`spaceships_sim::rules::SpawnRules`], which is the single definition the
/// client's own spawn path reads too.
///
/// **This used to be a second copy.** The z, y and jitter figures were written
/// out here as literals, with a comment explaining that `sim` exposed only
/// `mothership_z` and `airfield_z` and therefore could not be asked. It exposed
/// all of it, in `SpawnRules`, and the copies drifted the moment the terrain
/// map's airfields moved onto mesas: the server would have kept spawning ships
/// at `y = 40`, 170 units inside a hill. Duplicated rules are the failure mode
/// `crates/sim/src/rules.rs` exists to prevent, so this reads them.
const TEAM_FACING: [Quat; 2] = [[0.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 0.0]];

/// A spawn position and orientation.
#[derive(Debug, Clone, Copy)]
pub struct SpawnPoint {
    /// World position.
    pub pos: Vec3,
    /// World orientation.
    pub quat: Quat,
}

/// `spawnForTeam` — a jittered point in front of the team's mothership or above
/// its airfield.
///
/// The three jitter draws happen in `x, y, z` order, matching the JS argument
/// evaluation order, so a seeded run reproduces exactly.
fn spawn_for_team(rng: &mut Rng, team: u8, map: MapKind) -> SpawnPoint {
    let idx = usize::from(team.min(1));
    let sign = if idx == 0 { -1.0 } else { 1.0 };
    let spawn = &spaceships_sim::rules::Rules::DEFAULT.spawn;
    let (z, y, jitter) = match map {
        MapKind::Terrain => (spawn.terrain_z, spawn.terrain_y, spawn.terrain_jitter),
        MapKind::Space => (spawn.space_z, spawn.space_y, spawn.space_jitter),
    };
    SpawnPoint {
        pos: [
            (rng.next_f64() - 0.5) * jitter.x,
            y + (rng.next_f64() - 0.5) * jitter.y,
            sign * z + (rng.next_f64() - 0.5) * jitter.z,
        ],
        quat: TEAM_FACING[idx],
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// State
// ─────────────────────────────────────────────────────────────────────────────

/// One player in a room — human or balance bot.
#[derive(Debug, Clone)]
pub struct Player {
    /// Connection id, or a negative synthetic id for bots.
    pub id: PlayerId,
    /// Display name.
    pub name: String,
    /// Current hit points.
    pub hp: i32,
    /// Whether the ship is alive (dead ships are awaiting respawn).
    pub alive: bool,
    /// Kills this match.
    pub kills: u32,
    /// Deaths this match.
    pub deaths: u32,
    /// Team, assigned at match start. `None` in the lobby.
    pub team: Option<u8>,
    /// Whether this is a server-spawned balance bot.
    pub is_bot: bool,
    /// End of spawn protection.
    pub invuln_until: Option<Instant>,
}

impl Player {
    fn new(id: PlayerId, name: String) -> Player {
        Player {
            id,
            name,
            hp: SHIP_MAX_HP,
            alive: true,
            kills: 0,
            deaths: 0,
            team: None,
            is_bot: false,
            invuln_until: None,
        }
    }
}

/// A room: a lobby before `start`, a match afterwards.
pub struct Room {
    /// Four uppercase letters.
    pub code: String,
    /// Host connection id.
    pub host_id: PlayerId,
    /// Hidden from `list-rooms`.
    pub is_private: bool,
    /// Map selection.
    pub map: MapKind,
    /// Whether the server may add a balance bot at start.
    pub allow_bot: bool,
    /// Connections in the room, in join order. Bots are not here.
    pub sockets: Vec<PlayerId>,
    /// Players in the room, in join order. Bots *are* here.
    pub players: Vec<Player>,
    /// Whether the match has begun.
    pub started: bool,
    /// The generated asteroid field, with server-side hit points.
    pub asteroids: Vec<Asteroid>,
    /// Seed the field was generated from.
    pub asteroid_seed: u64,
    /// Whether the match has ended.
    pub match_over: bool,
    /// Kills per team.
    pub team_kills: [u32; 2],
    /// When the match clock runs out.
    pub match_end: Option<Instant>,
    /// Who drives the balance bots.
    pub bot_host_id: Option<PlayerId>,
    /// The 5-minute end timer and the 1 Hz tick, so they can be cancelled.
    timers: Vec<JoinHandle<()>>,
}

impl Room {
    fn player(&self, id: PlayerId) -> Option<&Player> {
        self.players.iter().find(|p| p.id == id)
    }

    fn player_mut(&mut self, id: PlayerId) -> Option<&mut Player> {
        self.players.iter_mut().find(|p| p.id == id)
    }

    /// `roomSnapshot`.
    fn snapshot(&self) -> Vec<PlayerInfo> {
        self.players
            .iter()
            .map(|p| PlayerInfo {
                id: p.id,
                name: p.name.clone(),
                host: p.id == self.host_id,
                team: p.team,
                is_bot: p.is_bot,
                kills: p.kills,
                deaths: p.deaths,
            })
            .collect()
    }

    fn cancel_timers(&mut self) {
        for t in self.timers.drain(..) {
            t.abort();
        }
    }
}

/// A live WebSocket connection.
pub struct Conn {
    /// Connection id, `nextId++`.
    pub id: PlayerId,
    /// Outbound queue; the writer task drains it into the socket.
    pub tx: UnboundedSender<String>,
    /// Current display name.
    pub name: String,
    /// Authenticated pilot row id, or `None` for a guest.
    pub pilot_id: Option<i64>,
    /// Room code this connection is in.
    pub room: Option<String>,
    /// `ws.hitTimes` — last accepted hit per (effective shooter, weapon).
    pub hit_times: HashMap<(PlayerId, WeaponKind), Instant>,
}

struct Inner {
    rooms: Vec<Room>,
    conns: HashMap<PlayerId, Conn>,
    next_id: PlayerId,
    rng: Rng,
}

impl Inner {
    fn room(&self, code: &str) -> Option<&Room> {
        self.rooms.iter().find(|r| r.code == code)
    }

    fn room_mut(&mut self, code: &str) -> Option<&mut Room> {
        self.rooms.iter_mut().find(|r| r.code == code)
    }

    /// `genCode` — four uppercase letters, retried until unused.
    fn gen_code(&mut self) -> String {
        loop {
            let code: String = (0..ROOM_CODE_LEN)
                .map(|_| (b'A' + u8::try_from(self.rng.bounded_u32(26)).unwrap_or(0)) as char)
                .collect();
            if !self.rooms.iter().any(|r| r.code == code) {
                return code;
            }
        }
    }

    fn send(&self, id: PlayerId, msg: &str) {
        if let Some(c) = self.conns.get(&id) {
            // A closed receiver just means the writer task already exited;
            // the JS equivalent is the `if (c.open)` guard.
            let _ = c.tx.send(msg.to_string());
        }
    }

    /// `broadcast` — to every socket in the room, sender included.
    fn broadcast(&self, code: &str, msg: &ServerMessage) {
        let Some(room) = self.room(code) else { return };
        let Ok(text) = serde_json::to_string(msg) else {
            return;
        };
        for id in &room.sockets {
            self.send(*id, &text);
        }
    }

    /// Relay to every socket in the room **except** `from`, which is how the
    /// JS handles `state`, `fire`, `flare`, `colors`, `ship-model`,
    /// `bot-state`, and `bot-fire` — the sender already drew its own action
    /// locally.
    fn relay(&self, code: &str, from: PlayerId, msg: &ServerMessage) {
        let Some(room) = self.room(code) else { return };
        let Ok(text) = serde_json::to_string(msg) else {
            return;
        };
        for id in &room.sockets {
            if *id != from {
                self.send(*id, &text);
            }
        }
    }

    /// `broadcastRoom`.
    fn broadcast_room(&self, code: &str) {
        let Some(room) = self.room(code) else { return };
        let msg = ServerMessage::Players {
            players: room.snapshot(),
        };
        self.broadcast(code, &msg);
    }
}

/// The whole lobby: rooms, connections, and the id counter.
pub struct Lobby {
    inner: Mutex<Inner>,
    db: Arc<Db>,
}

impl Lobby {
    /// Builds an empty lobby.
    ///
    /// The RNG is seeded from the wall clock. It drives room codes, spawn
    /// jitter, and the per-match asteroid seed — the same jobs `Math.random()`
    /// does in the JS, and like `Math.random()` it is not used for anything
    /// security-sensitive.
    #[must_use]
    pub fn new(db: Arc<Db>) -> Lobby {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x5EED_1234_5678_9ABC);
        Lobby {
            inner: Mutex::new(Inner {
                rooms: Vec::new(),
                conns: HashMap::new(),
                next_id: 1,
                rng: Rng::new(seed),
            }),
            db,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Registers a new connection and returns its id.
    ///
    /// The default name is the authenticated pilot's callsign, or
    /// `Player <id>` for a guest — `ws.pilotUsername || ('Player ' + ws.id)`.
    pub fn connect(
        &self,
        tx: UnboundedSender<String>,
        pilot_id: Option<i64>,
        pilot_username: Option<String>,
    ) -> PlayerId {
        let mut inner = self.lock();
        let id = inner.next_id;
        inner.next_id += 1;
        let name = pilot_username.unwrap_or_else(|| format!("Player {id}"));
        inner.conns.insert(
            id,
            Conn {
                id,
                tx,
                name,
                pilot_id,
                room: None,
                hit_times: HashMap::new(),
            },
        );
        id
    }

    /// The `close` handler: leave the room, then tell the survivors — but only
    /// if the match had actually started.
    pub fn disconnect(&self, id: PlayerId) {
        let mut inner = self.lock();
        let code = inner.conns.get(&id).and_then(|c| c.room.clone());
        let was_started = code
            .as_deref()
            .and_then(|c| inner.room(c))
            .is_some_and(|r| r.started);
        leave_room(&mut inner, id);
        if was_started {
            if let Some(code) = code {
                // The room may have just been torn down; `broadcast` is a no-op
                // then, which is also what the JS does (its socket set is
                // empty by that point).
                inner.broadcast(&code, &ServerMessage::Disconnect { id });
            }
        }
        inner.conns.remove(&id);
    }

    /// Number of live rooms, for tests and the status line.
    #[must_use]
    pub fn room_count(&self) -> usize {
        self.lock().rooms.len()
    }

    // ── Dispatch ────────────────────────────────────────────────────────────

    /// One inbound frame.
    ///
    /// Unparseable JSON and unknown `type` tags are dropped silently, exactly
    /// as the JS `try { msg = JSON.parse(data) } catch { return }` plus the
    /// fall-through at the end of the `if` chain do.
    pub async fn on_message(self: &Arc<Self>, id: PlayerId, raw: &str) {
        let Some(msg) = crate::wire::parse_client_message(raw) else {
            return;
        };
        match msg {
            ClientMessage::Name { name } => self.on_name(id, &name),
            ClientMessage::ListRooms => self.on_list_rooms(id),
            ClientMessage::Create {
                private,
                map,
                allow_bot,
            } => self.on_create(id, private, map, allow_bot),
            ClientMessage::Join { code } => self.on_join(id, &code),
            ClientMessage::Start => self.on_start(id),
            ClientMessage::Leave => {
                let mut inner = self.lock();
                leave_room(&mut inner, id);
            }
            ClientMessage::State { pos, quat, boost } => self.on_state(id, pos, quat, boost),
            ClientMessage::Fire { kind, shots } => {
                self.relay_if_alive(id, ServerMessage::Fire { id, kind, shots });
            }
            ClientMessage::Flare { pos, quat } => {
                self.relay_if_alive(id, ServerMessage::Flare { id, pos, quat });
            }
            ClientMessage::Hit {
                target_id,
                kind,
                from_bot_id,
            } => self.on_hit(id, target_id, kind, from_bot_id),
            ClientMessage::SelfDamage { dmg } => self.on_self_damage(id, dmg),
            ClientMessage::Colors {
                hull_color,
                accent_color,
            } => self.on_colors(id, hull_color, accent_color),
            ClientMessage::ShipModel { model_url } => self.on_ship_model(id, &model_url),
            ClientMessage::AsteroidHit { id: rock } => self.on_asteroid_hit(id, rock),
            ClientMessage::BotState { bot_id, pos, quat } => {
                self.on_bot_state(id, bot_id, pos, quat);
            }
            ClientMessage::BotFire {
                bot_id,
                kind,
                shots,
            } => self.on_bot_fire(id, bot_id, kind, shots),
        }
    }

    // ── Lobby ───────────────────────────────────────────────────────────────

    /// `name` — sanitize to `[A-Za-z0-9 _-]`, trim, truncate to 16. An empty
    /// result is discarded and the previous name kept.
    fn on_name(&self, id: PlayerId, raw: &str) {
        let cleaned = sanitize_name(raw);
        if cleaned.is_empty() {
            return;
        }
        let mut inner = self.lock();
        let Some(conn) = inner.conns.get_mut(&id) else {
            return;
        };
        conn.name = cleaned.clone();
        let Some(code) = conn.room.clone() else {
            return;
        };
        if let Some(room) = inner.room_mut(&code) {
            if let Some(p) = room.player_mut(id) {
                p.name = cleaned;
            }
        }
        inner.broadcast_room(&code);
    }

    /// `list-rooms` — public, not-yet-started rooms only, with humans counted
    /// separately from bots.
    fn on_list_rooms(&self, id: PlayerId) {
        let inner = self.lock();
        let rooms = inner
            .rooms
            .iter()
            .filter(|r| !r.is_private && !r.started)
            .map(|r| RoomSummary {
                code: r.code.clone(),
                player_count: r.players.iter().filter(|p| !p.is_bot).count() as u32,
                host_name: r
                    .player(r.host_id)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| "Unknown".to_string()),
            })
            .collect();
        let Ok(text) = serde_json::to_string(&ServerMessage::RoomsList { rooms }) else {
            return;
        };
        inner.send(id, &text);
    }

    fn on_create(&self, id: PlayerId, private: bool, map: MapKind, allow_bot: bool) {
        let mut inner = self.lock();
        if inner.conns.get(&id).and_then(|c| c.room.as_ref()).is_some() {
            leave_room(&mut inner, id);
        }
        let code = inner.gen_code();
        let name = inner
            .conns
            .get(&id)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        inner.rooms.push(Room {
            code: code.clone(),
            host_id: id,
            is_private: private,
            map,
            allow_bot,
            sockets: vec![id],
            players: vec![Player::new(id, name)],
            started: false,
            asteroids: Vec::new(),
            asteroid_seed: 0,
            match_over: false,
            team_kills: [0, 0],
            match_end: None,
            bot_host_id: None,
            timers: Vec::new(),
        });
        if let Some(c) = inner.conns.get_mut(&id) {
            c.room = Some(code.clone());
        }
        let ack = ServerMessage::Room {
            code: code.clone(),
            host: true,
            you: id,
            private,
        };
        if let Ok(text) = serde_json::to_string(&ack) {
            inner.send(id, &text);
        }
        inner.broadcast_room(&code);
    }

    fn on_join(&self, id: PlayerId, raw_code: &str) {
        let mut inner = self.lock();
        let code = raw_code.to_uppercase();
        let (exists, started, is_private) = match inner.room(&code) {
            Some(r) => (true, r.started, r.is_private),
            None => (false, false, false),
        };
        if !exists {
            self::send_error(&inner, id, "Room not found");
            return;
        }
        if started {
            self::send_error(&inner, id, "Game already started");
            return;
        }
        if inner.conns.get(&id).and_then(|c| c.room.as_ref()).is_some() {
            leave_room(&mut inner, id);
        }
        let name = inner
            .conns
            .get(&id)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        if let Some(room) = inner.room_mut(&code) {
            room.sockets.push(id);
            room.players.push(Player::new(id, name));
        }
        if let Some(c) = inner.conns.get_mut(&id) {
            c.room = Some(code.clone());
        }
        let ack = ServerMessage::Room {
            code: code.clone(),
            host: false,
            you: id,
            private: is_private,
        };
        if let Ok(text) = serde_json::to_string(&ack) {
            inner.send(id, &text);
        }
        inner.broadcast_room(&code);
    }

    // ── Match start ─────────────────────────────────────────────────────────

    /// `start` — host only.
    ///
    /// Assigns teams, spawns, an optional balance bot, and the asteroid field,
    /// then arms the match clock.
    ///
    /// # One deliberate divergence
    ///
    /// The JS does not guard against a second `start`. Pressing it twice
    /// installs a fresh `setTimeout` *without* clearing the first, and also
    /// resets `matchOver` to `false` — so the original 5-minute timer fires
    /// against the restarted match and ends it early. Here the previous timers
    /// are aborted first. A restart is still allowed; it just no longer carries
    /// a stale end timer.
    fn on_start(self: &Arc<Self>, id: PlayerId) {
        let mut inner = self.lock();
        let Some(code) = inner.conns.get(&id).and_then(|c| c.room.clone()) else {
            return;
        };
        {
            let Some(room) = inner.room(&code) else {
                return;
            };
            if room.host_id != id {
                return;
            }
        }

        let now = Instant::now();
        let invuln = now + Duration::from_millis(SPAWN_INVULN_MS);
        let mut spawns: std::collections::BTreeMap<PlayerId, Spawn> = Default::default();
        let mut bot_assignments: Vec<BotAssignment> = Vec::new();

        // Destructure once: spawn jitter needs the RNG and bot ids need the id
        // counter, both of which live beside `rooms` in `Inner`.
        let Inner {
            rooms,
            rng,
            next_id,
            ..
        } = &mut *inner;
        let Some(room) = rooms.iter_mut().find(|r| r.code == code) else {
            return;
        };
        room.cancel_timers();
        room.started = true;

        let map = room.map;
        let mut human_idx: u32 = 0;
        for p in room.players.iter_mut() {
            p.hp = SHIP_MAX_HP;
            p.alive = true;
            // `p.team === undefined` — a team assigned by a previous match on
            // this room survives, and only the counter advances.
            match p.team {
                None => {
                    p.team = Some((human_idx % 2) as u8);
                    human_idx += 1;
                }
                Some(_) => human_idx += 1,
            }
            p.kills = 0;
            p.deaths = 0;
            p.invuln_until = Some(invuln);
            let team = p.team.unwrap_or(0);
            let sp = spawn_for_team(rng, team, map);
            spawns.insert(
                p.id,
                Spawn {
                    team,
                    pos: sp.pos,
                    quat: sp.quat,
                },
            );
        }

        // Balance teams with one hard bot on the smaller side.
        let team0 = room.players.iter().filter(|p| p.team == Some(0)).count();
        let team1 = room.players.iter().filter(|p| p.team == Some(1)).count();
        if room.allow_bot && team0 != team1 {
            let smaller: u8 = if team0 < team1 { 0 } else { 1 };
            // Bot ids come out of the same counter as connection ids, negated.
            let bot_id = -*next_id;
            *next_id += 1;
            let sp = spawn_for_team(rng, smaller, map);
            let mut bot = Player::new(bot_id, "Bot [Hard]".to_string());
            bot.team = Some(smaller);
            bot.is_bot = true;
            bot.invuln_until = Some(invuln);
            room.players.push(bot);
            spawns.insert(
                bot_id,
                Spawn {
                    team: smaller,
                    pos: sp.pos,
                    quat: sp.quat,
                },
            );
            room.bot_host_id = Some(id);
            bot_assignments.push(BotAssignment {
                id: bot_id,
                team: smaller,
                pos: sp.pos,
                quat: sp.quat,
            });
        }

        let seed = rng.next_u64();
        room.asteroid_seed = seed;
        room.asteroids = crate::field::generate(seed, map);
        room.match_over = false;
        room.team_kills = [0, 0];
        room.match_end = Some(now + Duration::from_millis(MATCH_DURATION_MS));

        // `asteroids` is still sent because the shipped JS client has no
        // generator and reads the array directly. `seed` is additive: a client
        // built on `spaceships-sim` can regenerate the identical field from it
        // and ignore the records. Dropping the array — this frame is 16,399
        // bytes, essentially all of it those sixty records, and it lands while
        // assets are still loading — needs the client to signal that it can,
        // which is a capability handshake this protocol does not have yet.
        let start_msg = ServerMessage::Start {
            spawns,
            asteroids: room.asteroids.clone(),
            seed: Some(seed),
            map,
            bot_assignments,
        };

        // Arm the clock. Both tasks are stored so a restart or teardown can
        // abort them; the JS leaks the old ones instead.
        let end_lobby = Arc::clone(self);
        let end_code = code.clone();
        let end_timer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(MATCH_DURATION_MS)).await;
            end_lobby.end_match(&end_code).await;
        });
        let tick_lobby = Arc::clone(self);
        let tick_code = code.clone();
        let tick_timer = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(1));
            // The JS `setInterval` fires its first callback after the full
            // period, not immediately.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if !tick_lobby.tick_match(&tick_code) {
                    // The room is gone. `setInterval` would keep firing against
                    // a dead room; stopping is equivalent and cheaper.
                    return;
                }
            }
        });
        room.timers.push(end_timer);
        room.timers.push(tick_timer);

        inner.broadcast(&code, &start_msg);
        inner.broadcast_room(&code);
    }

    /// One 1 Hz `match-state`. Returns whether the room still exists.
    fn tick_match(&self, code: &str) -> bool {
        let inner = self.lock();
        let Some(room) = inner.room(code) else {
            return false;
        };
        // `if (!room.started || room.matchOver) return;` — the interval keeps
        // running but emits nothing.
        if !room.started || room.match_over {
            return true;
        }
        let remaining = room
            .match_end
            .map(|end| end.saturating_duration_since(Instant::now()).as_secs_f64())
            .unwrap_or(0.0);
        let msg = ServerMessage::MatchState {
            timer: remaining,
            team_kills: room.team_kills,
        };
        inner.broadcast(code, &msg);
        true
    }

    /// `endMatch` — announce the result, then persist stats for every
    /// authenticated human and pay them out.
    pub async fn end_match(&self, code: &str) {
        struct Payout {
            conn_id: PlayerId,
            pilot_id: i64,
            kills: i64,
            deaths: i64,
            won: Option<bool>,
        }

        let payouts: Vec<Payout> = {
            let mut inner = self.lock();
            let Some(room) = inner.room_mut(code) else {
                return;
            };
            if room.match_over {
                return;
            }
            room.match_over = true;
            room.cancel_timers();
            let [k0, k1] = room.team_kills;
            let winner: i8 = match k0.cmp(&k1) {
                std::cmp::Ordering::Greater => 0,
                std::cmp::Ordering::Less => 1,
                std::cmp::Ordering::Equal => -1,
            };
            let team_kills = room.team_kills;
            let sockets = room.sockets.clone();
            let players: Vec<Player> = room.players.clone();
            inner.broadcast(code, &ServerMessage::MatchEnd { winner, team_kills });

            sockets
                .iter()
                .filter_map(|sid| {
                    let pilot_id = inner.conns.get(sid).and_then(|c| c.pilot_id)?;
                    let player = players.iter().find(|p| p.id == *sid)?;
                    if player.is_bot {
                        return None;
                    }
                    Some(Payout {
                        conn_id: *sid,
                        pilot_id,
                        kills: i64::from(player.kills),
                        deaths: i64::from(player.deaths),
                        won: if winner == -1 {
                            None
                        } else {
                            Some(player.team == Some(winner as u8))
                        },
                    })
                })
                .collect()
        };

        for p in payouts {
            let db = Arc::clone(&self.db);
            let result = tokio::task::spawn_blocking(move || {
                let outcome = db.record_match_result(p.pilot_id, p.kills, p.deaths, p.won, 0)?;
                let total = db.get_credits(p.pilot_id)?;
                Ok::<_, crate::db::ApiError>((p.conn_id, outcome, total))
            })
            .await;
            match result {
                Ok(Ok((conn_id, outcome, total))) => {
                    let earned = if outcome.new_achievements.is_empty() {
                        None
                    } else {
                        Some(
                            outcome
                                .new_achievements
                                .iter()
                                .map(|a| spaceships_protocol::Achievement {
                                    kind: a.kind.to_string(),
                                    label: a.label.to_string(),
                                    icon: a.icon.to_string(),
                                    reward: a.reward,
                                })
                                .collect(),
                        )
                    };
                    let msg = ServerMessage::MatchCredits {
                        credits_earned: outcome.credits_earned,
                        total_credits: total,
                        earned,
                    };
                    if let Ok(text) = serde_json::to_string(&msg) {
                        self.lock().send(conn_id, &text);
                    }
                }
                Ok(Err(e)) => eprintln!("stats save failed: {e}"),
                Err(e) => eprintln!("stats task failed: {e}"),
            }
        }
    }

    // ── Relay ───────────────────────────────────────────────────────────────

    fn on_state(&self, id: PlayerId, pos: Vec3, quat: Quat, boost: bool) {
        let inner = self.lock();
        let Some(code) = inner.conns.get(&id).and_then(|c| c.room.clone()) else {
            return;
        };
        if !inner.room(&code).is_some_and(|r| r.started) {
            return;
        }
        inner.relay(
            &code,
            id,
            &ServerMessage::State {
                id,
                pos,
                quat,
                boost,
            },
        );
    }

    /// `fire` and `flare` share a guard: the room must have started and the
    /// sender must be alive.
    fn relay_if_alive(&self, id: PlayerId, msg: ServerMessage) {
        let inner = self.lock();
        let Some(code) = inner.conns.get(&id).and_then(|c| c.room.clone()) else {
            return;
        };
        let Some(room) = inner.room(&code) else {
            return;
        };
        if !room.started {
            return;
        }
        if !room.player(id).is_some_and(|p| p.alive) {
            return;
        }
        inner.relay(&code, id, &msg);
    }

    /// `colors` — relayed with no validation at all, exactly as the JS does.
    fn on_colors(&self, id: PlayerId, hull_color: u32, accent_color: u32) {
        let inner = self.lock();
        let Some(code) = inner.conns.get(&id).and_then(|c| c.room.clone()) else {
            return;
        };
        inner.relay(
            &code,
            id,
            &ServerMessage::Colors {
                id,
                hull_color,
                accent_color,
            },
        );
    }

    /// `ship-model` — the one relay the JS *does* filter.
    ///
    /// It rejects absolute URLs and anything containing `//`, which stops a
    /// player pushing a third-party URL to everyone else's browser and
    /// harvesting their IP addresses. Note what it deliberately does **not**
    /// check: whether the sender actually owns the admin ship. The JS carries a
    /// comment saying an ownership check via `getCustomizationUnlocks` was
    /// considered and skipped. That decision is preserved here — see the crate
    /// docs.
    fn on_ship_model(&self, id: PlayerId, model_url: &str) {
        if model_url.is_empty()
            || model_url.starts_with("http://")
            || model_url.starts_with("https://")
            || model_url.contains("//")
        {
            return;
        }
        let inner = self.lock();
        let Some(code) = inner.conns.get(&id).and_then(|c| c.room.clone()) else {
            return;
        };
        inner.relay(
            &code,
            id,
            &ServerMessage::ShipModel {
                id,
                model_url: model_url.to_string(),
            },
        );
    }

    /// `bot-state` — host-only, relayed as an ordinary `state` with `boost`
    /// forced to `false`.
    fn on_bot_state(&self, id: PlayerId, bot_id: PlayerId, pos: Vec3, quat: Quat) {
        let inner = self.lock();
        let Some(code) = inner.conns.get(&id).and_then(|c| c.room.clone()) else {
            return;
        };
        let Some(room) = inner.room(&code) else {
            return;
        };
        if !room.started || room.bot_host_id != Some(id) {
            return;
        }
        if !room.player(bot_id).is_some_and(|p| p.is_bot) {
            return;
        }
        inner.relay(
            &code,
            id,
            &ServerMessage::State {
                id: bot_id,
                pos,
                quat,
                boost: false,
            },
        );
    }

    /// `bot-fire` — host-only, and the weapon is narrowed harder than a
    /// player's: anything that is not exactly `missile` becomes `bullet`, so a
    /// bot can never emit a beam.
    fn on_bot_fire(
        &self,
        id: PlayerId,
        bot_id: PlayerId,
        kind: WeaponKind,
        shots: Vec<spaceships_protocol::Shot>,
    ) {
        let inner = self.lock();
        let Some(code) = inner.conns.get(&id).and_then(|c| c.room.clone()) else {
            return;
        };
        let Some(room) = inner.room(&code) else {
            return;
        };
        if !room.started || room.bot_host_id != Some(id) {
            return;
        }
        if !room.player(bot_id).is_some_and(|p| p.is_bot && p.alive) {
            return;
        }
        let kind = if kind == WeaponKind::Missile {
            WeaponKind::Missile
        } else {
            WeaponKind::Bullet
        };
        inner.relay(
            &code,
            id,
            &ServerMessage::Fire {
                id: bot_id,
                kind,
                shots,
            },
        );
    }

    // ── Damage ──────────────────────────────────────────────────────────────

    /// `asteroid-hit` — one point of damage regardless of weapon.
    ///
    /// The rock stays in the array at zero hit points rather than being
    /// removed; the `hp <= 0` guard is what makes a late duplicate report a
    /// no-op. [`spaceships_sim::asteroids::apply_damage`] removes it instead,
    /// which is the better model but a different one, and this handler has to
    /// answer `asteroid-hit` reports that arrive after the kill.
    fn on_asteroid_hit(&self, id: PlayerId, rock_id: u32) {
        let mut inner = self.lock();
        let Some(code) = inner.conns.get(&id).and_then(|c| c.room.clone()) else {
            return;
        };
        let Some(room) = inner.room_mut(&code) else {
            return;
        };
        if !room.started || room.asteroids.is_empty() {
            return;
        }
        let Some(rock) = room.asteroids.iter_mut().find(|a| a.id == rock_id) else {
            return;
        };
        if rock.hp <= 0 {
            return;
        }
        rock.hp = (rock.hp - ASTEROID_HIT_DAMAGE).max(0);
        let hp = rock.hp;
        let msg = if hp == 0 {
            ServerMessage::AsteroidDestroyed { id: rock_id }
        } else {
            ServerMessage::AsteroidHp { id: rock_id, hp }
        };
        inner.broadcast(&code, &msg);
    }

    /// `self-damage` — environmental damage the client reports on itself.
    ///
    /// Clamped to `0..=SHIP_MAX_HP` and dropped when non-positive, then applied
    /// verbatim. There is no plausibility check: a client can report 100 damage
    /// every frame and kill itself repeatedly. That is what the JS does, and it
    /// only ever hurts the sender, so it is preserved.
    fn on_self_damage(self: &Arc<Self>, id: PlayerId, dmg: f64) {
        let (code, died) = {
            let mut inner = self.lock();
            let Some(code) = inner.conns.get(&id).and_then(|c| c.room.clone()) else {
                return;
            };
            let Some(room) = inner.room_mut(&code) else {
                return;
            };
            if !room.started {
                return;
            }
            let Some(me) = room.player_mut(id) else {
                return;
            };
            if !me.alive {
                return;
            }
            // `Math.max(0, Math.min(SHIP_MAX_HP, Number(msg.dmg) || 0))`. NaN
            // becomes 0 through the `|| 0`, which `clamp` would panic on, so it
            // is handled first.
            let dmg = if dmg.is_nan() { 0.0 } else { dmg };
            let dmg = dmg.clamp(0.0, f64::from(SHIP_MAX_HP));
            if dmg <= 0.0 {
                return;
            }
            // The JS subtracts a float from an int, so fractional damage yields
            // fractional hp on the wire. All shipped call sites send whole
            // numbers; truncating toward zero keeps those exact.
            me.hp = (me.hp - dmg as i32).max(0);
            let hp = me.hp;
            let died = hp == 0;
            if died {
                let me = room.player_mut(id).expect("looked up above");
                me.alive = false;
                me.deaths += 1;
            }
            inner.broadcast(&code, &ServerMessage::Hp { id, hp });
            if died {
                inner.broadcast(
                    &code,
                    &ServerMessage::Death {
                        id,
                        killer_id: None,
                    },
                );
                inner.broadcast_room(&code);
            }
            (code, died)
        };
        if died {
            self.schedule_respawn(code, id);
        }
    }

    /// `hit` — the client-authoritative damage report.
    ///
    /// # What the server checks
    ///
    /// In order, and every one of them straight from `server/index.js`:
    ///
    /// 1. The room exists and has started.
    /// 2. The target exists and is alive.
    /// 3. If `fromBotId` is set, the sender must be the room's `botHostId`, and
    ///    the named bot must exist, be a bot, and be alive.
    /// 4. The effective shooter exists.
    /// 5. Rate limit: 400 ms between missile hits, 40 ms otherwise, keyed per
    ///    *(effective shooter, weapon)* on the reporting socket.
    /// 6. The shooter is alive — unless the weapon is a missile, which is
    ///    already in flight and lands even if its owner died on the way.
    /// 7. Not self-inflicted, unless routed through a bot.
    /// 8. Not friendly fire.
    /// 9. The target is not still under spawn protection.
    ///
    /// # What the server does not check
    ///
    /// Everything about the geometry. There is no test that the shooter ever
    /// fired, that a projectile existed, that the two ships were within weapons
    /// range, that anything was in line of sight, or that the reported weapon
    /// is one the shooter had ammunition for. A modified client can send
    /// `{"type":"hit","targetId":N,"kind":"missile"}` every 400 ms and delete
    /// any opponent it likes, and neither the rate limit nor the friendly-fire
    /// check stops it.
    ///
    /// **That behaviour is preserved deliberately.** Closing it means the
    /// server has to own projectile simulation — which is exactly what
    /// [`spaceships_sim`] is being built to do — and doing it here, halfway,
    /// would desynchronise the shipped browser client, which resolves its own
    /// hits locally and would see damage it did not predict. See the crate
    /// docs for the full note.
    fn on_hit(
        self: &Arc<Self>,
        id: PlayerId,
        target_id: PlayerId,
        kind: WeaponKind,
        from_bot_id: Option<PlayerId>,
    ) {
        let (code, died, victim) = {
            let mut inner = self.lock();
            let Some(code) = inner.conns.get(&id).and_then(|c| c.room.clone()) else {
                return;
            };

            let effective_shooter;
            {
                let Some(room) = inner.room(&code) else {
                    return;
                };
                if !room.started {
                    return;
                }
                if !room.player(target_id).is_some_and(|p| p.alive) {
                    return;
                }
                effective_shooter = match from_bot_id {
                    Some(bot) => {
                        if room.bot_host_id != Some(id) {
                            return;
                        }
                        if !room.player(bot).is_some_and(|p| p.is_bot && p.alive) {
                            return;
                        }
                        bot
                    }
                    None => id,
                };
                if room.player(effective_shooter).is_none() {
                    return;
                }
            }

            // Rate limit, on the *reporting* socket's table.
            let now = Instant::now();
            let min_interval = Duration::from_millis(if kind == WeaponKind::Missile {
                MISSILE_HIT_MIN_INTERVAL_MS
            } else {
                GUN_HIT_MIN_INTERVAL_MS
            });
            {
                let Some(conn) = inner.conns.get_mut(&id) else {
                    return;
                };
                let key = (effective_shooter, kind);
                if let Some(prev) = conn.hit_times.get(&key) {
                    if now.duration_since(*prev) < min_interval {
                        return;
                    }
                }
                conn.hit_times.insert(key, now);
            }

            let Some(room) = inner.room_mut(&code) else {
                return;
            };
            let shooter_alive = room.player(effective_shooter).is_some_and(|p| p.alive);
            if !shooter_alive && kind != WeaponKind::Missile {
                return;
            }
            if target_id == id && from_bot_id.is_none() {
                return;
            }
            let shooter_team = room.player(effective_shooter).and_then(|p| p.team);
            let target_team = room.player(target_id).and_then(|p| p.team);
            if target_team.is_some() && target_team == shooter_team {
                return;
            }
            if room
                .player(target_id)
                .and_then(|p| p.invuln_until)
                .is_some_and(|until| Instant::now() < until)
            {
                return;
            }

            let dmg = if kind == WeaponKind::Missile {
                MISSILE_DAMAGE
            } else {
                GUN_DAMAGE
            };
            let target = room.player_mut(target_id).expect("checked above");
            target.hp = (target.hp - dmg).max(0);
            let hp = target.hp;
            let died = hp == 0;
            if died {
                target.alive = false;
                target.deaths += 1;
                if let Some(s) = room.player_mut(effective_shooter) {
                    s.kills += 1;
                }
                if let Some(team) = shooter_team {
                    if let Some(slot) = room.team_kills.get_mut(usize::from(team)) {
                        *slot += 1;
                    }
                }
            }
            inner.broadcast(&code, &ServerMessage::Hp { id: target_id, hp });
            if died {
                inner.broadcast(
                    &code,
                    &ServerMessage::Death {
                        id: target_id,
                        killer_id: Some(effective_shooter),
                    },
                );
                inner.broadcast_room(&code);
            }
            (code, died, target_id)
        };
        if died {
            self.schedule_respawn(code, victim);
        }
    }

    /// The `setTimeout(..., RESPAWN_DELAY_MS)` both death paths install.
    ///
    /// Re-checks that the player is still in the room and the match is still
    /// running, exactly as the JS closure does — a player who leaves during
    /// their death timer must not be resurrected.
    fn schedule_respawn(self: &Arc<Self>, code: String, victim: PlayerId) {
        let lobby = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(RESPAWN_DELAY_MS)).await;
            let mut inner = lobby.lock();
            let Inner { rooms, rng, .. } = &mut *inner;
            let Some(room) = rooms.iter_mut().find(|r| r.code == code) else {
                return;
            };
            if !room.started {
                return;
            }
            let map = room.map;
            let Some(p) = room.player_mut(victim) else {
                return;
            };
            p.hp = SHIP_MAX_HP;
            p.alive = true;
            p.invuln_until = Some(Instant::now() + Duration::from_millis(SPAWN_INVULN_MS));
            let team = p.team.unwrap_or(0);
            let sp = spawn_for_team(rng, team, map);
            inner.broadcast(
                &code,
                &ServerMessage::Respawn {
                    id: victim,
                    pos: sp.pos,
                    quat: sp.quat,
                },
            );
        });
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Free helpers
// ─────────────────────────────────────────────────────────────────────────────

fn send_error(inner: &Inner, id: PlayerId, message: &str) {
    let msg = ServerMessage::Error {
        message: message.to_string(),
    };
    if let Ok(text) = serde_json::to_string(&msg) {
        inner.send(id, &text);
    }
}

/// `leaveRoom`.
///
/// Removes the player, evicts the bots if their driver left, migrates the host
/// to the first remaining **human**, and tears the room down when no human is
/// left — a room of only bots has nobody to receive broadcasts and nobody to
/// drive them.
fn leave_room(inner: &mut Inner, id: PlayerId) {
    let Some(code) = inner.conns.get(&id).and_then(|c| c.room.clone()) else {
        return;
    };
    if let Some(c) = inner.conns.get_mut(&id) {
        c.room = None;
    }
    let Some(room) = inner.room_mut(&code) else {
        return;
    };
    room.sockets.retain(|s| *s != id);
    room.players.retain(|p| p.id != id);
    if room.bot_host_id == Some(id) {
        room.players.retain(|p| !p.is_bot);
        room.bot_host_id = None;
    }
    let next_host = room.players.iter().find(|p| !p.is_bot).map(|p| p.id);
    let Some(next_host) = next_host else {
        room.cancel_timers();
        inner.rooms.retain(|r| r.code != code);
        return;
    };
    if room.host_id == id {
        room.host_id = next_host;
    }
    inner.broadcast_room(&code);
}

/// The `name` sanitizer: strip everything outside `[A-Za-z0-9 _-]`, trim, cap
/// at 16 characters.
///
/// The order matters and is the JS's: filter first, *then* trim, *then*
/// truncate. Filtering first means `"  Mav  "` survives as `"Mav"`, and
/// truncating last means a name is cut at 16 characters of already-clean text.
#[must_use]
pub fn sanitize_name(raw: &str) -> String {
    let filtered: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == ' ' || *c == '_' || *c == '-')
        .collect();
    filtered.trim().chars().take(MAX_NAME_LEN).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_sanitizer_matches_the_js_regex() {
        assert_eq!(sanitize_name("Maverick"), "Maverick");
        assert_eq!(sanitize_name("  Mav  "), "Mav");
        // 18 characters survive the filter, then the 16-char cap bites.
        assert_eq!(
            sanitize_name("<script>alert(1)</script>"),
            "scriptalert1scri"
        );
        assert_eq!(sanitize_name("a-b_c 1"), "a-b_c 1");
        // Truncation happens after trimming, and at 16 characters.
        assert_eq!(
            sanitize_name("ABCDEFGHIJKLMNOPQRSTUVWXYZ"),
            "ABCDEFGHIJKLMNOP"
        );
        // Non-ASCII is stripped entirely, which can empty the name.
        assert_eq!(sanitize_name("日本語"), "");
        assert_eq!(sanitize_name("   "), "");
    }

    #[test]
    fn space_spawns_sit_in_front_of_each_mothership() {
        let mut rng = Rng::new(7);
        for _ in 0..200 {
            let a = spawn_for_team(&mut rng, 0, MapKind::Space);
            assert!((a.pos[2] - -540.0).abs() <= 3.0, "{:?}", a.pos);
            assert_eq!(a.quat, [0.0, 0.0, 0.0, 1.0]);
            let b = spawn_for_team(&mut rng, 1, MapKind::Space);
            assert!((b.pos[2] - 540.0).abs() <= 3.0, "{:?}", b.pos);
            assert_eq!(b.quat, [0.0, 1.0, 0.0, 0.0]);
        }
    }

    /// Above the runway, not inside the mesa it sits on: the assertion is
    /// against the *terrain*, so it stays honest if either the pad elevation or
    /// the launch height moves again.
    #[test]
    fn terrain_spawns_sit_above_the_runways() {
        let rules = spaceships_sim::rules::Rules::DEFAULT;
        let mut rng = Rng::new(11);
        for _ in 0..200 {
            for team in 0..2u8 {
                let s = spawn_for_team(&mut rng, team, MapKind::Terrain);
                let sign = if team == 0 { -1.0 } else { 1.0 };
                assert!((s.pos[2] - sign * rules.spawn.terrain_z).abs() <= 20.0);
                let ground =
                    spaceships_sim::terrain::ground_height(s.pos[0], s.pos[2], &rules.world);
                assert_eq!(ground, rules.world.airfield_elevation);
                let clearance = s.pos[1] - ground;
                assert!(
                    (clearance - 40.0).abs() <= 5.0,
                    "spawned {clearance} above the pad",
                );
            }
        }
    }
}
