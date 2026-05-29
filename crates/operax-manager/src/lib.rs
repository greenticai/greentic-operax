use operax_core::{OperationalPack, OperaxContext, OperaxError, Result, RunReport};
use operax_pack_loader::load_operational_pack;
use operax_runtime::run_loaded_pack;
use operax_sorx_http::SorxClient;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct ManagerOptions {
    pub artifact: PathBuf,
    pub tenant: String,
    pub team: Option<String>,
    pub locale: Option<String>,
    pub bind: String,
    pub audit_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagerRunResult {
    pub schema: String,
    pub run_id: String,
    pub report: RunReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card: Option<Value>,
}

#[derive(Debug, Clone)]
struct StoredRun {
    id: String,
    report: RunReport,
}

pub struct ManagerRuntime {
    pack: OperationalPack,
    tenant: String,
    team: Option<String>,
    locale: Option<String>,
    audit_dir: Option<PathBuf>,
    client: Arc<dyn SorxClient + Send + Sync>,
    runs: Mutex<VecDeque<StoredRun>>,
}

#[derive(Debug)]
pub struct ManagerResponse {
    pub status: u16,
    pub body: Value,
}

impl ManagerRuntime {
    pub fn load(
        options: ManagerOptions,
        client: Arc<dyn SorxClient + Send + Sync>,
    ) -> Result<Self> {
        let pack = load_operational_pack(&options.artifact)?;
        Ok(Self::new(
            pack,
            options.tenant,
            options.team,
            options.locale,
            options.audit_dir,
            client,
        ))
    }

    pub fn new(
        pack: OperationalPack,
        tenant: String,
        team: Option<String>,
        locale: Option<String>,
        audit_dir: Option<PathBuf>,
        client: Arc<dyn SorxClient + Send + Sync>,
    ) -> Self {
        Self {
            pack,
            tenant,
            team,
            locale,
            audit_dir,
            client,
            runs: Mutex::new(VecDeque::new()),
        }
    }

    pub fn handle_json(&self, method: &str, path: &str, body: &str) -> ManagerResponse {
        match self.try_handle_json(method, path, body) {
            Ok(response) => response,
            Err(error) => ManagerResponse {
                status: error_status(&error),
                body: error_card(&error),
            },
        }
    }

    fn try_handle_json(&self, method: &str, path: &str, body: &str) -> Result<ManagerResponse> {
        match (method, path.trim_end_matches('/')) {
            ("GET", "/healthz") | ("GET", "/readyz") => Ok(json_response(200, json!({"ok": true}))),
            ("GET", "/v1/operax/manager") | ("GET", "/manager") => {
                Ok(json_response(200, self.manager_shell()))
            }
            ("GET", "/v1/operax/manager/view") | ("GET", "/manager/view") => {
                Ok(json_response(200, self.manager_view()))
            }
            ("GET", "/v1/operax/manager/cards/dashboard") | ("GET", "/manager/cards/dashboard") => {
                Ok(json_response(200, self.dashboard_card()))
            }
            ("GET", "/v1/operax/manager/cards/input") | ("GET", "/manager/cards/input") => {
                Ok(json_response(200, self.input_card()))
            }
            ("POST", "/v1/operax/manager/submit") | ("POST", "/manager/submit") => {
                self.submit_manager(body)
            }
            ("POST", "/v1/operax/runs") => self.create_run(body),
            _ if method == "GET" && path.starts_with("/v1/operax/runs/") => {
                self.get_run(path.trim_start_matches("/v1/operax/runs/"))
            }
            _ if method == "GET" && path.starts_with("/v1/operax/manager/cards/decisions/") => {
                let run_id = path.trim_start_matches("/v1/operax/manager/cards/decisions/");
                Ok(json_response(200, self.decision_card(run_id)?))
            }
            _ => Err(OperaxError::new(
                "not_found",
                format!("manager route not found: {method} {path}"),
            )),
        }
    }

    fn submit_manager(&self, body: &str) -> Result<ManagerResponse> {
        let value: Value = parse_body(body)?;
        let input = value
            .get("input")
            .cloned()
            .or_else(|| value.get("json").cloned())
            .or_else(|| value.get("payload").cloned())
            .ok_or_else(|| {
                OperaxError::new("missing_input", "manager submit requires input JSON")
            })?;
        let mode = value
            .get("mode")
            .or_else(|| value.get("operation"))
            .and_then(Value::as_str)
            .unwrap_or("dry_run");
        let result = self.run_input(normalize_input(input)?, mode != "apply", true)?;
        Ok(json_response(
            200,
            result.card.unwrap_or_else(|| {
                self.decision_card(&result.run_id)
                    .unwrap_or_else(|_| self.dashboard_card())
            }),
        ))
    }

    fn create_run(&self, body: &str) -> Result<ManagerResponse> {
        let value: Value = parse_body(body)?;
        let input = normalize_input(value.get("input").cloned().unwrap_or(value.clone()))?;
        let mode = input_mode_from_body(body);
        let return_card = serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|value| value.get("return_card").and_then(Value::as_bool))
            .unwrap_or(false);
        let result = self.run_input(input, mode != "apply", return_card)?;
        Ok(json_response(200, serde_json::to_value(result)?))
    }

    fn run_input(
        &self,
        input: Value,
        dry_run: bool,
        return_card: bool,
    ) -> Result<ManagerRunResult> {
        let ctx = OperaxContext::new(
            self.tenant.clone(),
            self.team.clone(),
            self.locale.clone(),
            self.pack.pack_digest.clone(),
        )?;
        let report = run_loaded_pack(
            &self.pack,
            &ctx,
            input,
            dry_run,
            self.audit_dir.as_deref(),
            self.client.as_ref(),
        )?;
        let run_id = format!("run_{}", ctx.request_id.replace('-', "_"));
        self.push_run(run_id.clone(), report.clone());
        let card = return_card.then(|| self.decision_card_for_report(&run_id, &report));
        Ok(ManagerRunResult {
            schema: "greentic.operax.run-result.v1".to_string(),
            run_id,
            report,
            card,
        })
    }

    fn push_run(&self, id: String, report: RunReport) {
        let mut runs = self.runs.lock().unwrap();
        runs.push_front(StoredRun { id, report });
        while runs.len() > 20 {
            runs.pop_back();
        }
    }

    fn get_run(&self, run_id: &str) -> Result<ManagerResponse> {
        let Some(run) = self.find_run(run_id) else {
            return Err(OperaxError::new(
                "run_not_found",
                format!("run not found: {run_id}"),
            ));
        };
        Ok(json_response(
            200,
            json!({
                "schema": "greentic.operax.run-result.v1",
                "run_id": run.id,
                "report": run.report
            }),
        ))
    }

    fn find_run(&self, run_id: &str) -> Option<StoredRun> {
        self.runs
            .lock()
            .unwrap()
            .iter()
            .find(|run| run.id == run_id)
            .cloned()
    }

    fn manager_shell(&self) -> Value {
        json!({
            "schema": "greentic.operax.manager-shell.v1",
            "cards": "/v1/operax/manager/cards",
            "submit": "/v1/operax/manager/submit",
            "runs": "/v1/operax/runs"
        })
    }

    fn manager_view(&self) -> Value {
        json!({
            "schema": "greentic.operax.manager-view.v1",
            "pack": {
                "capability": self.pack.metadata.capability,
                "digest": self.pack.pack_digest,
                "handoff_digest": self.pack.handoff_digest
            },
            "tenant": self.tenant,
            "team": self.team,
            "cards": {
                "dashboard": "/v1/operax/manager/cards/dashboard",
                "input": "/v1/operax/manager/cards/input"
            }
        })
    }

    fn dashboard_card(&self) -> Value {
        adaptive_card(
            "manager.dashboard",
            vec![
                text_block("OperaX Manager", "Large", true),
                text_block(
                    &format!("Capability: {}", self.pack.metadata.capability),
                    "Default",
                    false,
                ),
                text_block(
                    &format!("Pack digest: {}", self.pack.pack_digest),
                    "Small",
                    false,
                ),
                text_block(
                    "Paste or upload JSON to process it through the current handoff pack.",
                    "Default",
                    false,
                ),
            ],
            vec![open_action("Process JSON", "input")],
            self.locale.as_deref().unwrap_or("en"),
        )
    }

    fn input_card(&self) -> Value {
        adaptive_card(
            "manager.input",
            vec![
                text_block("Process JSON", "Large", true),
                json!({
                    "type": "Input.Text",
                    "id": "input",
                    "label": "Single or batch JSON",
                    "placeholder": "{\"transaction_id\":\"...\"}",
                    "isMultiline": true,
                    "isRequired": true,
                    "errorMessage": "JSON input is required."
                }),
            ],
            vec![
                submit_action("Run dry-run", "run_dry_run"),
                submit_action("Run and apply", "apply"),
                open_action("Dashboard", "dashboard"),
            ],
            self.locale.as_deref().unwrap_or("en"),
        )
    }

    fn decision_card(&self, run_id: &str) -> Result<Value> {
        let Some(run) = self.find_run(run_id) else {
            return Err(OperaxError::new(
                "run_not_found",
                format!("run not found: {run_id}"),
            ));
        };
        Ok(self.decision_card_for_report(&run.id, &run.report))
    }

    fn decision_card_for_report(&self, run_id: &str, report: &RunReport) -> Value {
        let mut body = vec![
            text_block("OperaX Decisions", "Large", true),
            text_block(&format!("Run: {run_id}"), "Small", false),
            text_block(
                &format!(
                    "{} input(s), {} decision(s), {} applied, {} skipped",
                    report.input_count,
                    report.decisions.len(),
                    report.applied_actions,
                    report.skipped_actions
                ),
                "Default",
                false,
            ),
        ];
        for decision in &report.decisions {
            body.push(text_block(
                &format!(
                    "{}: {} ({:.0}%)",
                    decision.input_id, decision.outcome, decision.confidence
                ),
                "Default",
                decision.requires_approval,
            ));
        }
        adaptive_card(
            "manager.decisions",
            body,
            vec![open_action("Process another JSON", "input")],
            self.locale.as_deref().unwrap_or("en"),
        )
    }
}

