pub fn sanitize_id(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

pub fn route_card_id(target: &str) -> String {
    if target == "dashboard" {
        return "operax_dashboard".to_string();
    }
    target
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_id_preserves_alphanumeric_and_dots() {
        assert_eq!(sanitize_id("hello-world_1.0"), "hello-world_1.0");
    }

    #[test]
    fn sanitize_id_replaces_special_chars_with_dash() {
        assert_eq!(sanitize_id("foo bar/baz@qux"), "foo-bar-baz-qux");
    }

    #[test]
    fn sanitize_id_empty_string() {
        assert_eq!(sanitize_id(""), "");
    }

    #[test]
    fn sanitize_id_only_special_chars() {
        assert_eq!(sanitize_id("!@#$"), "----");
    }

    #[test]
    fn sanitize_id_unicode() {
        assert_eq!(sanitize_id("cafe\u{0301}"), "cafe-");
    }

    #[test]
    fn route_card_id_dashboard_maps_to_operax_dashboard() {
        assert_eq!(route_card_id("dashboard"), "operax_dashboard");
    }

    #[test]
    fn route_card_id_preserves_alphanumeric_underscore_dash() {
        assert_eq!(route_card_id("my_card-1"), "my_card-1");
    }

    #[test]
    fn route_card_id_replaces_special_chars_with_underscore() {
        assert_eq!(route_card_id("cards/detail.view"), "cards_detail_view");
    }

    #[test]
    fn route_card_id_empty_string() {
        assert_eq!(route_card_id(""), "");
    }

    #[test]
    fn route_card_id_spaces_replaced() {
        assert_eq!(route_card_id("my card"), "my_card");
    }
}
