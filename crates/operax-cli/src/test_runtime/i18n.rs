use super::packing::{sorted_files, write_json};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

pub fn write_card_i18n(cards_dir: &Path, locale: &str) -> Result<(), String> {
    let i18n_dir = cards_dir
        .parent()
        .ok_or_else(|| format!("cards directory has no parent: {}", cards_dir.display()))?
        .join("i18n");
    fs::create_dir_all(&i18n_dir)
        .map_err(|err| format!("failed to create {}: {err}", i18n_dir.display()))?;
    let mut en = serde_json::Map::new();
    for file in sorted_files(cards_dir)? {
        if file.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(card_name) = file.file_stem().and_then(|name| name.to_str()) else {
            continue;
        };
        let Ok(value) = fs::read_to_string(&file)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .ok_or(())
        else {
            continue;
        };
        collect_i18n(card_name, &value, Vec::new(), &mut en);
    }
    write_json(
        &i18n_dir.join("_manifest.json"),
        &json!({"locales": locale_codes(locale)}),
    )?;
    let en_value = Value::Object(en);
    write_json(&i18n_dir.join("en.json"), &en_value)?;
    for code in locale_codes(locale) {
        if code != "en" {
            write_json(&i18n_dir.join(format!("{code}.json")), &en_value)?;
        }
    }
    Ok(())
}

pub fn collect_i18n(
    card_name: &str,
    value: &Value,
    path: Vec<String>,
    out: &mut serde_json::Map<String, Value>,
) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if ["text", "title", "label", "placeholder", "errorMessage"].contains(&key.as_str())
                {
                    if let Some(text) = child.as_str() {
                        out.insert(
                            format!("cards.{card_name}.{}.{}", path.join("."), key),
                            json!(text),
                        );
                    }
                } else {
                    let mut next = path.clone();
                    next.push(key.clone());
                    collect_i18n(card_name, child, next, out);
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                let mut next = path.clone();
                next.push(format!("i{index}"));
                collect_i18n(card_name, child, next, out);
            }
        }
        _ => {}
    }
}