pub fn start_manager_server(runtime: Arc<ManagerRuntime>, bind: &str) -> Result<()> {
    let listener = TcpListener::bind(bind)
        .map_err(|err| OperaxError::new("manager_bind_failed", err.to_string()))?;
    println!("OperaX manager listening on http://{bind}");
    for stream in listener.incoming() {
        let stream = stream.map_err(OperaxError::from)?;
        let runtime = runtime.clone();
        std::thread::spawn(move || {
            let _ = handle_stream(runtime, stream);
        });
    }
    Ok(())
}

fn handle_stream(runtime: Arc<ManagerRuntime>, mut stream: TcpStream) -> Result<()> {
    let mut buffer = [0u8; 1024 * 256];
    let read = stream.read(&mut buffer)?;
    let request = String::from_utf8_lossy(&buffer[..read]);
    let (head, body) = request.split_once("\r\n\r\n").unwrap_or((&request, ""));
    let mut parts = head.lines().next().unwrap_or_default().split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let path = parts.next().unwrap_or("/");
    if method == "OPTIONS" {
        write!(
            stream,
            "HTTP/1.1 204 No Content\r\n{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            cors_headers()
        )?;
        return Ok(());
    }
    let response = runtime.handle_json(method, path.split('?').next().unwrap_or(path), body);
    let body_text = serde_json::to_string_pretty(&response.body)?;
    let status_text = match response.status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\n{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response.status,
        status_text,
        cors_headers(),
        body_text.len(),
        body_text
    )?;
    Ok(())
}

