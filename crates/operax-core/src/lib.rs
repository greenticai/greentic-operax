use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;

pub type Result<T> = std::result::Result<T, OperaxError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperaxError {
    pub code: String,
    pub message: String,
}

impl OperaxError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for OperaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for OperaxError {}

impl From<std::io::Error> for OperaxError {
    fn from(value: std::io::Error) -> Self {
        Self::new("io_error", value.to_string())
    }
}

impl From<serde_json::Error> for OperaxError {
    fn from(value: serde_json::Error) -> Self {
        Self::new("json_error", value.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperaxContext {
    pub tenant: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    pub caller_id: String,
    pub caller_role: String,
    pub request_id: String,
    pub pack_digest: String,
}

impl OperaxContext {
    pub fn new(
        tenant: String,
        team: Option<String>,
        locale: Option<String>,
        pack_digest: String,
    ) -> Result<Self> {
        if tenant.trim().is_empty() {
            return Err(OperaxError::new(
                "missing_tenant",
                "greentic-operax run requires --tenant",
            ));
        }
        Ok(Self {
            tenant,
            team,
            locale,
            caller_id: "operax".to_string(),
            caller_role: "service".to_string(),
            request_id: unique_request_id(),
            pack_digest,
        })
    }

    pub fn sorx_headers(&self, token: Option<&str>) -> Vec<(String, String)> {
        let mut headers = vec![
            ("X-Greentic-Tenant-Id".to_string(), self.tenant.clone()),
            ("X-Greentic-Caller-Id".to_string(), self.caller_id.clone()),
            (
                "X-Greentic-Caller-Role".to_string(),
                self.caller_role.clone(),
            ),
            ("X-Greentic-Request-Id".to_string(), self.request_id.clone()),
        ];
        if let Some(team) = &self.team {
            headers.push(("X-Greentic-Team".to_string(), team.clone()));
        }
        if let Some(locale) = &self.locale {
            headers.push(("Accept-Language".to_string(), locale.clone()));
        }
        if let Some(token) = token {
            headers.push(("Authorization".to_string(), format!("Bearer {token}")));
        }
        headers
    }

    pub fn with_caller_role(mut self, caller_role: impl Into<String>) -> Result<Self> {
        let caller_role = caller_role.into();
        if caller_role.trim().is_empty() {
            return Err(OperaxError::new(
                "missing_caller_role",
                "caller role must not be empty",
            ));
        }
        self.caller_role = caller_role;
        Ok(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OperaHandoffMetadata {
    pub schema: String,
    pub capability: String,
    pub extension: String,
    #[serde(default)]
    pub tenant_required: bool,
    #[serde(default)]
    pub team_optional: bool,
    #[serde(default)]
    pub team_required: bool,
    #[serde(default)]
    pub sorla: Option<HandoffSorla>,
    #[serde(default)]
    pub sorx: Option<HandoffSorx>,
    #[serde(default)]
    pub requires: Vec<CapabilityRequirement>,
    #[serde(default)]
    pub consumes: Vec<EventSubscription>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CapabilityRequirement {
    pub id: String,
    pub capability: String,
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventSubscription {
    pub id: String,
    pub capability: String,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HandoffSorla {
    #[serde(default)]
    pub source_digest: Option<String>,
    #[serde(default)]
    pub parser: Option<String>,
    #[serde(default)]
    pub required_schema: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HandoffSorx {
    pub transport: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FlowDefinition {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchemaRegistry {
    pub names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SorxBinding {
    pub transport: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OperationalPack {
    pub metadata: OperaHandoffMetadata,
    #[serde(default)]
    pub handoff: Value,
    pub flows: Vec<FlowDefinition>,
    pub schemas: SchemaRegistry,
    #[serde(default)]
    pub schema_documents: BTreeMap<String, Value>,
    pub sorx_binding: SorxBinding,
    pub pack_digest: String,
    pub sorla_source_digest: String,
    pub handoff_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MatchedRecord {
    pub record: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SorxTarget {
    BusinessAction {
        id: String,
        version: String,
        contract_hash: String,
    },
    GeneratedRoute {
        endpoint_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        operation_id: Option<String>,
        method: String,
        path: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProposedAction {
    pub sorx_target: SorxTarget,
    pub operation: ActionOperation,
    #[serde(default)]
    pub values: Map<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionOperation {
    DryRun,
    Invoke,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionAudit {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sorla_source_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operala_handoff_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionEnvelope {
    pub schema: String,
    pub tenant: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    pub capability: String,
    pub input_id: String,
    pub outcome: String,
    pub confidence: f64,
    #[serde(default)]
    pub matched_records: Vec<MatchedRecord>,
    pub proposed_actions: Vec<ProposedAction>,
    #[serde(default)]
    pub requires_approval: bool,
    pub reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit: Option<DecisionAudit>,
}

impl DecisionEnvelope {
    pub fn with_context(mut self, ctx: &OperaxContext, pack: &OperationalPack) -> Self {
        self.tenant = ctx.tenant.clone();
        self.team = ctx.team.clone();
        self.audit = Some(DecisionAudit {
            sorla_source_digest: Some(pack.sorla_source_digest.clone()),
            operala_handoff_digest: Some(pack.handoff_digest.clone()),
        });
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub input_count: usize,
    pub decisions: Vec<DecisionEnvelope>,
    pub applied_actions: usize,
    pub skipped_actions: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub planned_counts: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validation_errors: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub planned_operations: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_counts: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_operations: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_operation: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sorx_responses: Option<Vec<Value>>,
}

impl RunReport {
    pub fn reconciliation(
        input_count: usize,
        decisions: Vec<DecisionEnvelope>,
        applied_actions: usize,
        skipped_actions: usize,
    ) -> Self {
        Self {
            status: None,
            input_count,
            decisions,
            applied_actions,
            skipped_actions,
            dry_run: None,
            batch_id: None,
            planned_counts: None,
            validation_errors: Vec::new(),
            planned_operations: None,
            created_counts: None,
            completed_operations: None,
            failed_operation: None,
            sorx_responses: None,
        }
    }
}

pub fn sha256_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity("sha256:".len() + digest.len() * 2);
    out.push_str("sha256:");
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub fn looks_secret_like(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            let lower = key.to_ascii_lowercase();
            (lower.contains("password")
                || lower.contains("api_key")
                || lower.contains("client_secret")
                || lower == "token"
                || lower.ends_with("_token"))
                || looks_secret_like(value)
        }),
        Value::Array(items) => items.iter().any(looks_secret_like),
        Value::String(text) => {
            text.contains("sorx.customer.example")
                || text.contains("https://sorx.customer.")
                || text.contains("client_secret")
        }
        _ => false,
    }
}

fn unique_request_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("operax-{nanos}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_requires_tenant_and_builds_sorx_headers() {
        assert!(OperaxContext::new("".into(), None, None, "sha256:x".into()).is_err());
        let ctx = OperaxContext::new(
            "demo".into(),
            Some("ops".into()),
            Some("en-GB".into()),
            "sha256:x".into(),
        )
        .unwrap();
        let headers = ctx.sorx_headers(Some("secret"));
        assert!(headers.contains(&("X-Greentic-Tenant-Id".into(), "demo".into())));
        assert!(headers.contains(&("X-Greentic-Caller-Id".into(), "operax".into())));
        assert!(headers.contains(&("X-Greentic-Team".into(), "ops".into())));
        assert!(headers.contains(&("Authorization".into(), "Bearer secret".into())));
    }

    #[test]
    fn detects_secret_like_values() {
        let value = serde_json::json!({"auth": {"client_secret": "plain"}});
        assert!(looks_secret_like(&value));
    }

    #[test]
    fn metadata_defaults_capabilities_and_events() {
        let metadata: OperaHandoffMetadata = serde_yaml::from_str(
            r#"
schema: greentic.operala.handoff.v1
capability: reconciliation
extension: greentic.operala.reconciliation.v1
sorx:
  transport: http
  url: runtime-provided
"#,
        )
        .unwrap();
        assert!(metadata.requires.is_empty());
        assert!(metadata.consumes.is_empty());
    }

    #[test]
    fn metadata_parses_capabilities_and_events() {
        let metadata: OperaHandoffMetadata = serde_yaml::from_str(
            r#"
schema: greentic.operala.handoff.v1
capability: reconciliation
extension: greentic.operala.reconciliation.v1
sorx:
  transport: http
  url: runtime-provided
requires:
  - id: operax.needs.record-rent-payment
    capability: cap://greentic/sorx/tenancy/v1/functions/record-rent-payment
    metadata:
      kind: business_function
      action_id: record_rent_payment
      version: "0.1.0"
consumes:
  - id: operax.subscribes.work-order-assigned
    capability: cap://greentic/events/boiler-maintenance/v1/work-order-assigned
    mode: shared
    metadata:
      kind: business_event_subscription
"#,
        )
        .unwrap();
        assert_eq!(metadata.requires.len(), 1);
        assert_eq!(
            metadata.requires[0].metadata["action_id"],
            "record_rent_payment"
        );
        assert_eq!(metadata.consumes.len(), 1);
        assert_eq!(metadata.consumes[0].mode.as_deref(), Some("shared"));
    }
}
