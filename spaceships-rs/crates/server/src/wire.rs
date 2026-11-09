//! Inbound frame parsing, with the JS server's tolerance.
//!
//! [`spaceships_protocol::ClientMessage`] is a faithful description of what the
//! shipped browser client *sends* — and the shipped client always sends every
//! field, so a plain `serde_json::from_str` handles all real traffic. But the
//! JS server is more forgiving than that: it never validates a message, it just
//! reads properties and lets `undefined` fall through JavaScript's coercion
//! rules. `{"type":"state","pos":[…],"quat":[…]}` with no `boost` is accepted
//! there (`boost: !!msg.boost` → `false`) and rejected by a strict serde parse.
//!
//! Dropping such a frame would be a silent behaviour change for anything that
//! is not the stock client — an older cached build, a reconnecting tab, a test
//! harness. So parsing is two-stage: strict first, and on failure fill in the
//! exact defaults the JS coercions produce and try once more.
//!
//! # Which fields get a default, and which deliberately do not
//!
//! Only fields the JS coerces to a *definite* value are defaulted:
//!
//! | tag           | field(s)                        | JS expression                      |
//! |---------------|---------------------------------|------------------------------------|
//! | `create`      | `private`                       | `!!msg.private` → `false`          |
//! | `create`      | `map`                           | `… === 'terrain' ? … : 'space'`    |
//! | `create`      | `allowBot`                      | `msg.allowBot !== false` → `true`  |
//! | `state`       | `boost`                         | `!!msg.boost` → `false`            |
//! | `fire`        | `kind`, `shots`                 | relayed as-is; absent → bullet, [] |
//! | `bot-fire`    | `kind`, `shots`                 | `… === 'missile' ? … : 'bullet'`   |
//! | `hit`         | `kind`                          | `msg.kind === 'missile' ? 50 : 10` |
//! | `self-damage` | `dmg`                           | `Number(msg.dmg) \|\| 0` → `0`     |
//! | `name`        | `name`                          | `typeof … === 'string' ? … : ''`   |
//! | `join`        | `code`                          | `String(msg.code \|\| '')` → `''`  |
//! | `colors`      | `hullColor`, `accentColor`      | relayed as-is                      |
//! | `ship-model`  | `modelUrl`                      | `typeof … === 'string' ? … : null` |
//!
//! **Identity fields are never defaulted.** `hit.targetId`, `asteroid-hit.id`,
//! `bot-state.botId` and `bot-fire.botId` are left missing, so the message
//! fails to parse and is dropped. That is the same outcome the JS reaches by a
//! different route — `room.players.get(undefined)` and
//! `asteroids.find(x => x.id === undefined)` both yield nothing and the handler
//! returns — and inventing a `0` here would be *worse* than the JS, because
//! asteroid `0` is a real rock.

use serde_json::{Map, Value};
use spaceships_protocol::ClientMessage;

/// Parses one inbound text frame.
///
/// `None` means "ignore this frame", which is what the JS does for malformed
/// JSON (`try { … } catch { return }`) and for any `type` it has no arm for.
#[must_use]
pub fn parse_client_message(raw: &str) -> Option<ClientMessage> {
    if let Ok(msg) = serde_json::from_str::<ClientMessage>(raw) {
        return Some(msg);
    }
    let mut value: Value = serde_json::from_str(raw).ok()?;
    let obj = value.as_object_mut()?;
    let tag = obj.get("type")?.as_str()?.to_string();
    apply_js_defaults(obj, &tag);
    serde_json::from_value(value).ok()
}