fn cors_headers() -> &'static str {
    "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type, Accept, Authorization, X-Greentic-Tenant-Id, X-Greentic-Caller-Id, X-Greentic-Caller-Role, X-Greentic-Team, X-Greentic-Channel, X-Greentic-Locale, Accept-Language"
}

fn adaptive_card(kind: &str, body: Vec<Value>, actions: Vec<Value>, locale: &str) -> Value {
    json!({
        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
        "type": "AdaptiveCard",
        "version": "1.5",
        "lang": locale,
        "metadata": {
            "schema": "greentic.operax.manager-card.v1",
            "kind": kind,
            "locale": locale
        },
        "body": body,
        "actions": actions
    })
}

fn text_block(text: &str, size: &str, weight: bool) -> Value {
    let mut value = json!({"type": "TextBlock", "text": text, "wrap": true, "size": size});
    if weight {
        value["weight"] = json!("Bolder");
    }
    value
}

fn open_action(title: &str, target: &str) -> Value {
    json!({
        "type": "Action.Submit",
        "title": title,
        "data": {
            "action": "operax_manager_open",
            "manager_target": target,
            "manager_cards_base_url": "/v1/operax/manager/cards"
        }
    })
}

fn submit_action(title: &str, operation: &str) -> Value {
    json!({
        "type": "Action.Submit",
        "title": title,
        "data": {
            "action": "operax_manager_submit",
            "operation": operation,
            "manager_target": "decisions",
            "return_card": true
        }
    })
}

