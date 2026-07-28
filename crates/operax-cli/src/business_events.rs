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

/// Returns `true` when `env` is a delivery for the business event declared by `sub`.
///
/// Delegates to `operax_core::topic_matches`, which owns the cap↔topic sanitization and
/// entity/command publish-shape matching logic (moved out of this crate in slice 3).
pub fn event_matches(sub: &EventSubscription, env: &EventEnvelope) -> bool {
    operax_core::topic_matches(&sub.capability, &env.topic)
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

/// Routes NATS business events into the deployment registry. Subscribes to
/// `greentic.events.>` (all tenants, not tenant-scoped) and, for each decoded
/// [`EventEnvelope`], calls [`operax_manager::deployment::DeploymentManager::route_event`]
/// for a real (non-dry-run) routing pass, logging each [`operax_manager::deployment::RouteOutcome`].
///
/// Blocking: spins its own current-thread tokio runtime, so the caller should run this
/// on a dedicated thread. Not exercised by unit tests here (no live NATS broker available);
/// the routing logic itself is covered by `route_event`'s own tests and a follow-up e2e task.
pub fn run_event_router(
    nats_url: String,
    manager: std::sync::Arc<operax_manager::deployment::DeploymentManager>,
) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let client = async_nats::connect(&nats_url).await?;
        let mut subscriber = client.subscribe("greentic.events.>").await?;
        eprintln!("[operax serve] event router subscribed to greentic.events.>");
        while let Some(msg) = subscriber.next().await {
            let env: EventEnvelope = match serde_json::from_slice(&msg.payload) {
                Ok(env) => env,
                Err(err) => {
                    eprintln!("skip undecodable event on {}: {err}", msg.subject);
                    continue;
                }
            };
            let event_env = env.tenant.env.as_str().to_string();
            let tenant = env.tenant.tenant.to_string();
            let topic = env.topic.clone();
            let payload = env.payload.clone();
            let matched = manager.matching_deployments(&event_env, &tenant, &topic);
            if matched.is_empty() {
                eprintln!("[operax serve] event {topic} matched no deployments");
                continue;
            }
            let handles: Vec<_> = matched
                .into_iter()
                .map(|id| {
                    let mgr = manager.clone();
                    let payload = payload.clone();
                    let topic = topic.clone();
                    tokio::task::spawn_blocking(move || {
                        let result = mgr.run_as(&id, payload, false, Some("business-event"));
                        (topic, id, result)
                    })
                })
                .collect();
            for joined in futures::future::join_all(handles).await {
                match joined {
                    Ok((topic, id, Ok(_))) => {
                        eprintln!("[operax serve] routed {topic} -> {id}: ok")
                    }
                    Ok((topic, id, Err(e))) => {
                        eprintln!("[operax serve] routed {topic} -> {id}: failed: {e:?}")
                    }
                    Err(join_err) => eprintln!("[operax serve] route task panicked: {join_err}"),
                }
            }
        }
        eprintln!("[operax serve] event router subscription ended; routing stopped");
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
