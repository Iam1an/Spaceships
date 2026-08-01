//! End-to-end tests for every route ported from `server/index.js`.
//!
//! These drive a real server over a real TCP socket against a real copy of
//! `pilots.db`, and assert on raw response bytes wherever key order or number
//! formatting is part of the contract.

mod harness;

use harness::{get, post, put, register_and_login, request, start};

#[tokio::test]
async fn register_validates_exactly_like_the_js() {
    let s = start().await;

    // Too short, and the en dash in the message is the one from `db.js`.
    let res = post(
        s.addr,
        "/api/register",
        None,
        r#"{"username":"ab","password":"longenough"}"#,
    )
    .await;
    assert_eq!(res.status, 400);
    assert_eq!(
        res.body,
        r#"{"ok":false,"error":"Callsign must be 3–20 alphanumeric characters"}"#
    );

    // Too long: 21 characters.
    let res = post(
        s.addr,
        "/api/register",
        None,
        r#"{"username":"abcdefghijklmnopqrstu","password":"longenough"}"#,
    )
    .await;
    assert_eq!(res.status, 400);

    // Password under six characters.
    let res = post(
        s.addr,
        "/api/register",
        None,
        r#"{"username":"Goose","password":"five5"}"#,
    )
    .await;
    assert_eq!(res.status, 400);
    assert_eq!(
        res.body,
        r#"{"ok":false,"error":"Password must be at least 6 characters"}"#
    );

    // A missing body is `{}`, which fails callsign validation rather than
    // erroring.
    let res = request(s.addr, "POST", "/api/register", &[], None).await;
    assert_eq!(res.status, 400);

    // Success is 201 with the *sanitized* callsign echoed back.
    let res = post(
        s.addr,
        "/api/register",
        None,
        r#"{"username":"Mav erick!","password":"testpassword"}"#,
    )
    .await;
    assert_eq!(res.status, 201);
    // Spaces and punctuation are stripped by `/[^A-Za-z0-9_\-]/g`, unlike the
    // WebSocket `name` sanitizer which keeps spaces.
    assert_eq!(res.body, r#"{"ok":true,"username":"Maverick"}"#);

    // Duplicate callsigns are 409, and the column is COLLATE NOCASE so casing
    // does not dodge it.
    let res = post(
        s.addr,
        "/api/register",
        None,
        r#"{"username":"MAVERICK","password":"testpassword"}"#,
    )
    .await;
    assert_eq!(res.status, 409);
    assert_eq!(res.body, r#"{"ok":false,"error":"Callsign already taken"}"#);
}

#[tokio::test]
async fn login_returns_the_full_pilot_payload_in_js_key_order() {
    let s = start().await;
    let (_, _) = register_and_login(s.addr, "Iceman").await;

    let res = post(
        s.addr,
        "/api/login",
        None,
        r#"{"username":"iceman","password":"testpassword"}"#,
    )
    .await;
    assert_eq!(res.status, 200);
    let v = res.json();
    assert_eq!(v["ok"], true);
    // Stored casing wins over the casing used to log in.
    assert_eq!(v["username"], "Iceman");
    assert_eq!(v["rank"], "Cadet");
    assert_eq!(v["shipColor"], "#9fb6cc");
    assert_eq!(v["accentColor"], "#2a3340");
    // `kdr` is a *string*, and 0 kills over 0 deaths is "0.00", not "NaN".
    assert_eq!(v["kdr"], "0.00");
    assert!(v["trial1Best"].is_null());
    assert!(v["campaign1BestLives"].is_null());
    assert_eq!(v["unlockHull"], false);
    assert_eq!(v["credits"], 0);

    // Key order, verified against the raw bytes.
    let keys = [
        "ok",
        "token",
        "username",
        "rank",
        "highScore",
        "gamesPlayed",
    ];
    let mut at = 0;
    for k in keys {
        let needle = format!("\"{k}\":");
        let found = res.body[at..]
            .find(&needle)
            .unwrap_or_else(|| panic!("{k} missing or out of order in {}", res.body));
        at += found + needle.len();
    }

    // Wrong password and unknown callsign are the same 401 message.
    let res = post(
        s.addr,
        "/api/login",
        None,
        r#"{"username":"Iceman","password":"wrongpassword"}"#,
    )
    .await;
    assert_eq!(res.status, 401);
    assert_eq!(
        res.body,
        r#"{"ok":false,"error":"Invalid callsign or password"}"#
    );
    let res = post(
        s.addr,
        "/api/login",
        None,
        r#"{"username":"NoSuchPilot","password":"whatever"}"#,
    )
    .await;
    assert_eq!(res.status, 401);
    assert_eq!(
        res.body,
        r#"{"ok":false,"error":"Invalid callsign or password"}"#
    );
}

#[tokio::test]
async fn protected_routes_reject_a_missing_or_bad_token() {
    let s = start().await;
    for (method, path) in [
        ("GET", "/api/unlocks"),
        ("GET", "/api/credits"),
        ("GET", "/api/credits/history"),
    ] {
        let res = request(s.addr, method, path, &[], None).await;
        assert_eq!(res.status, 401, "{path}");
        assert_eq!(res.body, r#"{"ok":false,"error":"Not authenticated"}"#);
    }

    // A malformed bearer token is also 401.
    let res = get(s.addr, "/api/credits", Some("not.a.jwt")).await;
    assert_eq!(res.status, 401);

    // `PUT /api/colors` uses `?? 401` too.
    let res = request(s.addr, "PUT", "/api/colors", &[], Some("{}")).await;
    assert_eq!(res.status, 401);

    // The result routes fall back to 500, not 401 — a real asymmetry in the JS.
    let res = request(s.addr, "POST", "/api/solo-result", &[], Some("{}")).await;
    assert_eq!(res.status, 401, "extractPilotId still throws a 401 itself");
}

#[tokio::test]
async fn colors_persist_and_invalid_hex_falls_back() {
    let s = start().await;
    let (_, token) = register_and_login(s.addr, "Viper").await;

    let res = put(
        s.addr,
        "/api/colors",
        Some(&token),
        r##"{"shipColor":"#ff0000","accentColor":"#00ff00"}"##,
    )
    .await;
    assert_eq!(res.status, 200);
    assert_eq!(res.body, r#"{"ok":true}"#);

    let res = post(
        s.addr,
        "/api/login",
        None,
        r#"{"username":"Viper","password":"testpassword"}"#,
    )
    .await;
    let v = res.json();
    assert_eq!(v["shipColor"], "#ff0000");
    assert_eq!(v["accentColor"], "#00ff00");

    // Garbage hex silently reverts to the defaults rather than erroring.
    put(
        s.addr,
        "/api/colors",
        Some(&token),
        r##"{"shipColor":"red","accentColor":"#zzzzzz"}"##,
    )
    .await;
    let res = post(
        s.addr,
        "/api/login",
        None,
        r#"{"username":"Viper","password":"testpassword"}"#,
    )
    .await;
    let v = res.json();
    assert_eq!(v["shipColor"], "#9fb6cc");
    assert_eq!(v["accentColor"], "#2a3340");
}

#[tokio::test]
async fn profile_lists_every_achievement_with_progress() {
    let s = start().await;
    register_and_login(s.addr, "Jester").await;

    let res = get(s.addr, "/api/profile/Jester", None).await;
    assert_eq!(res.status, 200);
    let v = res.json();
    let profile = &v["profile"];
    assert_eq!(profile["username"], "Jester");
    assert_eq!(profile["kdr"], "0.00");

    let achievements = profile["achievements"].as_array().expect("achievements");
    assert_eq!(achievements.len(), 78, "all 78 ACHIEVEMENT_DEFS rows");
    let first = &achievements[0];
    assert_eq!(first["type"], "first_kill");
    assert_eq!(first["label"], "First Blood");
    assert_eq!(first["icon"], "\u{1f52b}");
    assert_eq!(first["earned"], false);
    assert!(first["earnedAt"].is_null());
    assert_eq!(first["progress"]["current"], 0);
    assert_eq!(first["progress"]["target"], 1);

    // `progress: null` achievements really carry null, not an empty object.
    let kdr = achievements
        .iter()
        .find(|a| a["type"] == "kdr_positive")
        .expect("kdr_positive");
    assert!(kdr["progress"].is_null());

    // The trial-time records carry `isTime`, and only when a time exists.
    let sub30 = achievements
        .iter()
        .find(|a| a["type"] == "trial1_sub30")
        .expect("trial1_sub30");
    assert!(sub30["progress"].is_null(), "no time recorded yet");

    let res = get(s.addr, "/api/profile/NoSuchPilot", None).await;
    assert_eq!(res.status, 404);
    assert_eq!(res.body, r#"{"ok":false,"error":"Pilot not found"}"#);
}

#[tokio::test]
async fn solo_result_awards_credits_achievements_and_rank() {
    let s = start().await;
    let (_, token) = register_and_login(s.addr, "Hollywood").await;

    let res = post(
        s.addr,
        "/api/solo-result",
        Some(&token),
        r#"{"kills":12,"deaths":3,"won":true,"botsKilled":5}"#,
    )
    .await;
    assert_eq!(res.status, 200);
    let v = res.json();
    // Match credits: kills 12*5 + bots 5*2 + win bonus 50 = 120.
    // Achievements: first_kill 100 + kills_10 150 + highscore_5 150
    //             + highscore_10 300 + first_win 200 = 900.
    // `bot_hunter` needs 10 bot kills, and only 5 were scored.
    assert_eq!(v["creditsEarned"], 1020);
    assert_eq!(v["totalCredits"], 1020);
    let earned: Vec<&str> = v["newAchievements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["type"].as_str().unwrap())
        .collect();
    // Order follows the ACHIEVEMENT_DEFS table, which the profile screen
    // depends on.
    assert_eq!(
        earned,
        [
            "first_kill",
            "kills_10",
            "highscore_5",
            "highscore_10",
            "first_win",
        ]
    );

    // Rank recomputed from lifetime kills: 12 kills is "Pilot" (>= 10).
    let res = get(s.addr, "/api/profile/Hollywood", None).await;
    let v = res.json();
    assert_eq!(v["profile"]["rank"], "Pilot");
    assert_eq!(v["profile"]["totalKills"], 12);
    assert_eq!(v["profile"]["gamesPlayed"], 1);
    assert_eq!(v["profile"]["highScore"], 12);
    // 12/3 = 4.00, formatted the JS way.
    assert_eq!(v["profile"]["kdr"], "4.00");

    // Achievements are awarded once. Replaying pays only the match credits.
    let res = post(
        s.addr,
        "/api/solo-result",
        Some(&token),
        r#"{"kills":1,"deaths":0,"won":false,"botsKilled":0}"#,
    )
    .await;
    assert_eq!(res.json()["creditsEarned"], 5);
    assert_eq!(res.json()["newAchievements"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn trial_and_campaign_results_validate_their_ranges() {
    let s = start().await;
    let (_, token) = register_and_login(s.addr, "Slider").await;

    for bad in [
        r#"{"trialNum":0,"time":10}"#,
        r#"{"trialNum":5,"time":10}"#,
        r#"{"trialNum":1,"time":0}"#,
        r#"{"trialNum":1,"time":-4}"#,
        r#"{"trialNum":1}"#,
        r#"{}"#,
    ] {
        let res = post(s.addr, "/api/trial-result", Some(&token), bad).await;
        assert_eq!(res.status, 400, "{bad}");
        assert_eq!(res.body, r#"{"ok":false,"error":"Invalid trial data"}"#);
    }

    // A sub-30 trial 1 earns trial1_complete (300) + trial1_sub30 (1500)
    // + speed_demon (5000).
    let res = post(
        s.addr,
        "/api/trial-result",
        Some(&token),
        r#"{"trialNum":1,"time":28.5}"#,
    )
    .await;
    assert_eq!(res.status, 200);
    assert_eq!(res.json()["creditsEarned"], 6800);

    // A *slower* run must not overwrite the best.
    post(
        s.addr,
        "/api/trial-result",
        Some(&token),
        r#"{"trialNum":1,"time":99}"#,
    )
    .await;
    let v = get(s.addr, "/api/profile/Slider", None).await.json();
    assert_eq!(v["profile"]["trial1Best"], 28.5);

    for bad in [
        r#"{"missionNum":0,"livesRemaining":3}"#,
        r#"{"missionNum":4,"livesRemaining":3}"#,
        r#"{"missionNum":1,"livesRemaining":-1}"#,
        r#"{"missionNum":1,"livesRemaining":4}"#,
    ] {
        let res = post(s.addr, "/api/campaign-result", Some(&token), bad).await;
        assert_eq!(res.status, 400, "{bad}");
        assert_eq!(res.body, r#"{"ok":false,"error":"Invalid campaign data"}"#);
    }

    // Mission 1 pays 500, plus campaign_m1_complete 1500 and
    // campaign_m1_flawless 3000 for a no-death run, plus campaign_boss_first
    // 2000.
    let res = post(
        s.addr,
        "/api/campaign-result",
        Some(&token),
        r#"{"missionNum":1,"livesRemaining":3}"#,
    )
    .await;
    assert_eq!(res.status, 200);
    assert_eq!(res.json()["creditsEarned"], 7000);
    let v = get(s.addr, "/api/profile/Slider", None).await.json();
    assert_eq!(v["profile"]["campaign1BestLives"], 3);
    assert_eq!(v["profile"]["campaignBossKills"], 1);
    assert_eq!(v["profile"]["campaignTotalCompletions"], 1);
}

#[tokio::test]
async fn an_integral_trial_time_serializes_without_a_decimal_point() {
    // SQLite stores this as REAL 28.0, and `JSON.stringify` writes `28`.
    // `serde_json` would write `28.0`, which is a different byte sequence.
    let s = start().await;
    let (_, token) = register_and_login(s.addr, "Merlin").await;
    post(
        s.addr,
        "/api/trial-result",
        Some(&token),
        r#"{"trialNum":2,"time":28}"#,
    )
    .await;
    let res = get(s.addr, "/api/profile/Merlin", None).await;
    assert!(
        res.body.contains(r#""trial2Best":28,"#),
        "expected an integral trial time, got: {}",
        &res.body[..300.min(res.body.len())]
    );
    let res = post(
        s.addr,
        "/api/login",
        None,
        r#"{"username":"Merlin","password":"testpassword"}"#,
    )
    .await;
    assert!(res.body.contains(r#""trial2Best":28,"#), "{}", res.body);
}

#[tokio::test]
async fn credits_unlocks_and_history() {
    let s = start().await;
    let (_, token) = register_and_login(s.addr, "Chipper").await;

    let res = get(s.addr, "/api/credits", Some(&token)).await;
    assert_eq!(res.body, r#"{"ok":true,"credits":0}"#);

    // The cost table ships with the unlocks response, in the JS key order.
    let res = get(s.addr, "/api/unlocks", Some(&token)).await;
    assert_eq!(
        res.body,
        r#"{"ok":true,"costs":{"hull":250,"accent":400,"trail":500,"trail_shape":200,"admin_ship":125000},"unlockHull":false,"unlockAccent":false,"unlockTrail":false,"unlockTrailShape":false,"unlockAdminShip":false}"#
    );

    // Cannot afford anything yet: 402 with the balance echoed back.
    let res = post(s.addr, "/api/unlock/hull", Some(&token), "").await;
    assert_eq!(res.status, 402);
    assert_eq!(
        res.body,
        r#"{"ok":false,"error":"Insufficient credits","balance":0}"#
    );

    // Unknown feature is 400 and carries *no* balance key.
    let res = post(s.addr, "/api/unlock/wings", Some(&token), "").await;
    assert_eq!(res.status, 400);
    assert_eq!(res.body, r#"{"ok":false,"error":"Unknown feature"}"#);

    // Earn some credits, then buy.
    post(
        s.addr,
        "/api/solo-result",
        Some(&token),
        r#"{"kills":20,"deaths":1,"won":true}"#,
    )
    .await;
    let res = post(s.addr, "/api/unlock/hull", Some(&token), "").await;
    assert_eq!(res.status, 200);
    let v = res.json();
    assert_eq!(v["ok"], true);
    assert_eq!(v["alreadyOwned"], false);

    // Buying again is a no-op that still returns 200.
    let res = post(s.addr, "/api/unlock/hull", Some(&token), "").await;
    assert_eq!(res.status, 200);
    let v = res.json();
    assert_eq!(v["alreadyOwned"], true);
    assert_eq!(v["newAchievements"].as_array().unwrap().len(), 0);

    // Spending validates the amount.
    let res = post(
        s.addr,
        "/api/credits/spend",
        Some(&token),
        r#"{"amount":0}"#,
    )
    .await;
    assert_eq!(res.status, 400);
    assert_eq!(res.body, r#"{"ok":false,"error":"Invalid amount"}"#);
    let res = post(
        s.addr,
        "/api/credits/spend",
        Some(&token),
        r#"{"amount":999999999}"#,
    )
    .await;
    assert_eq!(res.status, 402);

    let before = get(s.addr, "/api/credits", Some(&token)).await.json()["credits"]
        .as_i64()
        .unwrap();
    let res = post(
        s.addr,
        "/api/credits/spend",
        Some(&token),
        r#"{"amount":10,"reason":"paint job"}"#,
    )
    .await;
    assert_eq!(res.status, 200);
    assert_eq!(res.json()["balance"], before - 10);

    // History is newest-first and uses the *raw column names* — note
    // `created_at` in snake_case while every other endpoint is camelCase.
    let res = get(s.addr, "/api/credits/history", Some(&token)).await;
    let v = res.json();
    let history = v["history"].as_array().expect("history");
    assert!(!history.is_empty());
    assert!(history[0].get("created_at").is_some(), "{}", res.body);
    assert!(history[0].get("amount").is_some());
    assert!(history[0].get("reason").is_some());
    assert!(history
        .iter()
        .any(|r| r["reason"] == "paint job" && r["amount"] == -10));
    assert!(history.iter().any(|r| r["reason"] == "unlock:hull"));
    assert!(history
        .iter()
        .any(|r| r["reason"] == "match:kills(20),win_bonus"));

    // `limit` is clamped to 1..=100 and defaults to 50 for garbage.
    for (q, max) in [("?limit=1", 1), ("?limit=abc", 50), ("?limit=9999", 100)] {
        let res = get(s.addr, &format!("/api/credits/history{q}"), Some(&token)).await;
        let n = res.json()["history"].as_array().unwrap().len();
        assert!(n <= max, "limit{q} returned {n}");
    }
}

#[tokio::test]
async fn leaderboard_ranks_by_kills_then_wins() {
    let s = start().await;
    let (_, t1) = register_and_login(s.addr, "Alpha").await;
    let (_, t2) = register_and_login(s.addr, "Bravo").await;
    let (_, t3) = register_and_login(s.addr, "Charlie").await;

    post(
        s.addr,
        "/api/solo-result",
        Some(&t1),
        r#"{"kills":5,"deaths":2}"#,
    )
    .await;
    post(
        s.addr,
        "/api/solo-result",
        Some(&t2),
        r#"{"kills":50,"deaths":10}"#,
    )
    .await;
    post(
        s.addr,
        "/api/solo-result",
        Some(&t3),
        r#"{"kills":50,"deaths":0,"won":true}"#,
    )
    .await;

    let res = get(s.addr, "/api/leaderboard", None).await;
    let v = res.json();
    let rows = v["leaderboard"].as_array().expect("leaderboard");
    // Charlie ties Bravo on kills but has the win, so sorts first.
    assert_eq!(rows[0]["username"], "Charlie");
    assert_eq!(rows[0]["position"], 1);
    assert_eq!(rows[1]["username"], "Bravo");
    assert_eq!(rows[2]["username"], "Alpha");
    // The rank column is renamed `pilotRank` here.
    assert_eq!(rows[0]["pilotRank"], "Ace");
    assert!(rows[0].get("rank").is_none());
    // 50 kills, 0 deaths — the JS reports the kill count, not Infinity.
    assert_eq!(rows[0]["kdr"], "50.00");
    assert_eq!(rows[1]["kdr"], "5.00");
    assert_eq!(rows[2]["kdr"], "2.50");
}

#[tokio::test]
async fn static_files_are_served_with_no_store_and_no_validators() {
    let s = start().await;
    let res = get(s.addr, "/index.html", None).await;
    // The repo ships both dist/ and public/; either satisfies this.
    assert_eq!(res.status, 200, "index.html should be served");
    assert_eq!(res.header("cache-control"), Some("no-store"));
    assert_eq!(res.header("etag"), None, "etag: false");
    assert_eq!(res.header("last-modified"), None, "lastModified: false");

    // A directory request appends index.html, like express.static.
    let res = get(s.addr, "/", None).await;
    assert_eq!(res.status, 200);
    assert!(res.body.contains("<!DOCTYPE html>") || res.body.contains("<!doctype html>"));

    let res = get(s.addr, "/definitely-not-here.js", None).await;
    assert_eq!(res.status, 404);
}

#[tokio::test]
async fn static_content_types_carry_a_charset_like_express_send() {
    // An HTTP `Content-Type` charset overrides the document's `<meta charset>`,
    // so this is not purely cosmetic.
    let s = start().await;
    let res = get(s.addr, "/index.html", None).await;
    assert_eq!(res.status, 200);
    assert_eq!(res.header("content-type"), Some("text/html; charset=UTF-8"));
}
