//! End-to-end WebSocket tests: handshake, rooms, matchmaking, relay, damage.
//!
//! Every assertion here is against the raw JSON on the wire, because the
//! unmodified browser client is the consumer and it reads these keys by name.

mod harness;

use harness::{register_and_login, start, WsClient};
use serde_json::Value;

/// Parses a frame and returns its JSON.
fn j(raw: &str) -> Value {
    serde_json::from_str(raw).expect("valid JSON")
}

/// Creates a room from `client` and returns `(code, your_id)`.
async fn create_room(client: &mut WsClient) -> (String, i64) {
    client
        .send(r#"{"type":"create","private":false,"map":"space","allowBot":true}"#)
        .await;
    let raw = client.expect("room", 2000).await;
    let v = j(&raw);
    (
        v["code"].as_str().expect("code").to_string(),
        v["you"].as_i64().expect("you"),
    )
}

#[tokio::test]
async fn the_handshake_works_and_create_returns_a_room() {
    let s = start().await;
    let mut a = WsClient::connect(s.addr, None).await;

    a.send(r#"{"type":"name","name":"Maverick"}"#).await;
    let (code, you) = create_room(&mut a).await;

    assert_eq!(code.len(), 4);
    assert!(
        code.chars().all(|c| c.is_ascii_uppercase()),
        "room code should be four uppercase letters, got {code:?}"
    );
    assert_eq!(you, 1, "first connection gets id 1, like nextId++");

    // The id counter is process-wide, not per-room: a second socket creating
    // its own room is id 2.
    let mut b = WsClient::connect(s.addr, None).await;
    let (code_b, you_b) = create_room(&mut b).await;
    assert_eq!(you_b, 2);
    assert_ne!(code_b, code, "room codes are unique");
}

#[tokio::test]
async fn a_room_ack_has_the_exact_js_key_order() {
    let s = start().await;
    let mut a = WsClient::connect(s.addr, None).await;
    a.send(r#"{"type":"create","private":true,"map":"terrain","allowBot":false}"#)
        .await;
    let raw = a.expect("room", 2000).await;
    let code = j(&raw)["code"].as_str().unwrap().to_string();
    assert_eq!(
        raw,
        format!(r#"{{"type":"room","code":"{code}","host":true,"you":1,"private":true}}"#)
    );
}

#[tokio::test]
async fn joining_broadcasts_the_roster_to_everyone() {
    let s = start().await;
    let mut a = WsClient::connect(s.addr, None).await;
    a.send(r#"{"type":"name","name":"Maverick"}"#).await;
    let (code, _) = create_room(&mut a).await;

    // The host sees itself alone first.
    let raw = a.expect("players", 2000).await;
    let v = j(&raw);
    let players = v["players"].as_array().unwrap();
    assert_eq!(players.len(), 1);
    assert_eq!(players[0]["name"], "Maverick");
    assert_eq!(players[0]["host"], true);
    assert_eq!(players[0]["isBot"], false);
    assert!(players[0]["team"].is_null(), "no team until start");
    assert_eq!(players[0]["kills"], 0);
    assert_eq!(players[0]["deaths"], 0);

    let mut b = WsClient::connect(s.addr, None).await;
    b.send(r#"{"type":"name","name":"Goose"}"#).await;
    b.send(&format!(r#"{{"type":"join","code":"{code}"}}"#))
        .await;

    let raw = b.expect("room", 2000).await;
    let v = j(&raw);
    assert_eq!(v["host"], false);
    assert_eq!(v["you"], 2);
    assert_eq!(v["code"], code);

    // Both sides get the two-player roster, in join order.
    for client in [&mut a, &mut b] {
        let raw = client.expect("players", 2000).await;
        let v = j(&raw);
        let players = v["players"].as_array().unwrap();
        assert_eq!(players.len(), 2, "{raw}");
        assert_eq!(players[0]["name"], "Maverick");
        assert_eq!(players[0]["host"], true);
        assert_eq!(players[1]["name"], "Goose");
        assert_eq!(players[1]["host"], false);
    }
}

#[tokio::test]
async fn joining_is_case_insensitive_and_reports_the_js_errors() {
    let s = start().await;
    let mut a = WsClient::connect(s.addr, None).await;
    let (code, _) = create_room(&mut a).await;

    let mut b = WsClient::connect(s.addr, None).await;
    b.send(r#"{"type":"join","code":"ZZZZ"}"#).await;
    assert_eq!(
        b.expect("error", 2000).await,
        r#"{"type":"error","message":"Room not found"}"#
    );

    // Lowercase is uppercased server-side.
    b.send(&format!(
        r#"{{"type":"join","code":"{}"}}"#,
        code.to_lowercase()
    ))
    .await;
    let v = j(&b.expect("room", 2000).await);
    assert_eq!(v["code"], code);

    // Once started, further joins are refused.
    a.send(r#"{"type":"start"}"#).await;
    a.expect("start", 2000).await;
    let mut c = WsClient::connect(s.addr, None).await;
    c.send(&format!(r#"{{"type":"join","code":"{code}"}}"#))
        .await;
    assert_eq!(
        c.expect("error", 2000).await,
        r#"{"type":"error","message":"Game already started"}"#
    );
}

#[tokio::test]
async fn list_rooms_hides_private_and_started_rooms() {
    let s = start().await;

    let mut pub_host = WsClient::connect(s.addr, None).await;
    pub_host.send(r#"{"type":"name","name":"Public"}"#).await;
    pub_host
        .send(r#"{"type":"create","private":false,"map":"space","allowBot":true}"#)
        .await;
    let public_code = j(&pub_host.expect("room", 2000).await)["code"]
        .as_str()
        .unwrap()
        .to_string();

    let mut priv_host = WsClient::connect(s.addr, None).await;
    priv_host
        .send(r#"{"type":"create","private":true,"map":"space","allowBot":true}"#)
        .await;
    priv_host.expect("room", 2000).await;

    let mut browser = WsClient::connect(s.addr, None).await;
    browser.send(r#"{"type":"list-rooms"}"#).await;
    let v = j(&browser.expect("rooms-list", 2000).await);
    let rooms = v["rooms"].as_array().unwrap();
    assert_eq!(rooms.len(), 1, "the private room must not be listed");
    assert_eq!(rooms[0]["code"], public_code);
    assert_eq!(rooms[0]["playerCount"], 1);
    assert_eq!(rooms[0]["hostName"], "Public");

    // Starting removes it from the browser too.
    pub_host.send(r#"{"type":"start"}"#).await;
    pub_host.expect("start", 2000).await;
    browser.send(r#"{"type":"list-rooms"}"#).await;
    let v = j(&browser.expect("rooms-list", 2000).await);
    assert_eq!(v["rooms"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn start_assigns_alternating_teams_and_ships_the_asteroid_field() {
    let s = start().await;
    let mut a = WsClient::connect(s.addr, None).await;
    let (code, _) = create_room(&mut a).await;
    let mut b = WsClient::connect(s.addr, None).await;
    b.send(&format!(r#"{{"type":"join","code":"{code}"}}"#))
        .await;
    b.expect("room", 2000).await;

    a.send(r#"{"type":"start"}"#).await;
    let raw = a.expect("start", 2000).await;
    let v = j(&raw);

    assert_eq!(v["map"], "space");
    // Two humans is already balanced, so no bot is added.
    assert_eq!(v["botAssignments"].as_array().unwrap().len(), 0);

    let spawns = v["spawns"].as_object().unwrap();
    assert_eq!(spawns.len(), 2);
    // `humanIdx++ % 2` in join order.
    assert_eq!(spawns["1"]["team"], 0);
    assert_eq!(spawns["2"]["team"], 1);
    // Team 0 faces +Z from z=-540, team 1 mirrors it.
    assert!(spawns["1"]["pos"][2].as_f64().unwrap() < -530.0);
    assert!(spawns["2"]["pos"][2].as_f64().unwrap() > 530.0);
    assert_eq!(spawns["1"]["quat"], serde_json::json!([0.0, 0.0, 0.0, 1.0]));
    assert_eq!(spawns["2"]["quat"], serde_json::json!([0.0, 1.0, 0.0, 0.0]));

    let asteroids = v["asteroids"].as_array().unwrap();
    assert_eq!(asteroids.len(), 60, "60 rocks, like generateAsteroidField");
    for (i, rock) in asteroids.iter().enumerate() {
        assert_eq!(rock["id"], i, "ids are dense and 0-based");
        assert!(rock["size"].as_f64().unwrap() >= 5.0);
        assert!(rock["variant"].as_u64().unwrap() < 6);
        assert_eq!(rock["pos"].as_array().unwrap().len(), 3);
        // The moon is radius 80 at the origin. The JS server generates rocks
        // inside it; this one must not.
        let p = rock["pos"].as_array().unwrap();
        let d = (0..3)
            .map(|k| p[k].as_f64().unwrap().powi(2))
            .sum::<f64>()
            .sqrt();
        assert!(d > 80.0, "rock {i} at distance {d} is inside the moon");
    }

    // Both sides get the same field, and the roster now shows teams.
    let vb = j(&b.expect("start", 2000).await);
    assert_eq!(vb["asteroids"], v["asteroids"]);
    // Only the host receives bot assignments; here neither does.
    assert_eq!(vb["botAssignments"].as_array().unwrap().len(), 0);

    let roster = j(&a.expect("players", 2000).await);
    let players = roster["players"].as_array().unwrap();
    assert_eq!(players[0]["team"], 0);
    assert_eq!(players[1]["team"], 1);
}

#[tokio::test]
async fn an_odd_lobby_gets_a_balance_bot_assigned_to_the_host() {
    let s = start().await;
    let mut a = WsClient::connect(s.addr, None).await;
    let (_code, _) = create_room(&mut a).await;

    a.send(r#"{"type":"start"}"#).await;
    let v = j(&a.expect("start", 2000).await);
    let bots = v["botAssignments"].as_array().unwrap();
    assert_eq!(bots.len(), 1, "one human means one balance bot");
    let bot = &bots[0];
    let bot_id = bot["id"].as_i64().unwrap();
    assert!(bot_id < 0, "bot ids are negative: {bot_id}");
    assert_eq!(bot["team"], 1, "the human took team 0");

    // The bot appears in `spawns` and on the roster, flagged as a bot.
    assert!(v["spawns"][bot_id.to_string()].is_object());
    let roster = j(&a.expect("players", 2000).await);
    let players = roster["players"].as_array().unwrap();
    assert_eq!(players.len(), 2);
    assert_eq!(players[1]["isBot"], true);
    assert_eq!(players[1]["name"], "Bot [Hard]");
    assert_eq!(players[1]["id"], bot_id);
}

#[tokio::test]
async fn allow_bot_false_leaves_the_teams_uneven() {
    let s = start().await;
    let mut a = WsClient::connect(s.addr, None).await;
    a.send(r#"{"type":"create","private":false,"map":"space","allowBot":false}"#)
        .await;
    a.expect("room", 2000).await;
    a.send(r#"{"type":"start"}"#).await;
    let v = j(&a.expect("start", 2000).await);
    assert_eq!(v["botAssignments"].as_array().unwrap().len(), 0);
    assert_eq!(v["spawns"].as_object().unwrap().len(), 1);
}

#[tokio::test]
async fn the_terrain_map_spawns_on_the_airfields_with_no_asteroids() {
    let s = start().await;
    let mut a = WsClient::connect(s.addr, None).await;
    a.send(r#"{"type":"create","private":false,"map":"terrain","allowBot":false}"#)
        .await;
    a.expect("room", 2000).await;
    a.send(r#"{"type":"start"}"#).await;
    let v = j(&a.expect("start", 2000).await);
    assert_eq!(v["map"], "terrain");
    assert_eq!(v["asteroids"].as_array().unwrap().len(), 0);
    let pos = &v["spawns"]["1"]["pos"];
    assert!(
        (pos[1].as_f64().unwrap() - 40.0).abs() < 6.0,
        "above the runway"
    );
    assert!(pos[2].as_f64().unwrap() < -1380.0);
}

#[tokio::test]
async fn state_and_fire_are_relayed_to_peers_but_never_echoed() {
    let s = start().await;
    let mut a = WsClient::connect(s.addr, None).await;
    let (code, _) = create_room(&mut a).await;
    let mut b = WsClient::connect(s.addr, None).await;
    b.send(&format!(r#"{{"type":"join","code":"{code}"}}"#))
        .await;
    b.expect("room", 2000).await;
    a.send(r#"{"type":"start"}"#).await;
    a.expect("start", 2000).await;
    b.expect("start", 2000).await;

    a.send(r#"{"type":"state","pos":[1,2,3],"quat":[0,0,0,1],"boost":true}"#)
        .await;
    let raw = b.expect("state", 2000).await;
    assert_eq!(
        raw,
        r#"{"type":"state","id":1,"pos":[1.0,2.0,3.0],"quat":[0.0,0.0,0.0,1.0],"boost":true}"#
    );
    // The sender never sees its own state back.
    a.expect_none("state", 200).await;

    a.send(r#"{"type":"fire","kind":"bullet","shots":[{"pos":[1,2,3],"dir":[0,0,1]}]}"#)
        .await;
    let raw = b.expect("fire", 2000).await;
    let v = j(&raw);
    assert_eq!(v["id"], 1);
    assert_eq!(v["kind"], "bullet");
    assert_eq!(v["shots"][0]["dir"], serde_json::json!([0.0, 0.0, 1.0]));
    assert!(
        v["shots"][0].get("end").is_none(),
        "absent keys stay absent"
    );
    assert!(v["shots"][0].get("targetId").is_none());
    a.expect_none("fire", 200).await;

    // Beams carry `end` instead of `dir`.
    a.send(r#"{"type":"fire","kind":"beam","shots":[{"pos":[0,0,0],"end":[0,0,900]}]}"#)
        .await;
    let v = j(&b.expect("fire", 2000).await);
    assert_eq!(v["kind"], "beam");
    assert!(v["shots"][0].get("dir").is_none());
    assert_eq!(v["shots"][0]["end"], serde_json::json!([0.0, 0.0, 900.0]));

    a.send(r#"{"type":"flare","pos":[4,5,6],"quat":[0,0,0,1]}"#)
        .await;
    let v = j(&b.expect("flare", 2000).await);
    assert_eq!(v["id"], 1);
    assert_eq!(v["pos"], serde_json::json!([4.0, 5.0, 6.0]));

    a.send(r#"{"type":"colors","hullColor":16711680,"accentColor":65280}"#)
        .await;
    assert_eq!(
        b.expect("colors", 2000).await,
        r#"{"type":"colors","id":1,"hullColor":16711680,"accentColor":65280}"#
    );
}

#[tokio::test]
async fn relay_is_suppressed_before_the_match_starts() {
    let s = start().await;
    let mut a = WsClient::connect(s.addr, None).await;
    let (code, _) = create_room(&mut a).await;
    let mut b = WsClient::connect(s.addr, None).await;
    b.send(&format!(r#"{{"type":"join","code":"{code}"}}"#))
        .await;
    b.expect("room", 2000).await;
    b.expect("players", 2000).await;

    // `state`, `fire` and `flare` all require `room.started`.
    a.send(r#"{"type":"state","pos":[1,2,3],"quat":[0,0,0,1],"boost":false}"#)
        .await;
    a.send(r#"{"type":"fire","kind":"bullet","shots":[]}"#)
        .await;
    b.expect_none("state", 300).await;

    // `colors` does *not* — it is relayed in the lobby so late joiners can
    // paint the ship before the match begins.
    a.send(r#"{"type":"colors","hullColor":1,"accentColor":2}"#)
        .await;
    b.expect("colors", 2000).await;
}

#[tokio::test]
async fn ship_model_rejects_remote_urls() {
    let s = start().await;
    let mut a = WsClient::connect(s.addr, None).await;
    let (code, _) = create_room(&mut a).await;
    let mut b = WsClient::connect(s.addr, None).await;
    b.send(&format!(r#"{{"type":"join","code":"{code}"}}"#))
        .await;
    b.expect("room", 2000).await;

    for bad in [
        r#"{"type":"ship-model","modelUrl":"http://evil.example/x.glb"}"#,
        r#"{"type":"ship-model","modelUrl":"https://evil.example/x.glb"}"#,
        r#"{"type":"ship-model","modelUrl":"//evil.example/x.glb"}"#,
    ] {
        a.send(bad).await;
    }
    b.expect_none("ship-model", 300).await;

    a.send(r#"{"type":"ship-model","modelUrl":"models/admin_ship.glb"}"#)
        .await;
    assert_eq!(
        b.expect("ship-model", 2000).await,
        r#"{"type":"ship-model","id":1,"modelUrl":"models/admin_ship.glb"}"#
    );
}

#[tokio::test]
async fn asteroid_hits_are_tracked_server_side() {
    let s = start().await;
    let mut a = WsClient::connect(s.addr, None).await;
    let (code, _) = create_room(&mut a).await;
    let mut b = WsClient::connect(s.addr, None).await;
    b.send(&format!(r#"{{"type":"join","code":"{code}"}}"#))
        .await;
    b.expect("room", 2000).await;
    a.send(r#"{"type":"start"}"#).await;
    let start_v = j(&a.expect("start", 2000).await);
    b.expect("start", 2000).await;

    // Find a 5 HP rock so the test does not have to send 50 messages.
    let asteroids = start_v["asteroids"].as_array().unwrap();
    let small = asteroids
        .iter()
        .find(|r| r["hp"] == 5)
        .expect("a small rock exists");
    let id = small["id"].as_u64().unwrap();

    // Four hits chip it; the fifth destroys it. Both are broadcast to the whole
    // room, sender included.
    for expected_hp in [4, 3, 2, 1] {
        a.send(&format!(r#"{{"type":"asteroid-hit","id":{id}}}"#))
            .await;
        assert_eq!(
            a.expect("asteroid-hp", 2000).await,
            format!(r#"{{"type":"asteroid-hp","id":{id},"hp":{expected_hp}}}"#)
        );
        b.expect("asteroid-hp", 2000).await;
    }
    a.send(&format!(r#"{{"type":"asteroid-hit","id":{id}}}"#))
        .await;
    assert_eq!(
        a.expect("asteroid-destroyed", 2000).await,
        format!(r#"{{"type":"asteroid-destroyed","id":{id}}}"#)
    );
    b.expect("asteroid-destroyed", 2000).await;

    // A late duplicate report is a no-op, not a second destruction.
    a.send(&format!(r#"{{"type":"asteroid-hit","id":{id}}}"#))
        .await;
    a.expect_none("asteroid-destroyed", 300).await;
}

#[tokio::test]
async fn hits_respect_spawn_protection_then_damage_kill_and_respawn() {
    let s = start().await;
    let mut a = WsClient::connect(s.addr, None).await;
    let (code, _) = create_room(&mut a).await;
    let mut b = WsClient::connect(s.addr, None).await;
    b.send(&format!(r#"{{"type":"join","code":"{code}"}}"#))
        .await;
    b.expect("room", 2000).await;
    a.send(r#"{"type":"start"}"#).await;
    a.expect("start", 2000).await;
    b.expect("start", 2000).await;

    // Inside the 2 s spawn-protection window every hit is dropped.
    a.send(r#"{"type":"hit","targetId":2,"kind":"missile"}"#)
        .await;
    a.expect_none("hp", 500).await;

    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;

    // Missiles do 50. Two of them kill, but the rate limit is 400 ms.
    a.send(r#"{"type":"hit","targetId":2,"kind":"missile"}"#)
        .await;
    assert_eq!(
        a.expect("hp", 2000).await,
        r#"{"type":"hp","id":2,"hp":50}"#
    );
    b.expect("hp", 2000).await;

    // An immediate second missile is rate-limited away.
    a.send(r#"{"type":"hit","targetId":2,"kind":"missile"}"#)
        .await;
    a.expect_none("hp", 300).await;

    tokio::time::sleep(std::time::Duration::from_millis(450)).await;
    a.send(r#"{"type":"hit","targetId":2,"kind":"missile"}"#)
        .await;
    assert_eq!(a.expect("hp", 2000).await, r#"{"type":"hp","id":2,"hp":0}"#);
    assert_eq!(
        a.expect("death", 2000).await,
        r#"{"type":"death","id":2,"killerId":1}"#
    );

    // The roster refresh carries the updated score.
    let roster = j(&a.expect("players", 2000).await);
    let players = roster["players"].as_array().unwrap();
    assert_eq!(players[0]["kills"], 1);
    assert_eq!(players[1]["deaths"], 1);

    // Respawn arrives 2 s later, at the victim's own team spawn.
    let raw = b.expect("respawn", 4000).await;
    let v = j(&raw);
    assert_eq!(v["id"], 2);
    assert!(v["pos"][2].as_f64().unwrap() > 530.0, "team 1 side");
    assert_eq!(v["quat"], serde_json::json!([0.0, 1.0, 0.0, 0.0]));
}

#[tokio::test]
async fn friendly_fire_and_self_hits_are_dropped() {
    let s = start().await;
    let mut a = WsClient::connect(s.addr, None).await;
    let (code, _) = create_room(&mut a).await;
    // Three players: 1 and 3 land on team 0, 2 on team 1.
    let mut b = WsClient::connect(s.addr, None).await;
    b.send(&format!(r#"{{"type":"join","code":"{code}"}}"#))
        .await;
    b.expect("room", 2000).await;
    let mut c = WsClient::connect(s.addr, None).await;
    c.send(&format!(r#"{{"type":"join","code":"{code}"}}"#))
        .await;
    c.expect("room", 2000).await;

    a.send(r#"{"type":"start"}"#).await;
    let v = j(&a.expect("start", 2000).await);
    assert_eq!(v["spawns"]["1"]["team"], 0);
    assert_eq!(v["spawns"]["2"]["team"], 1);
    assert_eq!(v["spawns"]["3"]["team"], 0);
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;

    // Same team: dropped.
    a.send(r#"{"type":"hit","targetId":3,"kind":"bullet"}"#)
        .await;
    a.expect_none("hp", 400).await;

    // Self: dropped.
    a.send(r#"{"type":"hit","targetId":1,"kind":"bullet"}"#)
        .await;
    a.expect_none("hp", 400).await;

    // Cross-team: lands, for 10.
    a.send(r#"{"type":"hit","targetId":2,"kind":"bullet"}"#)
        .await;
    assert_eq!(
        a.expect("hp", 2000).await,
        r#"{"type":"hp","id":2,"hp":90}"#
    );
}

#[tokio::test]
async fn self_damage_is_applied_verbatim_and_can_kill() {
    let s = start().await;
    let mut a = WsClient::connect(s.addr, None).await;
    let (code, _) = create_room(&mut a).await;
    let mut b = WsClient::connect(s.addr, None).await;
    b.send(&format!(r#"{{"type":"join","code":"{code}"}}"#))
        .await;
    b.expect("room", 2000).await;
    a.send(r#"{"type":"start"}"#).await;
    a.expect("start", 2000).await;
    b.expect("start", 2000).await;

    // No spawn-protection check on self-damage — it applies immediately.
    a.send(r#"{"type":"self-damage","dmg":25}"#).await;
    assert_eq!(
        a.expect("hp", 2000).await,
        r#"{"type":"hp","id":1,"hp":75}"#
    );

    // Zero and negative damage are dropped.
    a.send(r#"{"type":"self-damage","dmg":0}"#).await;
    a.send(r#"{"type":"self-damage","dmg":-10}"#).await;
    a.expect_none("hp", 300).await;

    // Damage is clamped to SHIP_MAX_HP, and a self-kill has a null killer.
    a.send(r#"{"type":"self-damage","dmg":9999}"#).await;
    assert_eq!(a.expect("hp", 2000).await, r#"{"type":"hp","id":1,"hp":0}"#);
    assert_eq!(
        a.expect("death", 2000).await,
        r#"{"type":"death","id":1,"killerId":null}"#
    );
}

#[tokio::test]
async fn bot_state_and_fire_are_host_only() {
    let s = start().await;
    let mut a = WsClient::connect(s.addr, None).await;
    let (code, _) = create_room(&mut a).await;
    let mut b = WsClient::connect(s.addr, None).await;
    b.send(&format!(r#"{{"type":"join","code":"{code}"}}"#))
        .await;
    b.expect("room", 2000).await;
    let mut c = WsClient::connect(s.addr, None).await;
    c.send(&format!(r#"{{"type":"join","code":"{code}"}}"#))
        .await;
    c.expect("room", 2000).await;

    a.send(r#"{"type":"start"}"#).await;
    let v = j(&a.expect("start", 2000).await);
    let bot_id = v["botAssignments"][0]["id"].as_i64().expect("a bot");
    b.expect("start", 2000).await;

    // The host may drive it; the relay forces `boost: false`. Every other
    // socket in the room receives it — including `c`, which must be drained
    // before the negative assertion below or it would see this frame.
    a.send(&format!(
        r#"{{"type":"bot-state","botId":{bot_id},"pos":[9,8,7],"quat":[0,0,0,1]}}"#
    ))
    .await;
    let expected = format!(
        r#"{{"type":"state","id":{bot_id},"pos":[9.0,8.0,7.0],"quat":[0.0,0.0,0.0,1.0],"boost":false}}"#
    );
    assert_eq!(b.expect("state", 2000).await, expected);
    assert_eq!(c.expect("state", 2000).await, expected);

    // A non-host driving the same bot is ignored.
    b.send(&format!(
        r#"{{"type":"bot-state","botId":{bot_id},"pos":[1,1,1],"quat":[0,0,0,1]}}"#
    ))
    .await;
    c.expect_none("state", 300).await;

    // Bot weapons narrow to bullet unless they are exactly `missile`.
    a.send(&format!(
        r#"{{"type":"bot-fire","botId":{bot_id},"kind":"beam","shots":[{{"pos":[0,0,0],"dir":[0,0,1]}}]}}"#
    ))
    .await;
    let v = j(&b.expect("fire", 2000).await);
    assert_eq!(v["kind"], "bullet", "a bot can never emit a beam");
    assert_eq!(v["id"], bot_id);

    a.send(&format!(
        r#"{{"type":"bot-fire","botId":{bot_id},"kind":"missile","shots":[{{"pos":[0,0,0],"dir":[0,0,1],"targetId":2}}]}}"#
    ))
    .await;
    let v = j(&b.expect("fire", 2000).await);
    assert_eq!(v["kind"], "missile");
    assert_eq!(v["shots"][0]["targetId"], 2);
}

#[tokio::test]
async fn a_bot_hit_is_only_accepted_from_the_bot_host() {
    let s = start().await;
    let mut a = WsClient::connect(s.addr, None).await;
    let (code, _) = create_room(&mut a).await;
    let mut b = WsClient::connect(s.addr, None).await;
    b.send(&format!(r#"{{"type":"join","code":"{code}"}}"#))
        .await;
    b.expect("room", 2000).await;
    let mut c = WsClient::connect(s.addr, None).await;
    c.send(&format!(r#"{{"type":"join","code":"{code}"}}"#))
        .await;
    c.expect("room", 2000).await;

    a.send(r#"{"type":"start"}"#).await;
    let v = j(&a.expect("start", 2000).await);
    let bot_id = v["botAssignments"][0]["id"].as_i64().expect("a bot");
    let bot_team = v["botAssignments"][0]["team"].as_i64().unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;

    // Player 2 is on team 1; the bot fills the smaller team. Pick a target on
    // the other side from the bot.
    let target = if bot_team == 1 { 1 } else { 2 };

    // A non-host claiming a bot hit is refused.
    b.send(&format!(
        r#"{{"type":"hit","targetId":{target},"fromBotId":{bot_id},"kind":"bullet"}}"#
    ))
    .await;
    b.expect_none("hp", 400).await;

    // The host's identical report lands, and credit goes to the *bot*.
    a.send(&format!(
        r#"{{"type":"hit","targetId":{target},"fromBotId":{bot_id},"kind":"missile"}}"#
    ))
    .await;
    let v = j(&a.expect("hp", 2000).await);
    assert_eq!(v["id"], target);
    assert_eq!(v["hp"], 50);
}

#[tokio::test]
async fn a_name_change_updates_the_roster() {
    let s = start().await;
    let mut a = WsClient::connect(s.addr, None).await;
    let (_code, _) = create_room(&mut a).await;
    a.expect("players", 2000).await;

    a.send(r#"{"type":"name","name":"  Ice<>man!!  "}"#).await;
    let v = j(&a.expect("players", 2000).await);
    assert_eq!(v["players"][0]["name"], "Iceman");

    // An empty result keeps the previous name and sends nothing.
    a.send(r#"{"type":"name","name":"!!!"}"#).await;
    a.expect_none("players", 300).await;
}

#[tokio::test]
async fn leaving_migrates_the_host_and_disconnect_only_fires_mid_match() {
    let s = start().await;
    let mut a = WsClient::connect(s.addr, None).await;
    let (code, _) = create_room(&mut a).await;
    let mut b = WsClient::connect(s.addr, None).await;
    b.send(&format!(r#"{{"type":"join","code":"{code}"}}"#))
        .await;
    b.expect("room", 2000).await;
    b.expect("players", 2000).await;

    // Leaving the lobby before start produces no `disconnect`.
    a.send(r#"{"type":"leave"}"#).await;
    let v = j(&b.expect("players", 2000).await);
    let players = v["players"].as_array().unwrap();
    assert_eq!(players.len(), 1);
    assert_eq!(players[0]["id"], 2);
    assert_eq!(players[0]["host"], true, "host migrated to the survivor");
    b.expect_none("disconnect", 300).await;

    // Now start and drop a socket mid-match: that *does* announce.
    b.send(r#"{"type":"start"}"#).await;
    b.expect("start", 2000).await;
    let mut c = WsClient::connect(s.addr, None).await;
    c.send(&format!(r#"{{"type":"join","code":"{code}"}}"#))
        .await;
    // The room has started, so c cannot join; use a fresh room instead.
    c.expect("error", 2000).await;
    c.close().await;

    let mut d = WsClient::connect(s.addr, None).await;
    let (code2, _) = create_room(&mut d).await;
    let mut e = WsClient::connect(s.addr, None).await;
    e.send(&format!(r#"{{"type":"join","code":"{code2}"}}"#))
        .await;
    let e_id = j(&e.expect("room", 2000).await)["you"].as_i64().unwrap();
    d.send(r#"{"type":"start"}"#).await;
    d.expect("start", 2000).await;
    e.close().await;
    let v = j(&d.expect("disconnect", 2000).await);
    assert_eq!(v["id"], e_id);
}

#[tokio::test]
async fn an_authenticated_socket_takes_the_pilot_callsign_as_its_name() {
    let s = start().await;
    let (_, token) = register_and_login(s.addr, "Wolfman").await;
    let mut a = WsClient::connect(s.addr, Some(&token)).await;
    create_room(&mut a).await;
    let v = j(&a.expect("players", 2000).await);
    assert_eq!(v["players"][0]["name"], "Wolfman");
}

#[tokio::test]
async fn an_invalid_token_degrades_to_guest_rather_than_refusing_the_upgrade() {
    let s = start().await;
    // A syntactically valid but unverifiable token.
    let mut a = WsClient::connect(s.addr, Some("eyJhbGciOiJIUzI1NiJ9.e30.bogus")).await;
    create_room(&mut a).await;
    let v = j(&a.expect("players", 2000).await);
    assert_eq!(v["players"][0]["name"], "Player 1", "guest naming");
}

#[tokio::test]
async fn malformed_frames_are_ignored_without_dropping_the_socket() {
    let s = start().await;
    let mut a = WsClient::connect(s.addr, None).await;
    for junk in [
        "not json",
        "[]",
        "null",
        "123",
        r#""just a string""#,
        r#"{"no":"type"}"#,
        r#"{"type":"nonsense"}"#,
        r#"{"type":42}"#,
        r#"{"type":"hit"}"#,
    ] {
        a.send(junk).await;
    }
    // The socket is still usable.
    let (code, _) = create_room(&mut a).await;
    assert_eq!(code.len(), 4);
}

#[tokio::test]
async fn a_null_frame_does_not_kill_the_connection() {
    // This is a real divergence from the JS server, verified against a running
    // `node server/index.js`.
    //
    // `handleConnection` guards with `try { msg = JSON.parse(data) } catch
    // { return }`, which is clearly meant to ignore junk — but
    // `JSON.parse("null")` *succeeds* and yields `null`, so the catch never
    // fires. The next line, `msg.type === 'name'`, then throws
    //
    //     TypeError: Cannot read properties of null (reading 'type')
    //
    // which propagates out of the message handler into `WSConn.parse()`'s
    // `catch (e) { this.fail(e) }`, and `fail` closes the socket. Any client
    // can therefore drop its own connection — mid-match, losing its unsaved
    // kills — by sending four bytes. Observed on the live server as
    // `ws 1 error: Cannot read properties of null (reading 'type')` and a
    // client-side close code 1006.
    //
    // The port ignores the frame, which is what the `catch { return }` was for.
    let s = start().await;
    let mut a = WsClient::connect(s.addr, None).await;
    let (code, _) = create_room(&mut a).await;
    a.expect("players", 2000).await;

    let mut b = WsClient::connect(s.addr, None).await;
    b.send(&format!(r#"{{"type":"join","code":"{code}"}}"#))
        .await;
    b.expect("room", 2000).await;
    a.expect("players", 2000).await;
    a.send(r#"{"type":"start"}"#).await;
    a.expect("start", 2000).await;
    b.expect("start", 2000).await;

    b.send("null").await;

    // On the JS server this is where `b` would be gone and `a` would receive a
    // `disconnect`. Here the socket survives and keeps relaying.
    a.expect_none("disconnect", 400).await;
    b.send(r#"{"type":"state","pos":[7,7,7],"quat":[0,0,0,1],"boost":false}"#)
        .await;
    let v = j(&a.expect("state", 2000).await);
    assert_eq!(v["id"], 2, "the socket that sent `null` is still relaying");
}

#[tokio::test]
async fn the_match_clock_ticks_once_a_second() {
    let s = start().await;
    let mut a = WsClient::connect(s.addr, None).await;
    create_room(&mut a).await;
    a.send(r#"{"type":"start"}"#).await;
    a.expect("start", 2000).await;

    let v = j(&a.expect("match-state", 2500).await);
    let t = v["timer"].as_f64().expect("timer");
    // Counts down from 300 s.
    assert!((298.0..=300.0).contains(&t), "timer was {t}");
    assert_eq!(v["teamKills"], serde_json::json!([0, 0]));

    let v2 = j(&a.expect("match-state", 2500).await);
    let t2 = v2["timer"].as_f64().unwrap();
    assert!(t2 < t, "the clock must go down: {t} then {t2}");
}