fn parse_body(body: &str) -> Result<Value> {
    if body.trim().is_empty() {
        return Err(OperaxError::new("missing_body", "request body is required"));
    }
    serde_json::from_str(body).map_err(OperaxError::from)
}

fn normalize_input(input: Value) -> Result<Value> {
    if let Some(text) = input.as_str() {
        return serde_json::from_str(text).map_err(OperaxError::from);
    }
    Ok(input)
}

fn input_mode_from_body(body: &str) -> &'static str {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("mode")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|mode| mode == "apply")
        .map(|_| "apply")
        .unwrap_or("dry_run")
}

fn error_status(error: &OperaxError) -> u16 {
    match error.code.as_str() {
        "not_found" | "run_not_found" => 404,
        "missing_body"
        | "missing_input"
        | "json_error"
        | "unknown_input_shape"
        | "invalid_input"
        | "invalid_batch_input"
        | "secret_like_value" => 400,
        _ => 500,
    }
}

fn error_card(error: &OperaxError) -> Value {
    adaptive_card(
        "manager.error",
        vec![
            text_block("OperaX Manager Error", "Large", true),
            text_block(
                &format!("{}: {}", error.code, error.message),
                "Default",
                false,
            ),
        ],
        vec![open_action("Dashboard", "dashboard")],
        "en",
    )
}

fn json_response(status: u16, body: Value) -> ManagerResponse {
    ManagerResponse { status, body }
}

pub fn default_bind_from_url(url: &str) -> String {
    url.trim()
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use operax_core::{HandoffSorx, OperaHandoffMetadata, SchemaRegistry, SorxBinding};
    use operax_sorx_http::MockSorxClient;

    fn pack() -> OperationalPack {
        OperationalPack {
            metadata: OperaHandoffMetadata {
                schema: "greentic.operala.handoff.v1".into(),
                capability: "reconciliation".into(),
                extension: "greentic.operala.reconciliation.v1".into(),
                tenant_required: true,
                team_optional: true,
                team_required: false,
                sorla: None,
                sorx: Some(HandoffSorx {
                    transport: "http".into(),
                    url: "runtime-provided".into(),
                }),
            },
            flows: Vec::new(),
            schemas: SchemaRegistry { names: Vec::new() },
            sorx_binding: SorxBinding {
                transport: "http".into(),
                url: "runtime-provided".into(),
            },
            pack_digest: "sha256:pack".into(),
            sorla_source_digest: "sha256:sorla".into(),
            handoff_digest: "sha256:handoff".into(),
        }
    }

    fn runtime() -> ManagerRuntime {
        ManagerRuntime::new(
            pack(),
            "demo".into(),
            Some("property-ops".into()),
            Some("en".into()),
            None,
            Arc::new(MockSorxClient::default()),
        )
    }

    #[test]
    fn dashboard_card_has_operax_schema() {
        let card = runtime().handle_json("GET", "/v1/operax/manager/cards/dashboard", "");
        assert_eq!(card.status, 200);
        assert_eq!(
            card.body["metadata"]["schema"],
            "greentic.operax.manager-card.v1"
        );
    }

    #[test]
    fn run_endpoint_accepts_batch_json_and_returns_card() {
        let response = runtime().handle_json(
            "POST",
            "/v1/operax/runs",
            r#"{"return_card":true,"input":{"source":"bank","date":"2026-06-02","transactions":[{"transaction_id":"bank_tx_001","booked_at":"2026-06-02","amount":1250.0,"currency":"GBP","reference":"TEN-001 June Rent"}]}}"#,
        );
        assert_eq!(response.status, 200, "{:?}", response.body);
        assert_eq!(response.body["schema"], "greentic.operax.run-result.v1");
        assert_eq!(response.body["report"]["input_count"], 1);
        assert_eq!(
            response.body["card"]["metadata"]["schema"],
            "greentic.operax.manager-card.v1"
        );
    }

    #[test]
    fn manager_submit_rejects_invalid_json() {
        let response = runtime().handle_json("POST", "/v1/operax/manager/submit", "{");
        assert_eq!(response.status, 400);
        assert_eq!(response.body["metadata"]["kind"], "manager.error");
    }

    #[test]
    fn manager_submit_rejects_secret_like_input() {
        let response = runtime().handle_json(
            "POST",
            "/v1/operax/manager/submit",
            r#"{"input":{"transaction_id":"bank_tx_001","booked_at":"2026-06-02","amount":1250.0,"currency":"GBP","reference":"TEN-001","api_key":"secret"}}"#,
        );
        assert_eq!(response.status, 400);
    }
}
