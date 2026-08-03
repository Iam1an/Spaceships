//! Wire contract for the Spaceships multiplayer WebSocket protocol.
//!
//! This crate is a faithful, byte-compatible transcription of the JSON messages
//! that the **existing JavaScript** server (`server/index.js`) and client
//! (`public/src/main.js`, `public/src/lobby.js`) already exchange over `/ws`.
//! It is deliberately *descriptive*, not aspirational: nothing here invents a
//! new message, renames a field, or tightens a type beyond what the live game
//! actually puts on the wire. A Rust server built on this crate must be able to
//! serve the current unmodified browser client, and a Rust/WASM client must be
//! able to talk to the current unmodified Node server.
//!
//! # Encoding
//!
//! Every frame is a single unfragmented UTF-8 WebSocket text frame containing
//! one JSON object with a `"type"` discriminator. That maps onto serde's
//! internally-tagged enum representation:
//!
//! ```
//! use spaceships_protocol::ClientMessage;
//! let msg: ClientMessage = serde_json::from_str(r#"{"type":"join","code":"ABCD"}"#).unwrap();
//! assert_eq!(serde_json::to_string(&msg).unwrap(), r#"{"type":"join","code":"ABCD"}"#);
//! ```
//!
//! # Naming
//!
//! Field names are camelCase (`targetId`, `hullColor`, `botAssignments`) and are
//! produced by `rename_all_fields = "camelCase"`. Message *tags* are **not**
//! uniformly camelCase in the JS source — multi-word tags are kebab-case
//! (`list-rooms`, `asteroid-hit`, `match-end`, `self-damage`, `ship-model`).
//! Those variants therefore carry an explicit `#[serde(rename = "...")]` that
//! overrides the container-level `rename_all`. Byte compatibility with the live
//! game wins over cosmetic consistency.
//!
//! # Numeric fidelity
//!
//! One deliberate, semantically-neutral difference: fields typed `f64` here
//! re-serialize an integral value as `0.0` where `JSON.stringify` would emit
//! `0` (spawn quaternions such as `[0,0,0,1]`, and `self-damage`'s `dmg`, are
//! the cases that actually occur). Both forms parse to the identical IEEE-754
//! double in every JSON parser including V8, so this cannot change behaviour —
//! but it does mean output is byte-identical *modulo JSON number formatting*,
//! not literally byte-identical. The round-trip tests compare structurally for
//! exactly this reason.
//!
//! # Messages the JS does not have
//!
//! The paragraph above is still the rule, and there is now exactly one
//! documented exception to it: **`emp`**, in both directions
//! ([`ClientMessage::Emp`], [`ServerMessage::Emp`]). The EMP is a weapon the
//! browser game does not implement, so there is no `ws.send` to transcribe and
//! no `msg.type === 'emp'` to match.
//!
//! Adding it is safe in both directions and deliberately does not change the JS:
//!
//! - **To the Node server**, an unknown tag falls off the end of
//!   `handleConnection`'s `if` chain and is dropped. Nothing errors, nothing
//!   disconnects, and no browser peer is told — so an EMP fired in a match
//!   hosted by the JS server blinds only what the firing client simulates
//!   itself.
//! - **To a browser client**, both `lobby.js` and `main.js` ignore tags they do
//!   not know, so a Rust server may relay this into a mixed room without
//!   breaking anyone.
//!
//! The precedent for an additive field is already here: [`ServerMessage::Start`]
//! carries an optional `seed` the JS server never sends. The rule this section
//! makes explicit is that *additions must be inert to the JS on both sides*, and
//! anything that is not is a change to `server/index.js` instead.
//!
//! # Direction
//!
//! [`ClientMessage`] is browser -> server, [`ServerMessage`] is server ->
//! browser. Several tags appear in both directions (`state`, `fire`, `flare`,
//! `colors`, `ship-model`, `start`) but with *different shapes*: the client
//! omits the `id` field and the server stamps it on before relaying. That
//! asymmetry is why these are two separate enums rather than one shared one.
//!
//! # Source of truth
//!
//! - `server/index.js` `handleConnection` (client -> server dispatch)
//! - `server/index.js` `broadcast` / `broadcastRoom` / `roomSnapshot` / `endMatch`
//! - `public/src/main.js` in-game `message` listener and `ws.send` call sites
//! - `public/src/lobby.js` `handle()` and its `send()` call sites

