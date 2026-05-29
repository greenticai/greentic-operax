use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
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
    pub flows: Vec<FlowDefinition>,
    pub schemas: SchemaRegistry,
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
    pub input_count: usize,
    pub decisions: Vec<DecisionEnvelope>,
    pub applied_actions: usize,
    pub skipped_actions: usize,
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
}
