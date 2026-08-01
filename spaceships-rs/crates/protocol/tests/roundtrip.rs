//! Round-trip tests for every protocol message.
//!
//! Each test feeds in a JSON literal shaped exactly like what the live JS code
//! puts on the wire (traced back to a specific `ws.send(JSON.stringify(...))` or
//! `broadcast(...)` call site), deserializes it into the Rust type, serializes
//! it back, and asserts the two JSON documents are equivalent. That catches
//! renamed fields, wrong casing, dropped optionals, and tag typos — the failure
//! modes that would silently break compatibility with the existing game.

use serde_json::Value;
use spaceships_protocol::{
    Achievement, Asteroid, AsteroidTier, BotAssignment, ClientMessage, MapKind, PlayerInfo,
    RoomSummary, ServerMessage, Shot, Spawn, WeaponKind,
};

// ─────────────────────────────────────────────────────────────────────────────
// Harness
// ─────────────────────────────────────────────────────────────────────────────

/// Structural JSON equality that ignores object key order and the `1` vs `1.0`
/// distinction.
///
/// `serde_json::Value`'s own `PartialEq` treats the integer `1` and the float
/// `1.0` as different numbers. JSON does not: JavaScript parses both to the same
/// IEEE-754 double, so a Rust `f64` field re-emitting `0.0` where the JS emitted
/// `0` is not a compatibility break. Everything else is compared strictly.
fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(xf), Some(yf)) => xf == yf,
            _ => x == y,
        },
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(l, r)| json_eq(l, r))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).map(|w| json_eq(v, w)).unwrap_or(false))
        }
        _ => a == b,
    }
}

fn assert_round_trip<T>(json: &str) -> T
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let msg: T = serde_json::from_str(json)
        .unwrap_or_else(|e| panic!("deserialize failed for {json}\n  error: {e}"));
    let back = serde_json::to_string(&msg).expect("serialize failed");

    let want: Value = serde_json::from_str(json).expect("test literal is not valid JSON");
    let got: Value = serde_json::from_str(&back).expect("output is not valid JSON");
    assert!(
        json_eq(&want, &got),
        "round-trip mismatch\n  in:  {json}\n  out: {back}"
    );
    msg
}

fn client(json: &str) -> ClientMessage {
    assert_round_trip::<ClientMessage>(json)
}

fn server(json: &str) -> ServerMessage {
    assert_round_trip::<ServerMessage>(json)
}