#![warn(missing_docs)]
#![forbid(unsafe_code)]

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;

pub mod consts;

/// `serde_with`-style adapter for the `spawns` object in
/// [`ServerMessage::Start`].
///
/// JSON object keys are always strings, and `spawns` is keyed by player id. On
/// its own `serde_json` handles integer map keys transparently, but internally
/// tagged enums (`#[serde(tag = "type")]`) deserialize through serde's private
/// `Content` buffer, which erases the map-key context and makes the key arrive
/// as a plain string. This adapter does the `PlayerId <-> String` conversion
/// explicitly so the field can stay strongly typed.
mod spawn_map {
    use super::{PlayerId, Spawn};
    use serde::de::{self, MapAccess, Visitor};
    use serde::ser::SerializeMap;
    use serde::{Deserializer, Serializer};
    use std::collections::BTreeMap;
    use std::fmt;

    /// Writes the map with stringified integer keys, exactly as `JSON.stringify`
    /// does for a JS object keyed by number.
    pub fn serialize<S>(map: &BTreeMap<PlayerId, Spawn>, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut out = ser.serialize_map(Some(map.len()))?;
        for (id, spawn) in map {
            out.serialize_entry(&id.to_string(), spawn)?;
        }
        out.end()
    }

    /// Parses stringified integer keys back into [`PlayerId`].
    pub fn deserialize<'de, D>(de: D) -> Result<BTreeMap<PlayerId, Spawn>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SpawnMapVisitor;

        impl<'de> Visitor<'de> for SpawnMapVisitor {
            type Value = BTreeMap<PlayerId, Spawn>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object keyed by stringified player id")
            }

            fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut out = BTreeMap::new();
                while let Some((key, spawn)) = access.next_entry::<String, Spawn>()? {
                    let id = key.parse::<PlayerId>().map_err(|_| {
                        de::Error::custom(format!("spawns key is not a player id: {key:?}"))
                    })?;
                    out.insert(id, spawn);
                }
                Ok(out)
            }
        }

        de.deserialize_map(SpawnMapVisitor)
    }
}

/// A position or direction as it appears on the wire: a bare JSON array of
/// three `f64`, in THREE.js `[x, y, z]` order.
///
/// This is intentionally *not* `spaceships_sim::Vec3`; the protocol crate stays
/// free of simulation types so the wire format can never drift by accident when
/// the sim's math representation changes.
pub type Vec3 = [f64; 3];

/// A rotation as it appears on the wire: a bare JSON array of four `f64` in
/// THREE.js `Quaternion.toArray()` order, i.e. `[x, y, z, w]` (**w last**).
pub type Quat = [f64; 4];

/// Server-assigned connection id. Positive for humans (`nextId++`), negative for
/// server-spawned balance bots (`-(nextId++)`), so this must stay signed.
pub type PlayerId = i64;

/// Index of an asteroid within the room's generated field (`0..count`).
pub type AsteroidId = u32;

// ─────────────────────────────────────────────────────────────────────────────
// Scalar enums
// ─────────────────────────────────────────────────────────────────────────────

/// Which map a room is played on.
///
/// The JS server coerces this defensively (`msg.map === 'terrain' ? 'terrain' :
/// 'space'`), so deserialization here is deliberately lenient too: any
/// unrecognized string becomes [`MapKind::Space`] rather than erroring, which
/// preserves the current server's behaviour for malformed input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MapKind {
    /// Open space with two motherships and a generated asteroid field.
    #[default]
    Space,
    /// Terrain map with airfields at `z = ±1500`.
    Terrain,
}

impl MapKind {
    /// Parses a wire string the way the JS server does: anything that is not
    /// exactly `"terrain"` is treated as `"space"`.
    pub fn from_wire(s: &str) -> Self {
        match s {
            "terrain" => MapKind::Terrain,
            _ => MapKind::Space,
        }
    }

    /// The exact string this variant serializes to.
    pub fn as_wire(self) -> &'static str {
        match self {
            MapKind::Space => "space",
            MapKind::Terrain => "terrain",
        }
    }
}

impl<'de> Deserialize<'de> for MapKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(MapKind::from_wire(&s))
    }
}