/// Fills in the values JavaScript's coercions would have produced for absent
/// properties. Present properties are never touched, including explicit
/// `null`s — `!!null` is `false` and so is `!!undefined`, so the two agree
/// anyway for every field here.
fn apply_js_defaults(obj: &mut Map<String, Value>, tag: &str) {
    let mut fill = |key: &str, v: Value| {
        if !obj.contains_key(key) || obj[key].is_null() {
            obj.insert(key.to_string(), v);
        }
    };
    match tag {
        "create" => {
            fill("private", Value::Bool(false));
            fill("map", Value::String("space".into()));
            // The only opt-out is an explicit `false`; absent means on.
            fill("allowBot", Value::Bool(true));
        }
        "state" => fill("boost", Value::Bool(false)),
        "fire" | "bot-fire" => {
            fill("kind", Value::String("bullet".into()));
            fill("shots", Value::Array(Vec::new()));
        }
        "hit" => fill("kind", Value::String("bullet".into())),
        "self-damage" => fill("dmg", Value::from(0)),
        "name" => fill("name", Value::String(String::new())),
        "join" => fill("code", Value::String(String::new())),
        "colors" => {
            fill("hullColor", Value::from(0));
            fill("accentColor", Value::from(0));
        }
        "ship-model" => fill("modelUrl", Value::String(String::new())),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spaceships_protocol::{MapKind, WeaponKind};

    #[test]
    fn the_stock_client_parses_strictly() {
        // Exactly the bytes `public/src/lobby/rooms.js` and `main.js` emit.
        let cases = [
            r#"{"type":"name","name":"Maverick"}"#,
            r#"{"type":"list-rooms"}"#,
            r#"{"type":"create","private":false,"map":"space","allowBot":true}"#,
            r#"{"type":"join","code":"ABCD"}"#,
            r#"{"type":"start"}"#,
            r#"{"type":"leave"}"#,
            r#"{"type":"state","pos":[1,2,3],"quat":[0,0,0,1],"boost":false}"#,
            r#"{"type":"fire","kind":"bullet","shots":[{"pos":[1,2,3],"dir":[0,0,1]}]}"#,
            r#"{"type":"fire","kind":"beam","shots":[{"pos":[1,2,3],"end":[0,0,9]}]}"#,
            r#"{"type":"flare","pos":[1,2,3],"quat":[0,0,0,1]}"#,
            r#"{"type":"hit","targetId":7,"kind":"beam"}"#,
            r#"{"type":"hit","targetId":7,"fromBotId":-3,"kind":"missile"}"#,
            r#"{"type":"self-damage","dmg":1}"#,
            r#"{"type":"colors","hullColor":10461644,"accentColor":2765632}"#,
            r#"{"type":"ship-model","modelUrl":"models/admin.glb"}"#,
            r#"{"type":"asteroid-hit","id":12}"#,
            r#"{"type":"bot-state","botId":-2,"pos":[1,2,3],"quat":[0,0,0,1]}"#,
            r#"{"type":"bot-fire","botId":-2,"kind":"bullet","shots":[{"pos":[0,0,0],"dir":[0,0,1]}]}"#,
        ];
        for raw in cases {
            assert!(parse_client_message(raw).is_some(), "failed to parse {raw}");
        }
    }

    #[test]
    fn missing_coerced_fields_get_the_js_defaults() {
        let msg = parse_client_message(r#"{"type":"create"}"#).unwrap();
        assert_eq!(
            msg,
            ClientMessage::Create {
                private: false,
                map: MapKind::Space,
                allow_bot: true,
            }
        );

        let msg = parse_client_message(r#"{"type":"state","pos":[1,2,3],"quat":[0,0,0,1]}"#);
        assert_eq!(
            msg,
            Some(ClientMessage::State {
                pos: [1.0, 2.0, 3.0],
                quat: [0.0, 0.0, 0.0, 1.0],
                boost: false,
            })
        );

        let msg = parse_client_message(r#"{"type":"hit","targetId":4}"#).unwrap();
        assert_eq!(
            msg,
            ClientMessage::Hit {
                target_id: 4,
                kind: WeaponKind::Bullet,
                from_bot_id: None,
            }
        );

        assert_eq!(
            parse_client_message(r#"{"type":"self-damage"}"#),
            Some(ClientMessage::SelfDamage { dmg: 0.0 })
        );
    }

    #[test]
    fn allow_bot_only_opts_out_on_an_explicit_false() {
        for (raw, expected) in [
            (r#"{"type":"create","allowBot":false}"#, false),
            (r#"{"type":"create","allowBot":true}"#, true),
            (r#"{"type":"create"}"#, true),
            // `null` is not `false`, and `msg.allowBot !== false` is therefore
            // true — the JS turns the bot *on*.
            (r#"{"type":"create","allowBot":null}"#, true),
        ] {
            let ClientMessage::Create { allow_bot, .. } = parse_client_message(raw).unwrap() else {
                panic!("not a create: {raw}");
            };
            assert_eq!(allow_bot, expected, "{raw}");
        }
    }

    #[test]
    fn unknown_maps_and_weapons_coerce_rather_than_fail() {
        let ClientMessage::Create { map, .. } =
            parse_client_message(r#"{"type":"create","map":"moon"}"#).unwrap()
        else {
            panic!()
        };
        assert_eq!(map, MapKind::Space);

        let ClientMessage::Hit { kind, .. } =
            parse_client_message(r#"{"type":"hit","targetId":1,"kind":"plasma"}"#).unwrap()
        else {
            panic!()
        };
        assert_eq!(kind, WeaponKind::Bullet);
    }

    #[test]
    fn identity_fields_are_never_invented() {
        // Dropping these is the same outcome the JS reaches via
        // `players.get(undefined)`. Defaulting `asteroid-hit.id` to 0 would be
        // actively wrong — rock 0 exists.
        assert!(parse_client_message(r#"{"type":"asteroid-hit"}"#).is_none());
        assert!(parse_client_message(r#"{"type":"hit","kind":"bullet"}"#).is_none());
        assert!(
            parse_client_message(r#"{"type":"bot-state","pos":[0,0,0],"quat":[0,0,0,1]}"#)
                .is_none()
        );
    }

    #[test]
    fn garbage_is_ignored_rather_than_fatal() {
        for raw in [
            "",
            "not json",
            "[]",
            "null",
            "42",
            r#"{"no":"type"}"#,
            r#"{"type":"nonsense"}"#,
            r#"{"type":42}"#,
        ] {
            assert!(
                parse_client_message(raw).is_none(),
                "{raw:?} should be ignored"
            );
        }
    }
}
