use operax_core::{DecisionEnvelope, OperaxContext, OperaxError, ProposedAction, Result};
use serde::Serialize;
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct JsonlAuditSink {
    path: PathBuf,
}

impl JsonlAuditSink {
    pub fn new(audit_dir: Option<&Path>) -> Self {
        let path = audit_dir
            .map(|dir| dir.join("audit.jsonl"))
            .unwrap_or_else(|| PathBuf::from("target/operax/audit.jsonl"));
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write_event<T: Serialize>(&self, event: &str, payload: &T) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let value = json!({
            "event": event,
            "timestamp": timestamp(),
            "payload": payload
        });
        let line = serde_json::to_string(&value).map_err(OperaxError::from)?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    pub fn run_started(&self, ctx: &OperaxContext) -> Result<()> {
        self.write_event("operax.run.started", ctx)
    }

    pub fn decision_created(&self, decision: &DecisionEnvelope) -> Result<()> {
        self.write_event("operax.decision.created", decision)
    }

    pub fn action_applied(&self, ctx: &OperaxContext, action: &ProposedAction) -> Result<()> {
        self.write_event(
            "operax.action.applied",
            &redacted_action_payload(ctx, action, "applied"),
        )
    }

    pub fn action_skipped(&self, ctx: &OperaxContext, action: &ProposedAction) -> Result<()> {
        self.write_event(
            "operax.action.skipped",
            &redacted_action_payload(ctx, action, "skipped"),
        )
    }

    pub fn run_completed(&self, ctx: &OperaxContext) -> Result<()> {
        self.write_event("operax.run.completed", ctx)
    }

    pub fn run_failed(&self, ctx: Option<&OperaxContext>, error: &OperaxError) -> Result<()> {
        self.write_event(
            "operax.run.failed",
            &json!({
                "tenant": ctx.map(|ctx| ctx.tenant.clone()),
                "team": ctx.and_then(|ctx| ctx.team.clone()),
                "code": error.code,
                "message": error.message,
            }),
        )
    }
}

fn redacted_action_payload(ctx: &OperaxContext, action: &ProposedAction, status: &str) -> Value {
    json!({
        "tenant": ctx.tenant,
        "team": ctx.team,
        "request_id": ctx.request_id,
        "status": status,
        "sorx_target": action.sorx_target,
        "idempotency_key": action.idempotency_key,
    })
}

fn timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    format!("{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_jsonl_audit_events() {
        let temp = tempfile::tempdir().unwrap();
        let sink = JsonlAuditSink::new(Some(temp.path()));
        let ctx = OperaxContext::new("demo".into(), None, None, "sha256:x".into()).unwrap();
        sink.run_started(&ctx).unwrap();
        let body = std::fs::read_to_string(sink.path()).unwrap();
        assert!(body.contains("operax.run.started"));
        assert!(!body.contains("client_secret"));
    }
}
