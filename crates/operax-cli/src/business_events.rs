//! Pure cap↔topic matcher for OperaX business-event subscriptions.
//!
//! Maps an `operala.yaml` `consumes[].capability` entry (an events cap URI) to
//! the NATS topic suffix that SoRX actually publishes, and checks whether a
//! received [`EventEnvelope`] matches a declared [`EventSubscription`].
//!
//! `event_matches` is currently exercised only by this module's unit tests;
//! the subscriber that calls it in production lands in a follow-up task.
#![allow(dead_code)]

use greentic_types::EventEnvelope;
use operax_core::EventSubscription;

const EVENTS_CAP_PREFIX: &str = "cap://greentic/events/";

/// Keep `[A-Za-z0-9_-]`, map every other char (incl. `.`) to `-` — mirrors SoRX's topic-segment
/// sanitization so a declared cap resolves to the topic SoRX actually publishes.
fn sanitize_segment(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Parse an events cap into `(domain, name)`, tolerating an optional `vN` version segment
/// (`cap://greentic/events/<domain>/[v1/]<name>`). Returns `None` for non-events caps.
fn cap_domain_name(cap: &str) -> Option<(String, String)> {
    let rest = cap.strip_prefix(EVENTS_CAP_PREFIX)?;
    let mut segs: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() < 2 {
        return None;
    }
    let name = segs.pop()?;
    // Drop a trailing version segment if present (e.g. ".../v1/name").
    if segs.last().is_some_and(|s| {
        s.len() >= 2 && s.starts_with('v') && s[1..].chars().all(|c| c.is_ascii_digit())
    }) {
        segs.pop();
    }
    let domain = segs.join("-");
    Some((domain, name.to_string()))
}

/// Returns `true` when `env` is a delivery for the business event declared by `sub`.
pub fn event_matches(sub: &EventSubscription, env: &EventEnvelope) -> bool {
    let Some((domain, name)) = cap_domain_name(&sub.capability) else {
        return false;
    };
    let suffix = format!("{}.{}", sanitize_segment(&domain), sanitize_segment(&name));
    env.topic == format!("sorla.{suffix}") || env.topic.ends_with(&format!(".{suffix}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;
    use greentic_types::{EnvId, EventEnvelope, EventId, TenantCtx, TenantId};
    use operax_core::EventSubscription;

    fn sub(cap: &str) -> EventSubscription {
        EventSubscription {
            id: "s".into(),
            capability: cap.into(),
            mode: None,
            metadata: Default::default(),
        }
    }

    fn env_with_topic(topic: &str) -> EventEnvelope {
        EventEnvelope {
            id: EventId::new("evt-1").expect("valid id"),
            topic: topic.to_string(),
            r#type: "cap://greentic/events/dummy/dummy".to_string(),
            source: "test".to_string(),
            tenant: TenantCtx::new(
                EnvId::try_from("prod").expect("valid env"),
                TenantId::try_from("tenant-1").expect("valid tenant"),
            ),
            subject: None,
            time: DateTime::UNIX_EPOCH,
            correlation_id: None,
            payload: serde_json::Value::Null,
            metadata: Default::default(),
        }
    }

    #[test]
    fn matches_entity_topic() {
        // cap domain=landlord-tenant-sor name=tenant.code_generated  ↔  topic sorla.landlord-tenant-sor.tenant-code_generated
        assert!(event_matches(
            &sub("cap://greentic/events/landlord-tenant-sor/tenant.code_generated"),
            &env_with_topic("sorla.landlord-tenant-sor.tenant-code_generated")
        ));
    }

    #[test]
    fn does_not_match_wrong_name() {
        assert!(!event_matches(
            &sub("cap://greentic/events/landlord/tenant.created"),
            &env_with_topic("sorla.landlord.other-event")
        ));
    }

    #[test]
    fn ignores_non_business_event_cap() {
        assert!(!event_matches(
            &sub("cap://greentic/business-functions/x/y"),
            &env_with_topic("sorla.x.y")
        ));
    }

    #[test]
    fn matches_versioned_cap() {
        // Real operala.yaml shape: cap://greentic/events/{domain}/v1/{name}
        assert!(event_matches(
            &sub("cap://greentic/events/boiler-maintenance/v1/work-order-assigned"),
            &env_with_topic("sorla.boiler-maintenance.work-order-assigned")
        ));
    }
}
