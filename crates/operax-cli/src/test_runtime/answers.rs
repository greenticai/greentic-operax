use serde_json::{Value, json};
use std::path::Path;

pub const WEBCHAT_REF: &str =
    "oci://ghcr.io/greenticai/packs/messaging/messaging-webchat-gui:stable";

pub fn create_answers_value(locale: &str, bundle_id: &str, bundle_dir: &Path) -> Value {
    json!({
        "wizard_id": "greentic-bundle.wizard.run",
        "schema_id": "greentic-bundle.wizard.answers",
        "schema_version": "1.0.0",
        "locale": locale,
        "answers": {
            "access_rules": [],
            "advanced_setup": false,
            "app_pack_entries": [],
            "app_packs": [],
            "bundle_id": bundle_id,
            "bundle_name": bundle_id,
            "export_intent": false,
            "extension_provider_entries": [{
                "detected_kind": "oci",
                "display_name": "Greentic Messaging WebChat GUI (stable)",
                "provider_id": "greentic.messaging.webchat-gui.stable",
                "reference": WEBCHAT_REF,
                "version": "stable"
            }],
            "extension_providers": [WEBCHAT_REF],
            "mode": "create",
            "output_dir": bundle_dir,
            "remote_catalogs": [],
            "setup_answers": {},
            "setup_execution_intent": false,
            "setup_specs": {}
        }
    })
}

pub fn setup_answers_value(webchat_url_base: &str, operax_url_base: &str) -> Value {
    json!({
        "bundle_source": ".",
        "env": "dev",
        "greentic_setup_version": "1.0.0",
        "platform_setup": {
            "deployment_targets": [],
            "static_routes": {
                "default_route_prefix_policy": "pack_declared",
                "public_base_url": webchat_url_base,
                "public_surface_policy": "enabled",
                "public_web_enabled": true,
                "tenant_path_policy": "pack_declared"
            },
            "tunnel": {"mode": "off"}
        },
        "setup_answers": {
            "messaging-webchat-gui": {
                "base_url": webchat_url_base,
                "jwt_signing_key": "operax-manager-local-signing-key-0123456789abcdef",
                "mode": "local_queue",
                "nav_links": [
                    {
                        "id": "operax-manager",
                        "label": "OperaX Manager",
                        "url": format!("{operax_url_base}/v1/operax/manager")
                    },
                    {
                        "id": "operax-dashboard-card",
                        "label": "Dashboard Card",
                        "url": format!("{operax_url_base}/v1/operax/manager/cards/dashboard")
                    }
                ],
                "presentation_mode": "standalone",
                "public_base_url": webchat_url_base,
                "route": "webchat",
                "skin": "default",
                "tenant_channel_id": "demo:webchat",
                "text_input_enabled": false
            }
        },
        "team": "default",
        "tenant": "demo"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn create_answers_has_correct_structure() {
        let bundle_dir = PathBuf::from("/tmp/test-bundle");
        let value = create_answers_value("en-GB", "operax-manager-handoff", &bundle_dir);
        assert_eq!(value["wizard_id"], "greentic-bundle.wizard.run");
        assert_eq!(value["schema_id"], "greentic-bundle.wizard.answers");
        assert_eq!(value["locale"], "en-GB");
        assert_eq!(value["answers"]["bundle_id"], "operax-manager-handoff");
        assert_eq!(value["answers"]["bundle_name"], "operax-manager-handoff");
        assert_eq!(value["answers"]["mode"], "create");
        assert_eq!(value["answers"]["extension_providers"][0], WEBCHAT_REF);
    }

    #[test]
    fn create_answers_extension_provider_entry() {
        let bundle_dir = PathBuf::from("/tmp/test");
        let value = create_answers_value("en", "test-bundle", &bundle_dir);
        let entry = &value["answers"]["extension_provider_entries"][0];
        assert_eq!(entry["detected_kind"], "oci");
        assert_eq!(
            entry["provider_id"],
            "greentic.messaging.webchat-gui.stable"
        );
        assert_eq!(entry["reference"], WEBCHAT_REF);
        assert_eq!(entry["version"], "stable");
    }

    #[test]
    fn create_answers_output_dir_matches() {
        let bundle_dir = PathBuf::from("/home/user/bundles/my-bundle");
        let value = create_answers_value("nl", "my-bundle", &bundle_dir);
        assert_eq!(
            value["answers"]["output_dir"],
            "/home/user/bundles/my-bundle"
        );
    }

    #[test]
    fn create_answers_defaults_are_set() {
        let bundle_dir = PathBuf::from("/tmp/x");
        let value = create_answers_value("en", "x", &bundle_dir);
        assert_eq!(value["answers"]["advanced_setup"], false);
        assert_eq!(value["answers"]["export_intent"], false);
        assert_eq!(value["answers"]["setup_execution_intent"], false);
        assert!(
            value["answers"]["access_rules"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn setup_answers_has_correct_structure() {
        let value = setup_answers_value("http://127.0.0.1:8080", "http://127.0.0.1:8797");
        assert_eq!(value["bundle_source"], ".");
        assert_eq!(value["env"], "dev");
        assert_eq!(value["team"], "default");
        assert_eq!(value["tenant"], "demo");
    }

    #[test]
    fn setup_answers_webchat_config() {
        let value = setup_answers_value("http://127.0.0.1:8080", "http://127.0.0.1:8797");
        let webchat = &value["setup_answers"]["messaging-webchat-gui"];
        assert_eq!(webchat["base_url"], "http://127.0.0.1:8080");
        assert_eq!(webchat["public_base_url"], "http://127.0.0.1:8080");
        assert_eq!(webchat["mode"], "local_queue");
        assert_eq!(webchat["route"], "webchat");
        assert_eq!(webchat["skin"], "default");
        assert_eq!(webchat["text_input_enabled"], false);
        assert_eq!(webchat["presentation_mode"], "standalone");
    }

    #[test]
    fn setup_answers_nav_links_contain_operax_urls() {
        let value = setup_answers_value("http://127.0.0.1:8080", "http://localhost:9000");
        let links = value["setup_answers"]["messaging-webchat-gui"]["nav_links"]
            .as_array()
            .unwrap();
        assert_eq!(links.len(), 2);
        assert_eq!(links[0]["url"], "http://localhost:9000/v1/operax/manager");
        assert_eq!(
            links[1]["url"],
            "http://localhost:9000/v1/operax/manager/cards/dashboard"
        );
    }

    #[test]
    fn setup_answers_static_routes() {
        let value = setup_answers_value("http://127.0.0.1:8080", "http://127.0.0.1:8797");
        let routes = &value["platform_setup"]["static_routes"];
        assert_eq!(routes["public_base_url"], "http://127.0.0.1:8080");
        assert_eq!(routes["public_surface_policy"], "enabled");
        assert_eq!(routes["public_web_enabled"], true);
    }
}
