//! Cap↔topic matcher and NATS subscriber loop for OperaX business-event
//! subscriptions.
//!
//! Maps an `operala.yaml` `consumes[].capability` entry (an events cap URI) to
//! the NATS topic suffix that SoRX actually publishes, checks whether a
//! received [`EventEnvelope`] matches a declared [`EventSubscription`]
//! (`event_matches`), and dispatches matching events to `run_artifact_with_client`
//! via [`run_subscriber`].

use std::path::{Path, PathBuf};

use futures::StreamExt;
use greentic_types::EventEnvelope;
use operax_core::{EventSubscription, RunReport};
use operax_pack_loader::load_operational_pack;
use operax_runtime::{RunRequest, run_artifact_with_client};
use operax_sorx_http::HttpSorxClient;

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

    [entity_tail, command_tail]
        .into_iter()
        .any(|tail| env.topic == format!("sorla.{tail}"))
}

/// Configuration for [`run_subscriber`]: the NATS endpoint to connect to, the
/// tenant subject scope to subscribe on, the artifact whose declared
/// `consumes` subscriptions gate dispatch, and how to reach SoRX for the
/// resulting runs.
pub struct SubscriberConfig {
    pub nats_url: String,
    pub tenant: String,
    pub artifact: PathBuf,
    pub sorx_base_url: String,
    pub sorx_token: Option<String>,
}

/// Builds the `RunRequest` for dispatching `env` to `artifact` on behalf of the
/// declared subscription `sub`. Pure: no I/O.
///
/// `sub` isn't consulted for `RunRequest` fields today (routing already
/// happened via `event_matches`); it's kept in the signature so future
/// per-subscription overrides (team/locale) have somewhere to read from.
fn request_for(env: &EventEnvelope, _sub: &EventSubscription, artifact: &Path) -> RunRequest {
    RunRequest {
        artifact: artifact.to_path_buf(),
        tenant: env.tenant.tenant.to_string(),
        team: None,
        locale: None,
        caller_role: Some("business-event".to_string()),
        input: env.payload.clone(),
        dry_run: false,
        audit_dir: None,
    }
}

/// Classifies a completed run for logging: `"ok"` when SoRX reported no failed
/// operation, `"failed"` otherwise.
fn report_outcome(report: &RunReport) -> &'static str {
    if report.failed_operation.is_none() {
        "ok"
    } else {
        "failed"
    }
}

/// Runs the NATS subscription loop until the connection ends. Blocking: spins its own
/// current-thread tokio runtime, so the caller should run this on a dedicated thread.
///
/// Not exercised by this task's unit tests (no live NATS broker available here); a
/// live-NATS integration test is deferred to a follow-up task. Wired to the CLI via
/// the `operax events subscribe` subcommand (`lib.rs::run_events_subscribe`).
pub fn run_subscriber(config: SubscriberConfig) -> anyhow::Result<()> {
    let pack = load_operational_pack(&config.artifact)?;
    let subscriptions = pack.metadata.consumes.clone();
    if subscriptions.is_empty() {
        eprintln!(
            "no `consumes` subscriptions declared in {}; nothing to subscribe",
            config.artifact.display()
        );
        return Ok(());
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let client = async_nats::connect(&config.nats_url).await?;
        let subject = format!("greentic.events.{}.>", config.tenant);
        let mut subscriber = client.subscribe(subject.clone()).await?;
        eprintln!("subscribed to {subject}");
        while let Some(msg) = subscriber.next().await {
            let env: EventEnvelope = match serde_json::from_slice(&msg.payload) {
                Ok(env) => env,
                Err(err) => {
                    eprintln!("skip undecodable event on {}: {err}", msg.subject);
                    continue;
                }
            };
            for subscription in &subscriptions {
                if !event_matches(subscription, &env) {
                    continue;
                }
                let request = request_for(&env, subscription, &config.artifact);
                let sorx =
                    HttpSorxClient::new(config.sorx_base_url.clone(), config.sorx_token.clone());
                let topic = env.topic.clone();
                let artifact = config.artifact.clone();
                match tokio::task::spawn_blocking(move || run_artifact_with_client(request, &sorx))
                    .await
                {
                    Ok(Ok(report)) => {
                        eprintln!(
                            "ran {} for {topic}: {}",
                            artifact.display(),
                            report_outcome(&report)
                        );
                    }
                    Ok(Err(err)) => eprintln!("run failed for {topic}: {err}"),
                    Err(join_err) => eprintln!("run task panicked for {topic}: {join_err}"),
                }
            }
        }
        Ok::<(), anyhow::Error>(())
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
    fn does_not_match_other_pack_same_name() {
        // A different pack (`billing`) publishing the same entity/op tail (`tenant.created`)
        // must NOT match a cap declared for pack `landlord` — the matcher must not fall back
        // to a topic-suffix match, only the exact `sorla.<pack>.<tail>` topic counts.
        assert!(!event_matches(
            &sub("cap://greentic/events/tenant/created"),
            &env_with_topic("sorla.billing.tenant.created")
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

    #[test]
    fn routes_matching_event_to_run_request() {
        let mut env = env_with_topic("sorla.landlord.tenant-created");
        env.payload = serde_json::json!({"tenant_id": "tenant-1", "unit": "42"});
        let s = sub("cap://greentic/events/landlord/tenant.created");

        let req = request_for(&env, &s, std::path::Path::new("/x.gtpack"));

        assert_eq!(req.tenant, env.tenant.tenant.to_string());
        assert_eq!(req.input, env.payload);
        assert_eq!(req.caller_role.as_deref(), Some("business-event"));
        assert!(!req.dry_run);
        assert_eq!(req.artifact, std::path::PathBuf::from("/x.gtpack"));
        assert_eq!(req.team, None);
        assert_eq!(req.audit_dir, None);
    }
}