// ─────────────────────────────────────────────────────────────────────────────
// Client -> server
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn client_name() {
    // lobby.js: send({ type: 'name', name: pilotName() })
    let m = client(r#"{"type":"name","name":"Maverick"}"#);
    assert_eq!(
        m,
        ClientMessage::Name {
            name: "Maverick".into()
        }
    );
}

#[test]
fn client_list_rooms() {
    // lobby.js: send({ type: 'list-rooms' })
    let m = client(r#"{"type":"list-rooms"}"#);
    assert_eq!(m, ClientMessage::ListRooms);
}

#[test]
fn client_create() {
    // lobby.js: send({ type: 'create', private, map, allowBot })
    let m = client(r#"{"type":"create","private":false,"map":"space","allowBot":true}"#);
    assert_eq!(
        m,
        ClientMessage::Create {
            private: false,
            map: MapKind::Space,
            allow_bot: true
        }
    );

    let m = client(r#"{"type":"create","private":true,"map":"terrain","allowBot":false}"#);
    assert_eq!(
        m,
        ClientMessage::Create {
            private: true,
            map: MapKind::Terrain,
            allow_bot: false
        }
    );
}

#[test]
fn client_join() {
    // lobby.js: send({ type: 'join', code })
    let m = client(r#"{"type":"join","code":"QRZK"}"#);
    assert_eq!(
        m,
        ClientMessage::Join {
            code: "QRZK".into()
        }
    );
}

#[test]
fn client_start() {
    // lobby.js: startBtn -> send({ type: 'start' })
    assert_eq!(client(r#"{"type":"start"}"#), ClientMessage::Start);
}

#[test]
fn client_leave() {
    // lobby.js: btnLeave -> send({ type: 'leave' })
    assert_eq!(client(r#"{"type":"leave"}"#), ClientMessage::Leave);
}

#[test]
fn client_state() {
    // main.js: 20 Hz ws.send({ type: 'state', pos, quat, boost })
    let m = client(
        r#"{"type":"state","pos":[-1.9822,0.4413,-537.1509],"quat":[0.0125,-0.9987,0.0433,0.0201],"boost":true}"#,
    );
    match m {
        ClientMessage::State { pos, quat, boost } => {
            assert_eq!(pos, [-1.9822, 0.4413, -537.1509]);
            assert_eq!(quat, [0.0125, -0.9987, 0.0433, 0.0201]);
            assert!(boost);
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn client_fire_bullet() {
    // main.js: ws.send({ type: 'fire', kind: gunMode, shots }) with one shot per muzzle
    let m = client(
        r#"{"type":"fire","kind":"bullet","shots":[{"pos":[1.5,0.2,-530.4],"dir":[0.0,0.0,1.0]},{"pos":[-1.5,0.2,-530.4],"dir":[0.0,0.0,1.0]}]}"#,
    );
    match m {
        ClientMessage::Fire { kind, shots } => {
            assert_eq!(kind, WeaponKind::Bullet);
            assert_eq!(shots.len(), 2);
            assert!(shots[0].dir.is_some());
            assert!(shots[0].end.is_none());
            assert!(shots[0].target_id.is_none());
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn client_fire_beam() {
    // main.js beam branch: shots push { pos: visualStart, end }
    let m = client(
        r#"{"type":"fire","kind":"beam","shots":[{"pos":[1.5,0.2,-522.4],"end":[1.5,0.2,-122.4]}]}"#,
    );
    match m {
        ClientMessage::Fire { kind, shots } => {
            assert_eq!(kind, WeaponKind::Beam);
            assert_eq!(shots[0].end, Some([1.5, 0.2, -122.4]));
            assert!(shots[0].dir.is_none());
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn client_fire_missile() {
    // main.js KeyE branch: shots: [{ pos, dir, targetId }]
    let m = client(
        r#"{"type":"fire","kind":"missile","shots":[{"pos":[0.0,0.0,-528.0],"dir":[0.0,0.0,1.0],"targetId":4}]}"#,
    );
    match m {
        ClientMessage::Fire { kind, shots } => {
            assert_eq!(kind, WeaponKind::Missile);
            assert_eq!(shots[0].target_id, Some(4));
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn client_flare() {
    // main.js KeyQ branch
    let m =
        client(r#"{"type":"flare","pos":[3.1,-0.5,-410.2],"quat":[0.1305,-0.6597,0.0872,0.7361]}"#);
    match m {
        ClientMessage::Flare { pos, quat } => {
            assert_eq!(pos, [3.1, -0.5, -410.2]);
            assert_eq!(quat, [0.1305, -0.6597, 0.0872, 0.7361]);
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn client_hit_plain() {
    // main.js bullet/beam hit: { type: 'hit', targetId, kind }
    let m = client(r#"{"type":"hit","targetId":4,"kind":"bullet"}"#);
    assert_eq!(
        m,
        ClientMessage::Hit {
            target_id: 4,
            kind: WeaponKind::Bullet,
            from_bot_id: None
        }
    );

    let m = client(r#"{"type":"hit","targetId":4,"kind":"beam"}"#);
    assert_eq!(
        m,
        ClientMessage::Hit {
            target_id: 4,
            kind: WeaponKind::Beam,
            from_bot_id: None
        }
    );
}

#[test]
fn client_hit_from_bot() {
    // main.js: { type: 'hit', targetId, fromBotId: ownerId, kind: 'missile' }
    let m = client(r#"{"type":"hit","targetId":1,"fromBotId":-7,"kind":"missile"}"#);
    assert_eq!(
        m,
        ClientMessage::Hit {
            target_id: 1,
            kind: WeaponKind::Missile,
            from_bot_id: Some(-7)
        }
    );
}

#[test]
fn client_self_damage() {
    // main.js: brake overcharge sends dmg: 1; asteroid contact sends 15..29
    let m = client(r#"{"type":"self-damage","dmg":1}"#);
    match m {
        ClientMessage::SelfDamage { dmg } => assert_eq!(dmg, 1.0),
        other => panic!("wrong variant: {other:?}"),
    }
    let m = client(r#"{"type":"self-damage","dmg":23}"#);
    match m {
        ClientMessage::SelfDamage { dmg } => assert_eq!(dmg, 23.0),
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn client_colors() {
    // main.js: parseInt('44aaff', 16) === 4500223, parseInt('ff8800', 16) === 16746496
    let m = client(r#"{"type":"colors","hullColor":4500223,"accentColor":16746496}"#);
    assert_eq!(
        m,
        ClientMessage::Colors {
            hull_color: 0x44aaff,
            accent_color: 0xff8800
        }
    );
}

#[test]
fn client_ship_model() {
    // main.js: ws.send({ type: 'ship-model', modelUrl: ADMIN_MODEL_URL })
    let m = client(r#"{"type":"ship-model","modelUrl":"spaceshipADMIN.glb"}"#);
    assert_eq!(
        m,
        ClientMessage::ShipModel {
            model_url: "spaceshipADMIN.glb".into()
        }
    );
}

#[test]
fn client_asteroid_hit() {
    // main.js: ws.send({ type: 'asteroid-hit', id: hitAsteroidId })
    let m = client(r#"{"type":"asteroid-hit","id":37}"#);
    assert_eq!(m, ClientMessage::AsteroidHit { id: 37 });
}

#[test]
fn client_bot_state() {
    // main.js: { type: 'bot-state', botId, pos: toArray(), quat: toArray() }
    let m = client(
        r#"{"type":"bot-state","botId":-7,"pos":[10.25,4.5,499.75],"quat":[0.0,1.0,0.0,0.0]}"#,
    );
    match m {
        ClientMessage::BotState { bot_id, pos, quat } => {
            assert_eq!(bot_id, -7);
            assert_eq!(pos, [10.25, 4.5, 499.75]);
            assert_eq!(quat, [0.0, 1.0, 0.0, 0.0]);
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn client_bot_fire() {
    // main.js onFire: { type: 'bot-fire', botId, kind: 'bullet', shots: [{ pos, dir }] }
    let m = client(
        r#"{"type":"bot-fire","botId":-7,"kind":"bullet","shots":[{"pos":[10.0,4.0,494.0],"dir":[0.0,0.0,-1.0]}]}"#,
    );
    match m {
        ClientMessage::BotFire {
            bot_id,
            kind,
            ref shots,
        } => {
            assert_eq!(bot_id, -7);
            assert_eq!(kind, WeaponKind::Bullet);
            assert_eq!(shots.len(), 1);
        }
        ref other => panic!("wrong variant: {other:?}"),
    }

    // fireMissile: { type: 'bot-fire', botId, kind: 'missile', shots: [{ pos, dir, targetId }] }
    let m = client(
        r#"{"type":"bot-fire","botId":-7,"kind":"missile","shots":[{"pos":[10.0,4.0,494.0],"dir":[0.0,0.0,-1.0],"targetId":1}]}"#,
    );
    match m {
        ClientMessage::BotFire {
            kind, ref shots, ..
        } => {
            assert_eq!(kind, WeaponKind::Missile);
            assert_eq!(shots[0].target_id, Some(1));
        }
        ref other => panic!("wrong variant: {other:?}"),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Server -> client
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn server_room() {
    // index.js create/join: { type: 'room', code, host, you, private }
    let m = server(r#"{"type":"room","code":"QRZK","host":true,"you":1,"private":false}"#);
    assert_eq!(
        m,
        ServerMessage::Room {
            code: "QRZK".into(),
            host: true,
            you: 1,
            private: false
        }
    );
}

#[test]
fn server_players() {
    // index.js broadcastRoom -> roomSnapshot()
    let m = server(
        r#"{"type":"players","players":[{"id":1,"name":"Maverick","host":true,"team":0,"isBot":false,"kills":3,"deaths":1},{"id":-7,"name":"Bot [Hard]","host":false,"team":1,"isBot":true,"kills":0,"deaths":2}]}"#,
    );
    match m {
        ServerMessage::Players { players } => {
            assert_eq!(players.len(), 2);
            assert_eq!(
                players[0],
                PlayerInfo {
                    id: 1,
                    name: "Maverick".into(),
                    host: true,
                    team: Some(0),
                    is_bot: false,
                    kills: 3,
                    deaths: 1,
                }
            );
            assert!(players[1].is_bot);
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn server_players_before_start_has_null_team() {
    // roomSnapshot(): team: p.team ?? null — null until the host presses start.
    let m = server(
        r#"{"type":"players","players":[{"id":1,"name":"Player 1","host":true,"team":null,"isBot":false,"kills":0,"deaths":0}]}"#,
    );
    match m {
        ServerMessage::Players { players } => assert_eq!(players[0].team, None),
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn server_rooms_list() {
    // index.js list-rooms handler
    let m = server(
        r#"{"type":"rooms-list","rooms":[{"code":"QRZK","playerCount":2,"hostName":"Maverick"},{"code":"BLTZ","playerCount":1,"hostName":"Unknown"}]}"#,
    );
    match m {
        ServerMessage::RoomsList { rooms } => {
            assert_eq!(
                rooms[0],
                RoomSummary {
                    code: "QRZK".into(),
                    player_count: 2,
                    host_name: "Maverick".into()
                }
            );
            assert_eq!(rooms.len(), 2);
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn server_rooms_list_empty() {
    let m = server(r#"{"type":"rooms-list","rooms":[]}"#);
    assert_eq!(m, ServerMessage::RoomsList { rooms: vec![] });
}

#[test]
fn server_start() {
    // index.js start handler: broadcast({ type:'start', spawns, asteroids, map, botAssignments })
    let m = server(
        r#"{"type":"start",
            "spawns":{"1":{"team":0,"pos":[1.2,-0.5,-538.4],"quat":[0,0,0,1]},
                      "2":{"team":1,"pos":[-2.1,1.4,542.9],"quat":[0,1,0,0]}},
            "asteroids":[{"id":0,"pos":[103.4,-12.9,88.2],"rot":[1.02,2.51,0.33],"size":11.4,"hp":10,"tier":"medium","variant":3,"spin":[0.12,-0.44,0.08]}],
            "map":"space",
            "botAssignments":[]}"#,
    );
    match m {
        ServerMessage::Start {
            spawns,
            asteroids,
            map,
            bot_assignments,
        } => {
            assert_eq!(map, MapKind::Space);
            assert_eq!(spawns.len(), 2);
            assert_eq!(
                spawns[&1],
                Spawn {
                    team: 0,
                    pos: [1.2, -0.5, -538.4],
                    quat: [0.0, 0.0, 0.0, 1.0]
                }
            );
            assert_eq!(spawns[&2].quat, [0.0, 1.0, 0.0, 0.0]);
            assert_eq!(
                asteroids[0],
                Asteroid {
                    id: 0,
                    pos: [103.4, -12.9, 88.2],
                    rot: [1.02, 2.51, 0.33],
                    size: 11.4,
                    hp: 10,
                    tier: AsteroidTier::Medium,
                    variant: 3,
                    spin: [0.12, -0.44, 0.08],
                }
            );
            assert!(bot_assignments.is_empty());
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn server_start_with_bot_on_terrain() {
    // Terrain matches carry an empty asteroid list; the balance bot lands in
    // botAssignments and gets a negative id.
    let m = server(
        r#"{"type":"start",
            "spawns":{"1":{"team":0,"pos":[3.0,41.2,-1412.0],"quat":[0,0,0,1]},
                      "-7":{"team":1,"pos":[-8.0,38.9,1391.0],"quat":[0,1,0,0]}},
            "asteroids":[],
            "map":"terrain",
            "botAssignments":[{"id":-7,"team":1,"pos":[-8.0,38.9,1391.0],"quat":[0,1,0,0]}]}"#,
    );
    match m {
        ServerMessage::Start {
            spawns,
            asteroids,
            map,
            bot_assignments,
        } => {
            assert_eq!(map, MapKind::Terrain);
            assert!(asteroids.is_empty());
            assert!(spawns.contains_key(&-7));
            assert_eq!(
                bot_assignments[0],
                BotAssignment {
                    id: -7,
                    team: 1,
                    pos: [-8.0, 38.9, 1391.0],
                    quat: [0.0, 1.0, 0.0, 0.0],
                }
            );
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn server_all_asteroid_tiers_parse() {
    for (tier, want) in [
        ("small", AsteroidTier::Small),
        ("medium", AsteroidTier::Medium),
        ("big", AsteroidTier::Big),
        ("huge", AsteroidTier::Huge),
    ] {
        let json = format!(
            r#"{{"id":1,"pos":[0,0,0],"rot":[0,0,0],"size":20.0,"hp":30,"tier":"{tier}","variant":0,"spin":[0,0,0]}}"#
        );
        let a = assert_round_trip::<Asteroid>(&json);
        assert_eq!(a.tier, want);
    }
}

#[test]
fn server_state() {
    // index.js state/bot-state relay
    let m = server(
        r#"{"type":"state","id":2,"pos":[-1.98,0.44,537.15],"quat":[0.01,-0.99,0.04,0.02],"boost":false}"#,
    );
    match m {
        ServerMessage::State { id, boost, .. } => {
            assert_eq!(id, 2);
            assert!(!boost);
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn server_fire() {
    // index.js fire relay stamps the shooter id on
    let m = server(
        r#"{"type":"fire","id":2,"kind":"beam","shots":[{"pos":[1.5,0.2,522.4],"end":[1.5,0.2,122.4]}]}"#,
    );
    match m {
        ServerMessage::Fire { id, kind, shots } => {
            assert_eq!(id, 2);
            assert_eq!(kind, WeaponKind::Beam);
            assert_eq!(shots[0].end, Some([1.5, 0.2, 122.4]));
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn server_flare() {
    let m = server(
        r#"{"type":"flare","id":2,"pos":[3.1,-0.5,410.2],"quat":[0.1305,-0.6597,0.0872,0.7361]}"#,
    );
    match m {
        ServerMessage::Flare { id, .. } => assert_eq!(id, 2),
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn server_colors() {
    let m = server(r#"{"type":"colors","id":2,"hullColor":4500223,"accentColor":16746496}"#);
    assert_eq!(
        m,
        ServerMessage::Colors {
            id: 2,
            hull_color: 0x44aaff,
            accent_color: 0xff8800
        }
    );
}

#[test]
fn server_ship_model() {
    let m = server(r#"{"type":"ship-model","id":2,"modelUrl":"spaceshipADMIN.glb"}"#);
    assert_eq!(
        m,
        ServerMessage::ShipModel {
            id: 2,
            model_url: "spaceshipADMIN.glb".into()
        }
    );
}

#[test]
fn server_hp() {
    let m = server(r#"{"type":"hp","id":2,"hp":90}"#);
    assert_eq!(m, ServerMessage::Hp { id: 2, hp: 90 });
}

#[test]
fn server_death_with_killer() {
    let m = server(r#"{"type":"death","id":2,"killerId":1}"#);
    assert_eq!(
        m,
        ServerMessage::Death {
            id: 2,
            killer_id: Some(1)
        }
    );
}

#[test]
fn server_death_self_inflicted_keeps_explicit_null() {
    // index.js self-damage path: broadcast({ type:'death', id, killerId: null })
    // The key must survive the round trip as an explicit null, not be dropped.
    let m = server(r#"{"type":"death","id":2,"killerId":null}"#);
    assert_eq!(
        m,
        ServerMessage::Death {
            id: 2,
            killer_id: None
        }
    );

    let out = serde_json::to_string(&m).unwrap();
    assert!(
        out.contains("\"killerId\":null"),
        "killerId was dropped: {out}"
    );
}

#[test]
fn server_respawn() {
    let m = server(r#"{"type":"respawn","id":2,"pos":[-1.4,0.9,541.2],"quat":[0,1,0,0]}"#);
    match m {
        ServerMessage::Respawn { id, quat, .. } => {
            assert_eq!(id, 2);
            assert_eq!(quat, [0.0, 1.0, 0.0, 0.0]);
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn server_disconnect() {
    let m = server(r#"{"type":"disconnect","id":2}"#);
    assert_eq!(m, ServerMessage::Disconnect { id: 2 });
}

#[test]
fn server_asteroid_hp() {
    let m = server(r#"{"type":"asteroid-hp","id":37,"hp":9}"#);
    assert_eq!(m, ServerMessage::AsteroidHp { id: 37, hp: 9 });
}

#[test]
fn server_asteroid_destroyed() {
    let m = server(r#"{"type":"asteroid-destroyed","id":37}"#);
    assert_eq!(m, ServerMessage::AsteroidDestroyed { id: 37 });
}

#[test]
fn server_match_state() {
    // index.js 1 Hz tick: timer is fractional seconds remaining
    let m = server(r#"{"type":"match-state","timer":287.412,"teamKills":[3,5]}"#);
    match m {
        ServerMessage::MatchState { timer, team_kills } => {
            assert!((timer - 287.412).abs() < 1e-9);
            assert_eq!(team_kills, [3, 5]);
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn server_match_end() {
    let m = server(r#"{"type":"match-end","winner":1,"teamKills":[12,19]}"#);
    assert_eq!(
        m,
        ServerMessage::MatchEnd {
            winner: 1,
            team_kills: [12, 19]
        }
    );

    // endMatch(): winner is -1 on a draw.
    let m = server(r#"{"type":"match-end","winner":-1,"teamKills":[7,7]}"#);
    assert_eq!(
        m,
        ServerMessage::MatchEnd {
            winner: -1,
            team_kills: [7, 7]
        }
    );
}

#[test]
fn server_match_credits_without_achievements() {
    // endMatch() only attaches `earned` when newAchievements is non-empty, so
    // the key must stay absent (not null, not []) on the round trip.
    let m = server(r#"{"type":"match-credits","creditsEarned":180,"totalCredits":4320}"#);
    assert_eq!(
        m,
        ServerMessage::MatchCredits {
            credits_earned: 180,
            total_credits: 4320,
            earned: None
        }
    );

    let out = serde_json::to_string(&m).unwrap();
    assert!(!out.contains("earned"), "earned should be omitted: {out}");
}

#[test]
fn server_match_credits_with_achievements() {
    // db.js checkAndAwardAchievements pushes { type, label, icon, reward }.
    let m = server(
        r#"{"type":"match-credits","creditsEarned":280,"totalCredits":4420,"earned":[{"type":"first_kill","label":"First Blood","icon":"🔫","reward":100}]}"#,
    );
    match m {
        ServerMessage::MatchCredits {
            earned: Some(earned),
            ..
        } => {
            assert_eq!(
                earned[0],
                Achievement {
                    kind: "first_kill".into(),
                    label: "First Blood".into(),
                    icon: "🔫".into(),
                    reward: 100,
                }
            );
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn server_error() {
    for message in ["Room not found", "Game already started"] {
        let json = format!(r#"{{"type":"error","message":"{message}"}}"#);
        let m = server(&json);
        assert_eq!(
            m,
            ServerMessage::Error {
                message: message.into()
            }
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tag coverage and leniency
// ─────────────────────────────────────────────────────────────────────────────

/// Guards the exact set of tags each direction serializes to. If someone adds a
/// variant without updating this list, or a `rename` drifts, this fails.
#[test]
fn tag_spelling_is_exact() {
    fn tag_of<T: serde::Serialize>(v: &T) -> String {
        serde_json::to_value(v).unwrap()["type"]
            .as_str()
            .unwrap()
            .to_string()
    }

    let shot = || Shot {
        pos: [0.0; 3],
        dir: Some([0.0; 3]),
        end: None,
        target_id: None,
    };

    let client_msgs = vec![
        ClientMessage::Name {
            name: String::new(),
        },
        ClientMessage::ListRooms,
        ClientMessage::Create {
            private: false,
            map: MapKind::Space,
            allow_bot: true,
        },
        ClientMessage::Join {
            code: String::new(),
        },
        ClientMessage::Start,
        ClientMessage::Leave,
        ClientMessage::State {
            pos: [0.0; 3],
            quat: [0.0; 4],
            boost: false,
        },
        ClientMessage::Fire {
            kind: WeaponKind::Bullet,
            shots: vec![shot()],
        },
        ClientMessage::Flare {
            pos: [0.0; 3],
            quat: [0.0; 4],
        },
        ClientMessage::Hit {
            target_id: 0,
            kind: WeaponKind::Bullet,
            from_bot_id: None,
        },
        ClientMessage::SelfDamage { dmg: 0.0 },
        ClientMessage::Colors {
            hull_color: 0,
            accent_color: 0,
        },
        ClientMessage::ShipModel {
            model_url: String::new(),
        },
        ClientMessage::AsteroidHit { id: 0 },
        ClientMessage::BotState {
            bot_id: 0,
            pos: [0.0; 3],
            quat: [0.0; 4],
        },
        ClientMessage::BotFire {
            bot_id: 0,
            kind: WeaponKind::Bullet,
            shots: vec![shot()],
        },
    ];
    let got: Vec<String> = client_msgs.iter().map(tag_of).collect();
    assert_eq!(
        got,
        vec![
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
        ]
    );

    let server_msgs = vec![
        ServerMessage::Room {
            code: String::new(),
            host: false,
            you: 0,
            private: false,
        },
        ServerMessage::Players { players: vec![] },
        ServerMessage::RoomsList { rooms: vec![] },
        ServerMessage::Start {
            spawns: Default::default(),
            asteroids: vec![],
            map: MapKind::Space,
            bot_assignments: vec![],
        },
        ServerMessage::State {
            id: 0,
            pos: [0.0; 3],
            quat: [0.0; 4],
            boost: false,
        },
        ServerMessage::Fire {
            id: 0,
            kind: WeaponKind::Bullet,
            shots: vec![],
        },
        ServerMessage::Flare {
            id: 0,
            pos: [0.0; 3],
            quat: [0.0; 4],
        },
        ServerMessage::Colors {
            id: 0,
            hull_color: 0,
            accent_color: 0,
        },
        ServerMessage::ShipModel {
            id: 0,
            model_url: String::new(),
        },
        ServerMessage::Hp { id: 0, hp: 0 },
        ServerMessage::Death {
            id: 0,
            killer_id: None,
        },
        ServerMessage::Respawn {
            id: 0,
            pos: [0.0; 3],
            quat: [0.0; 4],
        },
        ServerMessage::Disconnect { id: 0 },
        ServerMessage::AsteroidHp { id: 0, hp: 0 },
        ServerMessage::AsteroidDestroyed { id: 0 },
        ServerMessage::MatchState {
            timer: 0.0,
            team_kills: [0, 0],
        },
        ServerMessage::MatchEnd {
            winner: -1,
            team_kills: [0, 0],
        },
        ServerMessage::MatchCredits {
            credits_earned: 0,
            total_credits: 0,
            earned: None,
        },
        ServerMessage::Error {
            message: String::new(),
        },
    ];
    let got: Vec<String> = server_msgs.iter().map(tag_of).collect();
    assert_eq!(
        got,
        vec![
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
        ]
    );
}

/// The JS server coerces an unknown `map` to `"space"` and an unknown weapon
/// `kind` to `"bullet"` instead of erroring. These do *not* round-trip
/// byte-for-byte by design — the point is that a garbage value is normalized,
/// exactly as the current server does.
#[test]
fn unknown_scalar_values_are_coerced_not_rejected() {
    let m: ClientMessage =
        serde_json::from_str(r#"{"type":"create","private":false,"map":"moon","allowBot":true}"#)
            .unwrap();
    assert_eq!(
        m,
        ClientMessage::Create {
            private: false,
            map: MapKind::Space,
            allow_bot: true
        }
    );

    let m: ClientMessage =
        serde_json::from_str(r#"{"type":"hit","targetId":1,"kind":"railgun"}"#).unwrap();
    assert_eq!(
        m,
        ClientMessage::Hit {
            target_id: 1,
            kind: WeaponKind::Bullet,
            from_bot_id: None
        }
    );
}

/// The JS handlers fall through on unrecognized tags rather than closing the
/// socket. A Rust port should treat a deserialization failure the same way.
#[test]
fn unknown_tag_is_an_error_the_caller_can_ignore() {
    let err = serde_json::from_str::<ClientMessage>(r#"{"type":"telemetry","x":1}"#);
    assert!(err.is_err());
}

/// Documents that both directions use `[x, y, z, w]` quaternion ordering (the
/// THREE.js `toArray()` order) with `w` last — the single easiest field in this
/// protocol to get backwards.
#[test]
fn quaternion_component_order_is_xyzw() {
    // TEAM_SPAWNS[1] is a 180-degree rotation about Y: x=0, y=1, z=0, w=0.
    let m = server(r#"{"type":"respawn","id":9,"pos":[0,0,540],"quat":[0,1,0,0]}"#);
    match m {
        ServerMessage::Respawn { quat, .. } => {
            assert_eq!(quat[1], 1.0, "y should be 1.0");
            assert_eq!(quat[3], 0.0, "w (last) should be 0.0");
        }
        other => panic!("wrong variant: {other:?}"),
    }
}