/// Which weapon a `fire`/`bot-fire`/`hit` message refers to.
///
/// Damage is resolved server-side purely from this field: missiles do 50, and
/// everything else does 10 (`server/index.js`, `hit` handler). Hit rate limiting
/// also keys off it — 400 ms minimum between missile hits, 40 ms otherwise.
///
/// Deserialization is lenient (unknown -> [`WeaponKind::Bullet`]) to mirror the
/// JS server, which never rejects a bad `kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WeaponKind {
    /// Projectile cannon. `Shot { pos, dir }`.
    #[default]
    Bullet,
    /// Hitscan beam. `Shot { pos, end }` — the client has already resolved the
    /// endpoint locally and only sends it for the visual.
    Beam,
    /// Homing missile. `Shot { pos, dir, targetId }`.
    Missile,
}

impl WeaponKind {
    /// Parses a wire string leniently; unknown values become
    /// [`WeaponKind::Bullet`].
    pub fn from_wire(s: &str) -> Self {
        match s {
            "beam" => WeaponKind::Beam,
            "missile" => WeaponKind::Missile,
            _ => WeaponKind::Bullet,
        }
    }

    /// The exact string this variant serializes to.
    pub fn as_wire(self) -> &'static str {
        match self {
            WeaponKind::Bullet => "bullet",
            WeaponKind::Beam => "beam",
            WeaponKind::Missile => "missile",
        }
    }
}

impl<'de> Deserialize<'de> for WeaponKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(WeaponKind::from_wire(&s))
    }
}

/// Size class of a generated asteroid.
///
/// Unlike [`MapKind`] and [`WeaponKind`] this is *server-authoritative* — it is
/// produced by `generateAsteroidField` from a fixed table and never accepted
/// from a client — so strict deserialization is correct here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AsteroidTier {
    /// size 5..7, 5 hp, 45% of the field.
    Small,
    /// size 9..15, 10 hp, 30% of the field.
    Medium,
    /// size 18..30, 30 hp, 18% of the field.
    Big,
    /// size 38..55, 50 hp, 7% of the field.
    Huge,
}

// ─────────────────────────────────────────────────────────────────────────────
// Payload structs
// ─────────────────────────────────────────────────────────────────────────────

/// One projectile/beam within a `fire` or `bot-fire` message.
///
/// The JS client emits three different shapes under one field name, which is why
/// every field but `pos` is optional:
///
/// | `kind`    | emitted fields          |
/// |-----------|-------------------------|
/// | `bullet`  | `pos`, `dir`            |
/// | `beam`    | `pos`, `end`            |
/// | `missile` | `pos`, `dir`, `targetId`|
///
/// Absent fields are omitted from the output rather than serialized as `null`,
/// matching the JS exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Shot {
    /// Muzzle origin in world space. For beams this is the *visual* start,
    /// pushed forward by `BEAM_FORWARD_OFFSET`, not the true ray origin.
    pub pos: Vec3,
    /// Unit direction. Present for `bullet` and `missile`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<Vec3>,
    /// Endpoint of an already-resolved hitscan beam. Present for `beam` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<Vec3>,
    /// Homing target. Present for `missile` only. May reference the receiving
    /// player, in which case the client homes onto its own ship record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<PlayerId>,
}

/// One row of the lobby roster, as produced by `roomSnapshot`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerInfo {
    /// Connection id.
    pub id: PlayerId,
    /// Sanitized display name (`[A-Za-z0-9 _-]`, trimmed, max 16 chars).
    pub name: String,
    /// True for the room host.
    pub host: bool,
    /// Team 0 or 1; `null` until the host presses start.
    pub team: Option<u8>,
    /// True for server-spawned balance bots.
    pub is_bot: bool,
    /// Kills this match.
    pub kills: u32,
    /// Deaths this match.
    pub deaths: u32,
}

/// One row of the public room browser, as produced by the `list-rooms` handler.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomSummary {
    /// Four uppercase ASCII letters.
    pub code: String,
    /// Humans only — bots are excluded from this count.
    pub player_count: u32,
    /// Host's display name, or `"Unknown"` if the host record is missing.
    pub host_name: String,
}

/// Where a player materializes at match start, keyed by player id in
/// [`ServerMessage::Start::spawns`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spawn {
    /// Team assignment decided at start time.
    pub team: u8,
    /// Spawn position (jittered around the team's mothership or airfield).
    pub pos: Vec3,
    /// Spawn orientation: `[0,0,0,1]` for team 0, `[0,1,0,0]` for team 1.
    pub quat: Quat,
}