pub fn locale_codes(locale: &str) -> Vec<String> {
    let mut codes = vec!["en".to_string()];
    if !codes.iter().any(|value| value == locale) {
        codes.push(locale.to_string());
    }
    if let Some(language) = locale.split('-').next()
        && !language.is_empty()
        && !codes.iter().any(|value| value == language)
    {
        codes.push(language.to_string());
    }
    codes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_codes_en_returns_just_en() {
        assert_eq!(locale_codes("en"), vec!["en"]);
    }

    #[test]
    fn locale_codes_nl_returns_en_and_nl() {
        assert_eq!(locale_codes("nl"), vec!["en", "nl"]);
    }

    #[test]
    fn locale_codes_en_gb_returns_en_and_en_gb() {
        let codes = locale_codes("en-GB");
        assert_eq!(codes, vec!["en", "en-GB"]);
    }

    #[test]
    fn locale_codes_fr_ca_returns_en_fr_ca_fr() {
        let codes = locale_codes("fr-CA");
        assert_eq!(codes, vec!["en", "fr-CA", "fr"]);
    }

    #[test]
    fn locale_codes_empty_returns_en_and_empty() {
        let codes = locale_codes("");
        assert_eq!(codes, vec!["en", ""]);
    }

    #[test]
    fn collect_i18n_extracts_text_fields() {
        let mut out = serde_json::Map::new();
        let card = json!({
            "body": [
                {"type": "TextBlock", "text": "Hello World", "size": "Large"}
            ]
        });
        collect_i18n("welcome", &card, Vec::new(), &mut out);
        assert_eq!(out["cards.welcome.body.i0.text"], "Hello World");
        assert!(!out.contains_key("cards.welcome.body.i0.size"));
    }

    #[test]
    fn collect_i18n_extracts_title_and_label() {
        let mut out = serde_json::Map::new();
        let card = json!({
            "actions": [{"title": "Click me"}],
            "body": [{"type": "Input.Text", "label": "Name", "placeholder": "Enter"}]
        });
        collect_i18n("form", &card, Vec::new(), &mut out);
        assert_eq!(out["cards.form.actions.i0.title"], "Click me");
        assert_eq!(out["cards.form.body.i0.label"], "Name");
        assert_eq!(out["cards.form.body.i0.placeholder"], "Enter");
    }

    #[test]
    fn collect_i18n_extracts_error_message() {
        let mut out = serde_json::Map::new();
        let card = json!({
            "body": [{"type": "Input.Text", "errorMessage": "Required field"}]
        });
        collect_i18n("validation", &card, Vec::new(), &mut out);
        assert_eq!(
            out["cards.validation.body.i0.errorMessage"],
            "Required field"
        );
    }

    #[test]
    fn collect_i18n_skips_non_string_text_fields() {
        let mut out = serde_json::Map::new();
        let card = json!({"text": 42, "title": true});
        collect_i18n("card", &card, Vec::new(), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn collect_i18n_handles_nested_structure() {
        let mut out = serde_json::Map::new();
        let card = json!({
            "body": [{
                "type": "Container",
                "items": [{"text": "Nested text"}]
            }]
        });
        collect_i18n("nested", &card, Vec::new(), &mut out);
        assert_eq!(out["cards.nested.body.i0.items.i0.text"], "Nested text");
    }

    #[test]
    fn collect_i18n_empty_card() {
        let mut out = serde_json::Map::new();
        collect_i18n("empty", &json!({}), Vec::new(), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn collect_i18n_primitive_value() {
        let mut out = serde_json::Map::new();
        collect_i18n("prim", &json!(42), Vec::new(), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn write_card_i18n_creates_manifest_and_locale_files() {
        let temp = tempfile::tempdir().unwrap();
        let assets = temp.path().join("assets");
        let cards = assets.join("cards");
        fs::create_dir_all(&cards).unwrap();
        let card = json!({"body": [{"text": "Hello"}]});
        fs::write(
            cards.join("welcome.json"),
            serde_json::to_string_pretty(&card).unwrap(),
        )
        .unwrap();

        write_card_i18n(&cards, "nl").unwrap();

        let i18n = assets.join("i18n");
        assert!(i18n.join("_manifest.json").exists());
        assert!(i18n.join("en.json").exists());
        assert!(i18n.join("nl.json").exists());

        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(i18n.join("_manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["locales"], json!(["en", "nl"]));

        let en: Value =
            serde_json::from_str(&fs::read_to_string(i18n.join("en.json")).unwrap()).unwrap();
        assert_eq!(en["cards.welcome.body.i0.text"], "Hello");
    }

    #[test]
    fn write_card_i18n_skips_non_json_files() {
        let temp = tempfile::tempdir().unwrap();
        let assets = temp.path().join("assets");
        let cards = assets.join("cards");
        fs::create_dir_all(&cards).unwrap();
        fs::write(cards.join("readme.txt"), b"not json").unwrap();
        fs::write(
            cards.join("valid.json"),
            serde_json::to_string(&json!({"text": "Hi"})).unwrap(),
        )
        .unwrap();

        write_card_i18n(&cards, "en").unwrap();

        let en: Value =
            serde_json::from_str(&fs::read_to_string(assets.join("i18n/en.json")).unwrap())
                .unwrap();
        assert!(en.as_object().unwrap().contains_key("cards.valid..text"));
        assert!(!en.as_object().unwrap().keys().any(|k| k.contains("readme")));
    }

    #[test]
    fn write_card_i18n_with_regional_locale() {
        let temp = tempfile::tempdir().unwrap();
        let assets = temp.path().join("assets");
        let cards = assets.join("cards");
        fs::create_dir_all(&cards).unwrap();
        fs::write(
            cards.join("test.json"),
            serde_json::to_string(&json!({"text": "Test"})).unwrap(),
        )
        .unwrap();

        write_card_i18n(&cards, "fr-CA").unwrap();

        let i18n = assets.join("i18n");
        assert!(i18n.join("en.json").exists());
        assert!(i18n.join("fr-CA.json").exists());
        assert!(i18n.join("fr.json").exists());
    }

    #[test]
    fn write_card_i18n_skips_malformed_json() {
        let temp = tempfile::tempdir().unwrap();
        let assets = temp.path().join("assets");
        let cards = assets.join("cards");
        fs::create_dir_all(&cards).unwrap();
        fs::write(cards.join("bad.json"), b"not valid json {{{").unwrap();
        fs::write(
            cards.join("good.json"),
            serde_json::to_string(&json!({"text": "OK"})).unwrap(),
        )
        .unwrap();

        write_card_i18n(&cards, "en").unwrap();
        let en: Value =
            serde_json::from_str(&fs::read_to_string(assets.join("i18n/en.json")).unwrap())
                .unwrap();
        assert!(en.as_object().unwrap().contains_key("cards.good..text"));
    }
}
