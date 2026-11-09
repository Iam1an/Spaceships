//! The match-end payout path.
//!
//! `endMatch` is reachable in production only from the 300-second
//! `setTimeout`, so neither the integration suite nor the Node differential
//! harness can get to it without waiting five minutes. The test therefore calls
//! [`spaceships_server::lobby::Lobby::end_match`] directly, which is exactly
//! what the timer does.
//!
//! What this pins:
//!
//! - `match-end` carries the winning team and the final team kill totals, and
//!   `-1` for a draw.
//! - `match-credits` goes only to **authenticated** sockets — guests get
//!   nothing — and carries `creditsEarned`, `totalCredits`, and `earned` only
//!   when something was actually unlocked.
//! - The stats are persisted, so the pilot's profile reflects the match
//!   afterwards.

mod harness;

use harness::{get, register_and_login, start, WsClient};
use serde_json::Value;

fn j(raw: &str) -> Value {
    serde_json::from_str(raw).expect("valid JSON")
}

#[tokio::test]
async fn match_end_pays_out_authenticated_pilots_and_persists_stats() {
    let s = start().await;
    let (_, token_a) = register_and_login(s.addr, "Payout").await;

    // A is authenticated, B is a guest.
    let mut a = WsClient::connect(s.addr, Some(&token_a)).await;
    a.send(r#"{"type":"create","private":false,"map":"space","allowBot":false}"#)
        .await;
    let code = j(&a.expect("room", 2000).await)["code"]
        .as_str()
        .unwrap()
        .to_string();

    let mut b = WsClient::connect(s.addr, None).await;
    b.send(&format!(r#"{{"type":"join","code":"{code}"}}"#))
        .await;
    b.expect("room", 2000).await;

    a.send(r#"{"type":"start"}"#).await;
    a.expect("start", 2000).await;
    b.expect("start", 2000).await;

    // Wait out spawn protection, then let A kill B twice.
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    for _ in 0..2 {
        // 100 hp at 50 per missile, 400 ms apart.
        for _ in 0..2 {
            a.send(r#"{"type":"hit","targetId":2,"kind":"missile"}"#)
                .await;
            a.expect("hp", 2000).await;
            tokio::time::sleep(std::time::Duration::from_millis(450)).await;
        }
        a.expect("death", 2000).await;
        // Wait for the respawn plus its spawn protection before going again.
        b.expect("respawn", 4000).await;
        tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    }

    // Fire the match clock by hand.
    s.lobby.end_match(&code).await;

    // Both sides see the result. A is team 0 and scored both kills.
    let end = j(&a.expect("match-end", 2000).await);
    assert_eq!(end["winner"], 0);
    assert_eq!(end["teamKills"], serde_json::json!([2, 0]));
    let end_b = j(&b.expect("match-end", 2000).await);
    assert_eq!(end_b["winner"], 0);

    // Only the authenticated socket is paid.
    let credits = j(&a.expect("match-credits", 3000).await);
    // 2 kills * 5 CR + 50 CR win bonus = 60, plus achievements:
    // first_kill 100 + first_win 200 + highscore_5 is not reached (2 kills).
    assert_eq!(credits["creditsEarned"], 360);
    assert_eq!(credits["totalCredits"], 360);
    let earned: Vec<&str> = credits["earned"]
        .as_array()
        .expect("earned present when something unlocked")
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    assert_eq!(earned, ["first_kill", "first_win"]);

    // The guest gets no payout at all.
    b.expect_none("match-credits", 500).await;

    // And the stats landed in the database.
    let profile = get(s.addr, "/api/profile/Payout", None).await.json();
    let p = &profile["profile"];
    assert_eq!(p["totalKills"], 2);
    assert_eq!(p["matchesWon"], 1);
    assert_eq!(p["matchesLost"], 0);
    assert_eq!(p["gamesPlayed"], 1);
    assert_eq!(p["highScore"], 2);
    assert_eq!(p["credits"], 360);
}

#[tokio::test]
async fn a_scoreless_match_is_a_draw_and_omits_the_earned_key() {
    let s = start().await;
    let (_, token) = register_and_login(s.addr, "Drawer").await;
    let mut a = WsClient::connect(s.addr, Some(&token)).await;
    a.send(r#"{"type":"create","private":false,"map":"space","allowBot":false}"#)
        .await;
    let code = j(&a.expect("room", 2000).await)["code"]
        .as_str()
        .unwrap()
        .to_string();
    a.send(r#"{"type":"start"}"#).await;
    a.expect("start", 2000).await;

    s.lobby.end_match(&code).await;

    let raw = a.expect("match-end", 2000).await;
    assert_eq!(raw, r#"{"type":"match-end","winner":-1,"teamKills":[0,0]}"#);

    // Nothing was unlocked, so the JS omits `earned` entirely rather than
    // sending an empty array. Assert on the raw bytes.
    let raw = a.expect("match-credits", 3000).await;
    assert_eq!(
        raw, r#"{"type":"match-credits","creditsEarned":0,"totalCredits":0}"#,
        "`earned` must be absent, not []"
    );

    // A draw records neither a win nor a loss.
    let profile = get(s.addr, "/api/profile/Drawer", None).await.json();
    assert_eq!(profile["profile"]["matchesWon"], 0);
    assert_eq!(profile["profile"]["matchesLost"], 0);
    assert_eq!(profile["profile"]["gamesPlayed"], 1);
}

#[tokio::test]
async fn ending_a_match_twice_is_a_no_op() {
    // `if (!room || room.matchOver) return;` — the guard that stops a second
    // payout. Worth pinning: without it every pilot would be paid twice.
    let s = start().await;
    let (_, token) = register_and_login(s.addr, "Doubler").await;
    let mut a = WsClient::connect(s.addr, Some(&token)).await;
    a.send(r#"{"type":"create","private":false,"map":"space","allowBot":false}"#)
        .await;
    let code = j(&a.expect("room", 2000).await)["code"]
        .as_str()
        .unwrap()
        .to_string();
    a.send(r#"{"type":"start"}"#).await;
    a.expect("start", 2000).await;

    s.lobby.end_match(&code).await;
    a.expect("match-end", 2000).await;
    a.expect("match-credits", 3000).await;

    s.lobby.end_match(&code).await;
    a.expect_none("match-end", 500).await;

    let profile = get(s.addr, "/api/profile/Doubler", None).await.json();
    assert_eq!(
        profile["profile"]["gamesPlayed"], 1,
        "the match must be counted once"
    );
}