/// A balance bot the host is told to drive locally.
///
/// Only the host receives a non-empty list; the JS lobby drops it for everyone
/// else (`myBotAssignments = isHost ? (msg.botAssignments || []) : []`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BotAssignment {
    /// Negative synthetic player id.
    pub id: PlayerId,
    /// Team the bot fills in for.
    pub team: u8,
    /// Spawn position.
    pub pos: Vec3,
    /// Spawn orientation.
    pub quat: Quat,
}

/// One asteroid from the server-authoritative field generated at match start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Asteroid {
    /// Index within the field; the id used by `asteroid-hit` / `asteroid-hp` /
    /// `asteroid-destroyed`.
    pub id: AsteroidId,
    /// World position.
    pub pos: Vec3,
    /// Initial Euler rotation, radians, `[x, y, z]`.
    pub rot: Vec3,
    /// Radius in world units, drawn from the tier's `[minSize, maxSize]` range.
    pub size: f64,
    /// Starting hit points; decremented by one per reported hit.
    pub hp: i32,
    /// Size class the other fields were rolled from.
    pub tier: AsteroidTier,
    /// Mesh variant index, `0..6`.
    pub variant: u8,
    /// Angular velocity, each component in `[-0.5, 0.5)`.
    pub spin: Vec3,
}

/// An achievement unlocked at match end, carried by
/// [`ServerMessage::MatchCredits`].
///
/// Mirrors `checkAndAwardAchievements` in `server/db.js`, which pushes
/// `{ type, label, icon, reward }` — a projection of `ACHIEVEMENT_DEFS` with the
/// `desc`, `check`, and `progress` fields dropped.
///
/// Note the `type` field here is ordinary data, *not* a message discriminator;
/// this struct is nested inside an already-tagged message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Achievement {
    /// Stable identifier, e.g. `"first_kill"`, `"kills_10"`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Human-readable title, e.g. `"First Blood"`.
    pub label: String,
    /// Emoji shown on the toast.
    pub icon: String,
    /// Credits granted for this unlock; `0` means no payout.
    pub reward: i64,
}

// ─────────────────────────────────────────────────────────────────────────────
// Client -> server
// ─────────────────────────────────────────────────────────────────────────────

