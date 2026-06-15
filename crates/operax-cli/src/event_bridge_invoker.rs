//! Production [`operax_event_bridge::OperaxInvoker`] backed by the real operax
//! runtime.
//!
//! The event bridge consumes `greentic.operala.request.v1` from NATS and needs
//! to invoke the local operax runtime. [`CoreOperaxInvoker`] holds the configured
//! handoff artifact path plus an [`HttpSorxClient`] and maps a dispatch request
//! onto a [`RunRequest`], then runs [`run_artifact_with_client`] on the blocking
//! pool (the runtime is synchronous).
//!
//! Mapping decisions (documented for future maintainers):
//! - `artifact`    -> the serve-configured artifact path (the dispatch does not
//!   carry an artifact; one OperaX serve process serves one handoff).
//! - `tenant`      -> [`RunRequest::tenant`] (from the dispatch's tenant header).
//! - `target`      -> [`RunRequest::team`] when non-empty, else the serve default
//!   team.
//! - `operation`   -> currently unused by the runtime invocation (the handoff's
//!   capability determines behavior); kept in the trait signature for parity
//!   with the wire contract.
//! - `caller_role` -> a fixed service identity (`"event-bridge"`).
//! - `dry_run`     -> the serve-configured default (defaults to true so the
//!   e2e needs no live SORX; real applies require `--no-dry-run`).

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use operax_core::RunReport;
use operax_runtime::{RunRequest, run_artifact_with_client};
use operax_sorx_http::HttpSorxClient;
use serde_json::Value;

use operax_event_bridge::{InvokeOutcome, OperaxInvoker};

/// Caller role used for all event-bridge invocations.
const EVENT_BRIDGE_ROLE: &str = "event-bridge";

/// Production [`OperaxInvoker`] wrapping the live operax runtime.
pub struct CoreOperaxInvoker {
    artifact: PathBuf,
    client: Arc<HttpSorxClient>,
    default_team: Option<String>,
    dry_run: bool,
}

impl CoreOperaxInvoker {
    /// Build an invoker over a configured handoff artifact and SORX client.
    pub fn new(
        artifact: PathBuf,
        client: HttpSorxClient,
        default_team: Option<String>,
        dry_run: bool,
    ) -> Self {
        Self {
            artifact,
            client: Arc::new(client),
            default_team,
            dry_run,
        }
    }
}

/// Map a runtime [`RunReport`] onto the bridge's transport-agnostic
/// [`InvokeOutcome`].
///
/// `ok` is `true` when the run recorded no failed operation
/// ([`RunReport::failed_operation`] is `None`). The full report is serialized
/// into `output` so the caller can surface decisions, applied/skipped counts,
/// validation errors, and any failure detail. No events are emitted here: the
/// operax run report does not carry a discrete event stream like SORX does.
fn outcome_from_report(report: &RunReport) -> InvokeOutcome {
    let ok = report.failed_operation.is_none();
    let output = serde_json::to_value(report).unwrap_or(Value::Null);
    InvokeOutcome {
        ok,
        output,
        events: vec![],
    }
}

#[async_trait]
impl OperaxInvoker for CoreOperaxInvoker {
    async fn invoke(
        &self,
        tenant: &str,
        _env: &str,
        target: &str,
        _operation: &str,
        input: Value,
        _idempotency_key: Option<&str>,
    ) -> Result<InvokeOutcome> {
        let team = if target.is_empty() {
            self.default_team.clone()
        } else {
            Some(target.to_string())
        };

        let request = RunRequest {
            artifact: self.artifact.clone(),
            tenant: tenant.to_string(),
            team,
            locale: None,
            caller_role: Some(EVENT_BRIDGE_ROLE.to_string()),
            input,
            dry_run: self.dry_run,
            audit_dir: None,
        };

        // `run_artifact_with_client` is synchronous and performs blocking pack
        // loading and (when not dry-run) HTTP I/O, so run it on the blocking
        // pool to avoid stalling the async reactor driving the NATS subscription.
        let client = self.client.clone();
        let report =
            tokio::task::spawn_blocking(move || run_artifact_with_client(request, client.as_ref()))
                .await
                .context("operax invoke task panicked")?
                .map_err(|err| anyhow::anyhow!("operax run failed: {err}"))?;

        Ok(outcome_from_report(&report))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use operax_core::RunReport;
    use serde_json::json;

    fn base_report() -> RunReport {
        RunReport::reconciliation(2, vec![], 1, 0)
    }

    #[test]
    fn no_failed_operation_maps_to_ok_true() {
        let report = base_report();
        let outcome = outcome_from_report(&report);
        assert!(outcome.ok);
        // Full report is serialized into output.
        assert_eq!(outcome.output["input_count"], json!(2));
        assert_eq!(outcome.output["applied_actions"], json!(1));
        assert!(outcome.events.is_empty());
    }

    #[test]
    fn failed_operation_maps_to_ok_false_but_keeps_output() {
        let mut report = base_report();
        report.failed_operation = Some(json!({"reason": "boom"}));
        let outcome = outcome_from_report(&report);
        assert!(!outcome.ok);
        assert_eq!(outcome.output["failed_operation"]["reason"], json!("boom"));
    }
}
