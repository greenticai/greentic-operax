use super::http::ParsedHttpUrl;
use super::ids::route_card_id;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub fn welcome_card(locale: &str, artifact_base: &str, operax_url_base: &str) -> Value {
    json!({
        "type": "AdaptiveCard",
        "version": "1.5",
        "lang": locale,
        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
        "metadata": {"locale": locale, "schema": "greentic.operax.manager-card.v1"},
        "body": [
            {"type": "TextBlock", "text": "OperaX Manager", "size": "Large", "weight": "Bolder", "wrap": true},
            {"type": "TextBlock", "text": format!("Testing {artifact_base}"), "wrap": true}
        ],
        "actions": [{
            "type": "Action.Submit",
            "title": "Open Dashboard",
            "data": {
                "action": "operax_manager_open",
                "manager_target": "dashboard",
                "manager_cards_base_url": format!("{operax_url_base}/v1/operax/manager/cards"),
                "routeToCardId": "operax_dashboard",
                "cardId": "operax_dashboard",
                "step": "open"
            }
        }]
    })
}

pub fn placeholder_dashboard_card(locale: &str) -> Value {
    json!({
        "type": "AdaptiveCard",
        "version": "1.5",
        "lang": locale,
        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
        "metadata": {
            "locale": locale,
            "schema": "greentic.operax.manager-card.v1",
            "kind": "manager.dashboard.placeholder"
        },
        "body": [
            {"type": "TextBlock", "text": "OperaX dashboard is starting", "size": "Large", "weight": "Bolder", "wrap": true},
            {"type": "TextBlock", "text": "The live dashboard card will be injected after the OperaX manager is ready.", "wrap": true}
        ],
        "actions": []
    })
}

pub fn normalize_card_for_webchat(card: &mut Value, operax_url: &ParsedHttpUrl) {
    normalize_actions(card, operax_url);
    normalize_card_items(card);
}

fn normalize_actions(value: &mut Value, operax_url: &ParsedHttpUrl) {
    match value {
        Value::Object(map) => {
            let is_submit = map
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|value| value == "Action.Submit");
            if is_submit && let Some(data) = map.get_mut("data").and_then(Value::as_object_mut) {
                if data.get("action").and_then(Value::as_str) == Some("operax_manager_submit") {
                    set_absolute_manager_url(
                        data,
                        "manager_submit_url",
                        &format!("{}/v1/operax/manager/submit", operax_url.base),
                    );
                }
                if let Some(target) = data
                    .get("manager_target")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                {
                    set_absolute_manager_url(
                        data,
                        "manager_cards_base_url",
                        &format!("{}/v1/operax/manager/cards", operax_url.base),
                    );
                    data.insert("routeToCardId".to_string(), json!(route_card_id(&target)));
                    data.entry("cardId".to_string())
                        .or_insert_with(|| json!(route_card_id(&target)));
                    data.entry("step".to_string())
                        .or_insert_with(|| json!("open"));
                    data.entry("action".to_string())
                        .or_insert_with(|| json!("operax_manager_open"));
                }
            }
            for child in map.values_mut() {
                normalize_actions(child, operax_url);
            }
        }
        Value::Array(items) => {
            for child in items {
                normalize_actions(child, operax_url);
            }
        }
        _ => {}
    }
}

fn normalize_card_items(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("TextBlock") {
                for key in ["size", "weight"] {
                    if let Some(text) = map.get(key).and_then(Value::as_str) {
                        let mut chars = text.chars();
                        if let Some(first) = chars.next() {
                            *map.get_mut(key).expect("key exists") = Value::String(format!(
                                "{}{}",
                                first.to_uppercase(),
                                chars.as_str()
                            ));
                        }
                    }
                }
            }
            if map.get("type").and_then(Value::as_str) == Some("Input.Text") {
                let label = map
                    .get("label")
                    .or_else(|| map.get("placeholder"))
                    .or_else(|| map.get("id"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                if let Some(label) = label {
                    map.entry("label".to_string())
                        .or_insert_with(|| json!(label));
                }
            }
            for child in map.values_mut() {
                normalize_card_items(child);
            }
        }
        Value::Array(items) => {
            for child in items {
                normalize_card_items(child);
            }
        }
        _ => {}
    }
}