/// Messages the browser sends to the server.
///
/// Every variant here corresponds to exactly one `if (msg.type === '...')` arm
/// in `handleConnection`. Anything not listed is silently ignored by the current
/// server, so a Rust port should ignore unknown tags rather than disconnect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ClientMessage {
    /// Set/replace the display name. Sent immediately before `create`, `join`,
    /// or `list-rooms`. The server sanitizes to `[A-Za-z0-9 _-]`, trims, and
    /// truncates to 16 chars; an empty result is discarded and the previous name
    /// is kept.
    Name {
        /// Requested display name, pre-sanitization.
        name: String,
    },

    /// Ask for the public room browser listing. Answered with
    /// [`ServerMessage::RoomsList`].
    #[serde(rename = "list-rooms")]
    ListRooms,

    /// Create a room and become its host. Answered with [`ServerMessage::Room`]
    /// plus a broadcast [`ServerMessage::Players`].
    Create {
        /// Hide the room from `list-rooms`.
        private: bool,
        /// Map selection.
        map: MapKind,
        /// Whether the server may add a balance bot to the smaller team at
        /// start. The JS default is *on*: the server treats only an explicit
        /// `false` as opt-out.
        allow_bot: bool,
    },

    /// Join an existing room by code. The server uppercases the code and
    /// replies with [`ServerMessage::Error`] if the room is missing or already
    /// started.
    Join {
        /// Four-letter room code; case-insensitive on the wire.
        code: String,
    },

    /// Host-only: begin the match. Ignored from non-hosts.
    Start,

    /// Leave the current room without closing the socket.
    Leave,

    /// 20 Hz position update for the sender's own ship. Only sent while alive.
    State {
        /// World position.
        pos: Vec3,
        /// World orientation.
        quat: Quat,
        /// Whether the boost effect should be shown on the remote ship.
        boost: bool,
    },

    /// The sender fired. Relayed to everyone else as [`ServerMessage::Fire`]
    /// with the shooter's id stamped on; the server does no validation of the
    /// shot contents beyond requiring the shooter to be alive.
    Fire {
        /// Weapon used.
        kind: WeaponKind,
        /// One entry per muzzle (or a single entry for missiles).
        shots: Vec<Shot>,
    },

    /// The sender deployed a countermeasure flare.
    Flare {
        /// Deployment position.
        pos: Vec3,
        /// Deployment orientation.
        quat: Quat,
    },

    /// The sender set off an EMP. Relayed as [`ServerMessage::Emp`].
    ///
    /// **The one message in this crate with no JavaScript counterpart.** See the
    /// crate docs' *"Messages the JS does not have"* for the rule, and
    /// `spaceships-sim`'s `emp` module for the weapon. What it means in practice:
    /// `server/index.js` dispatches a fixed `if (msg.type === ...)` chain and
    /// falls through on anything else, so sending this to the Node server is a
    /// no-op — it is dropped, no browser peer is told, and nothing breaks. The
    /// Rust server relays it.
    ///
    /// Carries the centre of the pulse and nothing else: who it caught is each
    /// recipient's own answer, computed against the poses that recipient already
    /// holds. There is no `quat` because a sphere has no facing, and no radius
    /// because that is a rule both ends already share.
    Emp {
        /// Centre of the pulse: the firing ship's position when it went off.
        pos: Vec3,
    },

    /// Client-authoritative hit report. The server applies damage, kills, team
    /// score, and respawn scheduling from this message.
    ///
    /// Rejected when: the target is missing/dead, the shooter is dead and the
    /// weapon is not a missile, the report is self-inflicted without a
    /// `fromBotId`, the two are on the same team, the target still has spawn
    /// protection, or the per-shooter/per-weapon rate limit trips.
    Hit {
        /// Who was hit.
        target_id: PlayerId,
        /// Weapon that landed; drives damage (missile 50, else 10).
        kind: WeaponKind,
        /// Set when a locally-driven balance bot scored the hit. Only accepted
        /// from the room's `botHostId`. Absent for the player's own shots.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_bot_id: Option<PlayerId>,
    },

    /// Environmental self-damage (asteroid scrape, brake overcharge, terrain or
    /// water impact). The server clamps to `0..=SHIP_MAX_HP`.
    #[serde(rename = "self-damage")]
    SelfDamage {
        /// Damage to apply.
        ///
        /// TODO(verify): all current call sites send whole numbers (1, a random
        /// 15..29 for asteroid contact, or `SHIP_MAX_HP`), but the JS server
        /// applies `Number(msg.dmg)` with no rounding, so a fractional value
        /// would produce fractional hp. Typed as `f64` to preserve that exact
        /// behaviour rather than silently tightening the contract.
        dmg: f64,
    },

    /// Announce the sender's ship colors so late joiners and new peers can paint
    /// the remote ship correctly.
    Colors {
        /// Hull color as a packed `0xRRGGBB` integer.
        ///
        /// TODO(verify): the JS client always *sends* a number
        /// (`parseInt(hex, 16)`), but its receive path tolerates a string
        /// (`typeof msg.hullColor === 'number' ? ... : parseInt(String(...)...)`).
        /// Typed strictly as an integer here because no code path in this repo
        /// emits the string form; a lenient port would need an untagged
        /// number-or-string type.
        hull_color: u32,
        /// Accent color as a packed `0xRRGGBB` integer. Same caveat as
        /// `hullColor`.
        accent_color: u32,
    },

    /// Announce a non-default ship model (the admin hull).
    ///
    /// The server rejects anything containing `//` or starting with `http://` /
    /// `https://` to keep remote URLs off other players' machines.
    #[serde(rename = "ship-model")]
    ShipModel {
        /// Same-origin relative path to a `.glb`.
        model_url: String,
    },

    /// Report a bullet/beam hit on an asteroid. Always exactly 1 damage,
    /// regardless of weapon.
    #[serde(rename = "asteroid-hit")]
    AsteroidHit {
        /// Asteroid index within the room's field.
        id: AsteroidId,
    },

    /// Host-only: position update for a locally-driven balance bot. Relayed to
    /// the other clients as an ordinary [`ServerMessage::State`] with
    /// `boost: false` forced.
    #[serde(rename = "bot-state")]
    BotState {
        /// Which bot (negative id).
        bot_id: PlayerId,
        /// Bot world position.
        pos: Vec3,
        /// Bot world orientation.
        quat: Quat,
    },

    /// Host-only: a locally-driven balance bot fired. Relayed as
    /// [`ServerMessage::Fire`].
    ///
    /// The server narrows `kind` harder than it does for players: anything that
    /// is not `"missile"` is rewritten to `"bullet"`, so a bot can never emit a
    /// beam even if the client asks for one.
    #[serde(rename = "bot-fire")]
    BotFire {
        /// Which bot (negative id).
        bot_id: PlayerId,
        /// Weapon; coerced to bullet unless it is exactly `missile`.
        kind: WeaponKind,
        /// Shots fired.
        shots: Vec<Shot>,
    },
}

