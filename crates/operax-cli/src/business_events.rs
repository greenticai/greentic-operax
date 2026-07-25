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
    // Drop a trailing version segment if present (e.g. ".../v1/name"), but only when at
    // least one segment would remain as `domain` — otherwise a pack literally named
    // `v1`/`v2` would resolve to a blank domain.
    if segs.len() > 1
        && segs.last().is_some_and(|s| {
            s.len() >= 2 && s.starts_with('v') && s[1..].chars().all(|c| c.is_ascii_digit())
        })
    {
        segs.pop();
    }
    let domain = segs.join("-");
    Some((domain, name.to_string()))
}

/// Returns `true` when `env` is a delivery for the business event declared by `sub`.
///
/// The cap string alone can't tell whether SoRX published this as an entity-lifecycle
/// (CRUD) event or a command event, so both publish shapes are tried:
/// - entity form: `<pack>/<Entity>.<op>` → `sorla.<san(pack)>.<san(Entity)>.<san(op)>`
///   (each dot-separated part of `name` sanitized independently, dots kept as separators).
/// - command form: `<pack>/<event_name>` → `sorla.<san(pack)>.<san(event_name)>`
///   (`name` sanitized as a single segment, dots collapsed to dashes).
pub fn event_matches(sub: &EventSubscription, env: &EventEnvelope) -> bool {
    let Some((domain, name)) = cap_domain_name(&sub.capability) else {
        return false;
    };
    let san_domain = sanitize_segment(&domain);

    let entity_tail = format!(
        "{san_domain}.{}",
        name.split('.')
            .map(sanitize_segment)
            .collect::<Vec<_>>()
            .join(".")
    );
    let command_tail = format!("{san_domain}.{}", sanitize_segment(&name));

    [entity_tail, command_tail].into_iter().any(|tail| {
        env.topic == format!("sorla.{tail}") || env.topic.ends_with(&format!(".{tail}"))
    })
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
    fn matches_command_event_topic() {
        // Command event: cap domain=landlord-tenant-sor name=tenant.code_generated (dots
        // collapsed to dashes as a single topic segment)  ↔  sorla.landlord-tenant-sor.tenant-code_generated
        assert!(event_matches(
            &sub("cap://greentic/events/landlord-tenant-sor/tenant.code_generated"),
            &env_with_topic("sorla.landlord-tenant-sor.tenant-code_generated")
        ));
    }

    #[test]
    fn matches_entity_lifecycle_topic() {
        // Entity-lifecycle (CRUD) event: cap domain=landlord name=Tenant.created — the `.`
        // between Entity and op is a topic-segment SEPARATOR, not part of one sanitized
        // segment. Previously broken: the matcher only tried the command-form collapse.
        assert!(event_matches(
            &sub("cap://greentic/events/landlord/Tenant.created"),
            &env_with_topic("sorla.landlord.Tenant.created")
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

    #[test]
    fn matches_pack_named_like_version() {
        // A 2-segment cap whose pack is itself named like a version segment (`v2`) must
        // still parse domain="v2" — the version-strip guard must not blank it out just
        // because it's the only segment left after popping `name`.
        assert!(event_matches(
            &sub("cap://greentic/events/v2/tenant.created"),
            &env_with_topic("sorla.v2.tenant.created")
        ));
    }
}