pub fn set_absolute_manager_url(map: &mut serde_json::Map<String, Value>, key: &str, url: &str) {
    let needs_set = map
        .get(key)
        .and_then(Value::as_str)
        .is_none_or(|value| value.starts_with('/') || value.trim().is_empty());
    if needs_set {
        map.insert(key.to_string(), json!(url));
    }
}

pub fn collect_navigable_targets(card: &Value) -> Vec<String> {
    let mut targets = BTreeSet::new();
    collect_targets(card, &mut targets);
    targets.into_iter().collect()
}

fn collect_targets(value: &Value, targets: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("Action.Submit")
                && let Some(target) = map
                    .get("data")
                    .and_then(Value::as_object)
                    .and_then(|data| data.get("manager_target"))
                    .and_then(Value::as_str)
            {
                targets.insert(target.to_string());
            }
            for child in map.values() {
                collect_targets(child, targets);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_targets(child, targets);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_card_contains_artifact_and_locale() {
        let card = welcome_card("en-GB", "my-handoff.gtpack", "http://127.0.0.1:8797");
        assert_eq!(card["lang"], "en-GB");
        assert_eq!(card["body"][1]["text"], "Testing my-handoff.gtpack");
        assert_eq!(
            card["actions"][0]["data"]["manager_cards_base_url"],
            "http://127.0.0.1:8797/v1/operax/manager/cards"
        );
        assert_eq!(
            card["actions"][0]["data"]["routeToCardId"],
            "operax_dashboard"
        );
        assert_eq!(card["actions"][0]["data"]["step"], "open");
    }

    #[test]
    fn placeholder_dashboard_card_has_locale_and_kind() {
        let card = placeholder_dashboard_card("nl");
        assert_eq!(card["lang"], "nl");
        assert_eq!(card["metadata"]["kind"], "manager.dashboard.placeholder");
        assert!(
            card["body"][0]["text"]
                .as_str()
                .unwrap()
                .contains("starting")
        );
    }

    #[test]
    fn normalize_card_adds_submit_url_for_submit_action() {
        let operax_url = ParsedHttpUrl::parse("http://127.0.0.1:8797", "test").unwrap();
        let mut card = json!({
            "type": "AdaptiveCard",
            "actions": [{
                "type": "Action.Submit",
                "title": "Submit",
                "data": {"action": "operax_manager_submit", "manager_target": "input"}
            }]
        });
        normalize_card_for_webchat(&mut card, &operax_url);
        let data = &card["actions"][0]["data"];
        assert_eq!(
            data["manager_submit_url"],
            "http://127.0.0.1:8797/v1/operax/manager/submit"
        );
        assert_eq!(
            data["manager_cards_base_url"],
            "http://127.0.0.1:8797/v1/operax/manager/cards"
        );
        assert_eq!(data["routeToCardId"], "input");
        assert_eq!(data["step"], "open");
        assert_eq!(data["cardId"], "input");
    }

    #[test]
    fn normalize_card_preserves_existing_absolute_url() {
        let operax_url = ParsedHttpUrl::parse("http://127.0.0.1:8797", "test").unwrap();
        let mut card = json!({
            "type": "AdaptiveCard",
            "actions": [{
                "type": "Action.Submit",
                "data": {
                    "action": "operax_manager_submit",
                    "manager_submit_url": "http://other:9999/v1/operax/manager/submit"
                }
            }]
        });
        normalize_card_for_webchat(&mut card, &operax_url);
        let data = &card["actions"][0]["data"];
        assert_eq!(
            data["manager_submit_url"],
            "http://other:9999/v1/operax/manager/submit"
        );
    }

    #[test]
    fn normalize_card_replaces_relative_url() {
        let operax_url = ParsedHttpUrl::parse("http://127.0.0.1:8797", "test").unwrap();
        let mut card = json!({
            "type": "AdaptiveCard",
            "actions": [{
                "type": "Action.Submit",
                "data": {
                    "action": "operax_manager_submit",
                    "manager_submit_url": "/v1/operax/manager/submit"
                }
            }]
        });
        normalize_card_for_webchat(&mut card, &operax_url);
        let data = &card["actions"][0]["data"];
        assert_eq!(
            data["manager_submit_url"],
            "http://127.0.0.1:8797/v1/operax/manager/submit"
        );
    }

    #[test]
    fn normalize_card_capitalizes_text_block_size_and_weight() {
        let operax_url = ParsedHttpUrl::parse("http://127.0.0.1:8797", "test").unwrap();
        let mut card = json!({
            "type": "AdaptiveCard",
            "body": [{"type": "TextBlock", "text": "Hello", "size": "large", "weight": "bolder"}]
        });
        normalize_card_for_webchat(&mut card, &operax_url);
        assert_eq!(card["body"][0]["size"], "Large");
        assert_eq!(card["body"][0]["weight"], "Bolder");
    }

    #[test]
    fn normalize_card_adds_label_to_input_text_from_placeholder() {
        let operax_url = ParsedHttpUrl::parse("http://127.0.0.1:8797", "test").unwrap();
        let mut card = json!({
            "type": "AdaptiveCard",
            "body": [{"type": "Input.Text", "id": "name", "placeholder": "Enter name"}]
        });
        normalize_card_for_webchat(&mut card, &operax_url);
        assert_eq!(card["body"][0]["label"], "Enter name");
    }

    #[test]
    fn normalize_card_adds_label_from_id_when_no_placeholder() {
        let operax_url = ParsedHttpUrl::parse("http://127.0.0.1:8797", "test").unwrap();
        let mut card = json!({
            "type": "AdaptiveCard",
            "body": [{"type": "Input.Text", "id": "field_name"}]
        });
        normalize_card_for_webchat(&mut card, &operax_url);
        assert_eq!(card["body"][0]["label"], "field_name");
    }

    #[test]
    fn normalize_card_does_not_overwrite_existing_label() {
        let operax_url = ParsedHttpUrl::parse("http://127.0.0.1:8797", "test").unwrap();
        let mut card = json!({
            "type": "AdaptiveCard",
            "body": [{"type": "Input.Text", "id": "x", "label": "Existing", "placeholder": "Other"}]
        });
        normalize_card_for_webchat(&mut card, &operax_url);
        assert_eq!(card["body"][0]["label"], "Existing");
    }

    #[test]
    fn normalize_card_adds_open_action_defaults() {
        let operax_url = ParsedHttpUrl::parse("http://127.0.0.1:8797", "test").unwrap();
        let mut card = json!({
            "type": "AdaptiveCard",
            "actions": [{
                "type": "Action.Submit",
                "data": {"manager_target": "details"}
            }]
        });
        normalize_card_for_webchat(&mut card, &operax_url);
        let data = &card["actions"][0]["data"];
        assert_eq!(data["action"], "operax_manager_open");
        assert_eq!(data["step"], "open");
        assert_eq!(data["cardId"], "details");
        assert_eq!(data["routeToCardId"], "details");
    }

    #[test]
    fn normalize_card_preserves_existing_card_id() {
        let operax_url = ParsedHttpUrl::parse("http://127.0.0.1:8797", "test").unwrap();
        let mut card = json!({
            "type": "AdaptiveCard",
            "actions": [{
                "type": "Action.Submit",
                "data": {"manager_target": "details", "cardId": "existing_id"}
            }]
        });
        normalize_card_for_webchat(&mut card, &operax_url);
        let data = &card["actions"][0]["data"];
        assert_eq!(data["cardId"], "existing_id");
        assert_eq!(data["routeToCardId"], "details");
    }

    #[test]
    fn normalize_card_skips_empty_target() {
        let operax_url = ParsedHttpUrl::parse("http://127.0.0.1:8797", "test").unwrap();
        let mut card = json!({
            "type": "AdaptiveCard",
            "actions": [{
                "type": "Action.Submit",
                "data": {"manager_target": ""}
            }]
        });
        let original = card.clone();
        normalize_card_for_webchat(&mut card, &operax_url);
        assert_eq!(card, original);
    }

    #[test]
    fn normalize_card_recurses_into_nested_arrays() {
        let operax_url = ParsedHttpUrl::parse("http://127.0.0.1:8797", "test").unwrap();
        let mut card = json!({
            "type": "AdaptiveCard",
            "body": [{
                "type": "Container",
                "items": [{
                    "type": "ActionSet",
                    "actions": [{
                        "type": "Action.Submit",
                        "data": {"action": "operax_manager_submit", "manager_target": "nested"}
                    }]
                }]
            }]
        });
        normalize_card_for_webchat(&mut card, &operax_url);
        let data = &card["body"][0]["items"][0]["actions"][0]["data"];
        assert_eq!(
            data["manager_submit_url"],
            "http://127.0.0.1:8797/v1/operax/manager/submit"
        );
    }

    #[test]
    fn set_absolute_manager_url_sets_when_missing() {
        let mut map = serde_json::Map::new();
        set_absolute_manager_url(&mut map, "key", "http://host:1234/path");
        assert_eq!(map["key"], "http://host:1234/path");
    }

    #[test]
    fn set_absolute_manager_url_sets_when_relative() {
        let mut map = serde_json::Map::new();
        map.insert("key".to_string(), json!("/relative/path"));
        set_absolute_manager_url(&mut map, "key", "http://host:1234/path");
        assert_eq!(map["key"], "http://host:1234/path");
    }

    #[test]
    fn set_absolute_manager_url_sets_when_empty() {
        let mut map = serde_json::Map::new();
        map.insert("key".to_string(), json!("  "));
        set_absolute_manager_url(&mut map, "key", "http://host:1234/path");
        assert_eq!(map["key"], "http://host:1234/path");
    }

    #[test]
    fn set_absolute_manager_url_preserves_existing_absolute() {
        let mut map = serde_json::Map::new();
        map.insert("key".to_string(), json!("http://other:9999/existing"));
        set_absolute_manager_url(&mut map, "key", "http://host:1234/path");
        assert_eq!(map["key"], "http://other:9999/existing");
    }

    #[test]
    fn collect_navigable_targets_finds_submit_targets() {
        let card = json!({
            "type": "AdaptiveCard",
            "actions": [
                {"type": "Action.Submit", "data": {"manager_target": "details"}},
                {"type": "Action.Submit", "data": {"manager_target": "settings"}},
                {"type": "Action.Submit", "data": {"manager_target": "details"}}
            ]
        });
        let targets = collect_navigable_targets(&card);
        assert_eq!(targets, vec!["details", "settings"]);
    }

    #[test]
    fn collect_navigable_targets_empty_for_no_submits() {
        let card = json!({"type": "AdaptiveCard", "body": []});
        assert!(collect_navigable_targets(&card).is_empty());
    }

    #[test]
    fn collect_navigable_targets_ignores_non_submit_actions() {
        let card = json!({
            "type": "AdaptiveCard",
            "actions": [
                {"type": "Action.OpenUrl", "data": {"manager_target": "x"}}
            ]
        });
        assert!(collect_navigable_targets(&card).is_empty());
    }

    #[test]
    fn collect_navigable_targets_recurses_into_nested() {
        let card = json!({
            "type": "AdaptiveCard",
            "body": [{
                "type": "Container",
                "items": [{"type": "Action.Submit", "data": {"manager_target": "nested"}}]
            }]
        });
        assert_eq!(collect_navigable_targets(&card), vec!["nested"]);
    }

    #[test]
    fn collect_navigable_targets_from_arrays() {
        let card = json!([
            {"type": "Action.Submit", "data": {"manager_target": "a"}},
            {"type": "Action.Submit", "data": {"manager_target": "b"}}
        ]);
        assert_eq!(collect_navigable_targets(&card), vec!["a", "b"]);
    }

    #[test]
    fn normalize_card_non_submit_action_unchanged() {
        let operax_url = ParsedHttpUrl::parse("http://127.0.0.1:8797", "test").unwrap();
        let mut card = json!({
            "type": "AdaptiveCard",
            "actions": [{"type": "Action.OpenUrl", "url": "http://example.com"}]
        });
        let original = card.clone();
        normalize_card_for_webchat(&mut card, &operax_url);
        assert_eq!(card, original);
    }

    #[test]
    fn normalize_card_primitive_values_unchanged() {
        let operax_url = ParsedHttpUrl::parse("http://127.0.0.1:8797", "test").unwrap();
        let mut card = json!(42);
        normalize_card_for_webchat(&mut card, &operax_url);
        assert_eq!(card, json!(42));

        let mut card = json!("hello");
        normalize_card_for_webchat(&mut card, &operax_url);
        assert_eq!(card, json!("hello"));

        let mut card = json!(true);
        normalize_card_for_webchat(&mut card, &operax_url);
        assert_eq!(card, json!(true));

        let mut card = json!(null);
        normalize_card_for_webchat(&mut card, &operax_url);
        assert_eq!(card, json!(null));
    }
}