// ─────────────────────────────────────────────────────────────────────────────
// Server -> client
// ─────────────────────────────────────────────────────────────────────────────

/// Messages the server sends to the browser.
///
/// Variants are split between the lobby (handled in `public/src/lobby.js`) and
/// the in-game socket listener (`public/src/main.js`); both listeners run
/// against the same socket, and each simply ignores tags it does not know.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ServerMessage {
    /// Acknowledges `create` or `join`. Sent only to the requesting socket.
    Room {
        /// Four-letter room code.
        code: String,
        /// Whether the recipient is the host.
        host: bool,
        /// The recipient's own player id. Named `you` on the wire.
        you: PlayerId,
        /// Whether the room is hidden from the browser.
        private: bool,
    },

    /// Full lobby/scoreboard roster. Broadcast on join, leave, rename, match
    /// start, and every death.
    Players {
        /// One entry per player, bots included.
        players: Vec<PlayerInfo>,
    },

    /// Answer to `list-rooms`. Only open, not-yet-started rooms are listed.
    #[serde(rename = "rooms-list")]
    RoomsList {
        /// Joinable rooms.
        rooms: Vec<RoomSummary>,
    },

    /// The host started the match. Broadcast to the whole room.
    Start {
        /// Spawn point per player, keyed by player id.
        ///
        /// TODO(verify): JSON object keys are strings, so the `PlayerId` keys
        /// are converted by the [`spawn_map`] adapter. Re-serializing sorts
        /// numerically ascending, which puts negative bot ids *first*; V8 emits
        /// array-index-like keys ascending and all other keys (including
        /// negative ones) in insertion order, so a round-tripped `spawns` object
        /// can differ from the JS original in key *order* only. Nothing depends
        /// on that order — the client indexes with `msg.spawns[myId]`.
        #[serde(with = "spawn_map")]
        spawns: BTreeMap<PlayerId, Spawn>,
        /// The generated asteroid field. Empty (`[]`, never `null`) on the
        /// terrain map.
        ///
        /// This is the largest frame the protocol sends — measured at 16,399
        /// bytes, essentially all of it these records — and it arrives at the
        /// worst possible moment, while assets are still loading. On a mobile
        /// connection it is a visible hitch.
        ///
        /// Prefer [`seed`](Self::Start::seed) where the peer supports it: the
        /// simulation generates fields deterministically from a seed, so eight
        /// bytes reproduce what sixty records describe. Kept because the shipped
        /// JS client has no generator and reads this array directly; a Rust
        /// client that received a seed can ignore it.
        asteroids: Vec<Asteroid>,
        /// Seed for deterministic asteroid field generation.
        ///
        /// `None` from the JS server, which has no generator. When present, a
        /// client with `spaceships-sim` can reproduce
        /// [`asteroids`](Self::Start::asteroids) exactly rather than reading it
        /// off the wire — same tier table, same positions, same ids.
        ///
        /// Skipped when absent so the frame stays byte-identical to what the JS
        /// server emits.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seed: Option<u64>,
        /// Map the match is played on.
        map: MapKind,
        /// Bots the host must drive. Non-empty only for the host.
        bot_assignments: Vec<BotAssignment>,
    },

    /// Relayed position update for another player or bot. Never echoed back to
    /// the sender.
    State {
        /// Whose ship this describes.
        id: PlayerId,
        /// World position.
        pos: Vec3,
        /// World orientation.
        quat: Quat,
        /// Boost flag. Always `false` when relayed from a `bot-state`.
        boost: bool,
    },

    /// Relayed weapon discharge. Never echoed back to the sender.
    Fire {
        /// Shooter (may be a negative bot id).
        id: PlayerId,
        /// Weapon used.
        kind: WeaponKind,
        /// Shots fired.
        shots: Vec<Shot>,
    },

    /// Relayed flare deployment. Never echoed back to the sender.
    Flare {
        /// Who deployed it.
        id: PlayerId,
        /// Deployment position.
        pos: Vec3,
        /// Deployment orientation.
        quat: Quat,
    },

    /// Relayed EMP. Never echoed back to the sender, who has already detonated
    /// its own copy.
    ///
    /// No JS counterpart — see [`ClientMessage::Emp`]. A browser client ignores
    /// tags it does not know, so a Rust server relaying this into a mixed room
    /// is harmless there as well: the JS pilots simply never go dark.
    Emp {
        /// Who set it off. Excluded from their own pulse.
        id: PlayerId,
        /// Centre of the pulse.
        pos: Vec3,
    },

    /// Relayed ship colors. Never echoed back to the sender.
    Colors {
        /// Whose ship to repaint.
        id: PlayerId,
        /// Packed `0xRRGGBB` hull color.
        hull_color: u32,
        /// Packed `0xRRGGBB` accent color.
        accent_color: u32,
    },

    /// Relayed ship model override. Never echoed back to the sender.
    #[serde(rename = "ship-model")]
    ShipModel {
        /// Whose ship to re-mesh.
        id: PlayerId,
        /// Same-origin relative `.glb` path, already validated by the server.
        model_url: String,
    },

    /// Authoritative hit points after damage. Broadcast to the whole room,
    /// including the damaged player.
    Hp {
        /// Whose hp changed.
        id: PlayerId,
        /// New hp, clamped to `0..=SHIP_MAX_HP`.
        hp: i32,
    },

    /// A ship was destroyed. Broadcast to the whole room, followed by a
    /// [`ServerMessage::Players`] refresh.
    Death {
        /// Who died.
        id: PlayerId,
        /// Who gets the kill, or `null` for self-inflicted deaths. Always
        /// present as an explicit `null` rather than omitted.
        killer_id: Option<PlayerId>,
    },

    /// A ship came back, `RESPAWN_DELAY_MS` after its death. Broadcast to the
    /// whole room.
    Respawn {
        /// Who respawned.
        id: PlayerId,
        /// New position.
        pos: Vec3,
        /// New orientation.
        quat: Quat,
    },

    /// A player's socket closed mid-match. Only sent if the match had started.
    Disconnect {
        /// Who left.
        id: PlayerId,
    },

    /// An asteroid took damage but survived.
    #[serde(rename = "asteroid-hp")]
    AsteroidHp {
        /// Asteroid index.
        id: AsteroidId,
        /// Remaining hp, always `>= 1`.
        hp: i32,
    },

    /// An asteroid's hp reached zero.
    #[serde(rename = "asteroid-destroyed")]
    AsteroidDestroyed {
        /// Asteroid index.
        id: AsteroidId,
    },

    /// 1 Hz match clock and score tick.
    #[serde(rename = "match-state")]
    MatchState {
        /// Seconds remaining, floating point, floored at `0`.
        timer: f64,
        /// Kills per team, `[team0, team1]`.
        team_kills: [u32; 2],
    },

    /// The match ended, either on the timer or when the room emptied.
    #[serde(rename = "match-end")]
    MatchEnd {
        /// Winning team index, or `-1` for a draw.
        winner: i8,
        /// Final kills per team.
        team_kills: [u32; 2],
    },

    /// Per-pilot payout, sent individually to each authenticated socket right
    /// after `match-end`. Guests and bots get nothing.
    #[serde(rename = "match-credits")]
    MatchCredits {
        /// Credits earned from this match, including achievement rewards.
        credits_earned: i64,
        /// The pilot's new balance.
        total_credits: i64,
        /// Achievements unlocked by this match. The JS server *omits* this key
        /// entirely when nothing was unlocked, so it is skipped rather than sent
        /// as `[]`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        earned: Option<Vec<Achievement>>,
    },

    /// A lobby action failed. Sent only to the requesting socket.
    ///
    /// Current messages: `"Room not found"` and `"Game already started"`.
    Error {
        /// Human-readable reason, rendered verbatim in the lobby.
        message: String,
    },
}
