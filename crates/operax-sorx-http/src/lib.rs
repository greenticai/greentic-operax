use operax_core::{OperaxContext, OperaxError, ProposedAction, Result, SorxTarget};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SorxHealth {
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SorxRoute {
    pub endpoint_id: String,
    pub operation_id: Option<String>,
    pub method: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SorxBusinessAction {
    pub id: String,
    pub version: String,
    pub contract_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BusinessActionCall {
    pub id: String,
    pub version: String,
    pub contract_hash: String,
    pub values: Value,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GeneratedRouteCall {
    pub method: String,
    pub path: String,
    pub values: Value,
    pub idempotency_key: Option<String>,
}

pub trait SorxClient {
    fn health(&self, ctx: &OperaxContext) -> Result<SorxHealth>;
    fn routes(&self, ctx: &OperaxContext) -> Result<Vec<SorxRoute>>;
    fn business_actions(&self, ctx: &OperaxContext) -> Result<Vec<SorxBusinessAction>>;
    fn dry_run_business_action(
        &self,
        ctx: &OperaxContext,
        action: BusinessActionCall,
    ) -> Result<Value>;
    fn invoke_business_action(
        &self,
        ctx: &OperaxContext,
        action: BusinessActionCall,
    ) -> Result<Value>;
    fn invoke_generated_route(
        &self,
        ctx: &OperaxContext,
        route: GeneratedRouteCall,
    ) -> Result<Value>;
}

pub trait CapabilityClient {
    fn invoke(
        &self,
        capability: &str,
        operation: &str,
        input: Value,
        ctx: &OperaxContext,
        idempotency_key: Option<&str>,
    ) -> Result<Value>;
}

#[derive(Debug, Clone)]
pub struct SorxCapabilityClient<'a, C: SorxClient + ?Sized> {
    client: &'a C,
}

impl<'a, C: SorxClient + ?Sized> SorxCapabilityClient<'a, C> {
    pub fn new(client: &'a C) -> Self {
        Self { client }
    }
}

impl<C: SorxClient + ?Sized> CapabilityClient for SorxCapabilityClient<'_, C> {
    fn invoke(
        &self,
        capability: &str,
        operation: &str,
        input: Value,
        ctx: &OperaxContext,
        idempotency_key: Option<&str>,
    ) -> Result<Value> {
        let call = capability_business_action_call(capability, operation, input, idempotency_key)?;
        self.client.invoke_business_action(ctx, call)
    }
}

#[derive(Debug, Clone)]
pub struct HttpSorxClient {
    base_url: String,
    token: Option<String>,
}

impl HttpSorxClient {
    pub fn new(base_url: impl Into<String>, token: Option<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token,
        }
    }

    fn request(
        &self,
        ctx: &OperaxContext,
        method: &str,
        path: &str,
        body: Option<&Value>,
        idempotency_key: Option<&str>,
    ) -> Result<Value> {
        let parsed = ParsedHttpUrl::parse(&self.base_url)?;
        let body_text = body.map(Value::to_string).unwrap_or_default();
        let mut headers = ctx.sorx_headers(self.token.as_deref());
        if let Some(key) = idempotency_key {
            headers.push(("Idempotency-Key".to_string(), key.to_string()));
        }
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
            parsed.host,
            body_text.len()
        );
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");
        request.push_str(&body_text);

        let mut stream = TcpStream::connect((parsed.host.as_str(), parsed.port))
            .map_err(|err| OperaxError::new("sorx_connect_failed", err.to_string()))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(OperaxError::from)?;
        stream.write_all(request.as_bytes())?;
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        parse_http_json_response(&response)
    }
}

impl SorxClient for HttpSorxClient {
    fn health(&self, ctx: &OperaxContext) -> Result<SorxHealth> {
        let value = self.request(ctx, "GET", "/healthz", None, None)?;
        Ok(SorxHealth {
            ok: value.get("ok").and_then(Value::as_bool).unwrap_or(true),
        })
    }

    fn routes(&self, ctx: &OperaxContext) -> Result<Vec<SorxRoute>> {
        let value = self.request(ctx, "GET", "/v1/sorx/routes", None, None)?;
        let routes = value
            .get("routes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        routes
            .into_iter()
            .map(|route| {
                Ok(SorxRoute {
                    endpoint_id: string_field(&route, "endpoint_id")?,
                    operation_id: route
                        .get("operation_id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    method: string_field(&route, "method")?,
                    path: string_field(&route, "path")?,
                })
            })
            .collect()
    }

    fn business_actions(&self, ctx: &OperaxContext) -> Result<Vec<SorxBusinessAction>> {
        let value = self.request(ctx, "GET", "/v1/sorx/business-actions", None, None)?;
        let actions = value
            .get("actions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        actions
            .into_iter()
            .map(|action| {
                Ok(SorxBusinessAction {
                    id: string_field(&action, "id")?,
                    version: action
                        .get("version")
                        .and_then(Value::as_str)
                        .or_else(|| {
                            action
                                .get("versions")
                                .and_then(Value::as_array)
                                .and_then(|versions| versions.first())
                                .and_then(Value::as_str)
                        })
                        .unwrap_or("0.1.0")
                        .to_string(),
                    contract_hash: action
                        .get("contract_hash")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
            })
            .collect()
    }

    fn dry_run_business_action(
        &self,
        ctx: &OperaxContext,
        action: BusinessActionCall,
    ) -> Result<Value> {
        let path = format!(
            "/v1/sorx/business-actions/{}/versions/{}/dry-run",
            action.id, action.version
        );
        let body = business_action_body(&action);
        self.request(
            ctx,
            "POST",
            &path,
            Some(&body),
            action.idempotency_key.as_deref(),
        )
    }

    fn invoke_business_action(
        &self,
        ctx: &OperaxContext,
        action: BusinessActionCall,
    ) -> Result<Value> {
        let path = format!(
            "/v1/sorx/business-actions/{}/versions/{}/invoke",
            action.id, action.version
        );
        let body = business_action_body(&action);
        self.request(
            ctx,
            "POST",
            &path,
            Some(&body),
            action.idempotency_key.as_deref(),
        )
    }

    fn invoke_generated_route(
        &self,
        ctx: &OperaxContext,
        route: GeneratedRouteCall,
    ) -> Result<Value> {
        self.request(
            ctx,
            &route.method,
            &route.path,
            Some(&route.values),
            route.idempotency_key.as_deref(),
        )
    }
}

#[derive(Debug, Default, Clone)]
pub struct MockSorxClient {
    pub calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl SorxClient for MockSorxClient {
    fn health(&self, _ctx: &OperaxContext) -> Result<SorxHealth> {
        self.calls.lock().unwrap().push("health".into());
        Ok(SorxHealth { ok: true })
    }

    fn routes(&self, _ctx: &OperaxContext) -> Result<Vec<SorxRoute>> {
        self.calls.lock().unwrap().push("routes".into());
        Ok(vec![SorxRoute {
            endpoint_id: "reconciliation_case.create".into(),
            operation_id: Some("reconciliation_case.create".into()),
            method: "POST".into(),
            path: "/v1/agent/reconciliation-cases/create".into(),
        }])
    }

    fn business_actions(&self, _ctx: &OperaxContext) -> Result<Vec<SorxBusinessAction>> {
        self.calls.lock().unwrap().push("business_actions".into());
        Ok(vec![SorxBusinessAction {
            id: "record_rent_payment".into(),
            version: "0.1.0".into(),
            contract_hash: Some(
                "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
            ),
        }])
    }

    fn dry_run_business_action(
        &self,
        _ctx: &OperaxContext,
        action: BusinessActionCall,
    ) -> Result<Value> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("dry_run_business_action:{}", action.id));
        Ok(json!({"valid": true}))
    }

    fn invoke_business_action(
        &self,
        _ctx: &OperaxContext,
        action: BusinessActionCall,
    ) -> Result<Value> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("invoke_business_action:{}", action.id));
        Ok(json!({"ok": true, "action_ref": {"id": action.id}}))
    }

    fn invoke_generated_route(
        &self,
        _ctx: &OperaxContext,
        route: GeneratedRouteCall,
    ) -> Result<Value> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("invoke_generated_route:{}", route.path));
        Ok(json!({"ok": true}))
    }
}

pub fn action_to_business_call(action: &ProposedAction) -> Result<Option<BusinessActionCall>> {
    match &action.sorx_target {
        SorxTarget::BusinessAction {
            id,
            version,
            contract_hash,
        } => Ok(Some(BusinessActionCall {
            id: id.clone(),
            version: version.clone(),
            contract_hash: contract_hash.clone(),
            values: Value::Object(action.values.clone()),
            idempotency_key: action.idempotency_key.clone(),
        })),
        _ => Ok(None),
    }
}

pub fn action_to_generated_route(action: &ProposedAction) -> Result<Option<GeneratedRouteCall>> {
    match &action.sorx_target {
        SorxTarget::GeneratedRoute { method, path, .. } => Ok(Some(GeneratedRouteCall {
            method: method.clone(),
            path: path.clone(),
            values: Value::Object(action.values.clone()),
            idempotency_key: action.idempotency_key.clone(),
        })),
        _ => Ok(None),
    }
}

pub fn invoke_action_capability<C: CapabilityClient + ?Sized>(
    client: &C,
    ctx: &OperaxContext,
    action: &ProposedAction,
    declared_capability: Option<&str>,
) -> Result<Option<Value>> {
    let SorxTarget::BusinessAction {
        id,
        version,
        contract_hash,
    } = &action.sorx_target
    else {
        return Ok(None);
    };
    let fallback_capability = business_action_capability(id, version);
    Ok(Some(client.invoke(
        declared_capability.unwrap_or(&fallback_capability),
        id,
        json!({
            "action_ref": {
                "version": version,
                "contract_hash": contract_hash,
            },
            "values": action.values,
        }),
        ctx,
        action.idempotency_key.as_deref(),
    )?))
}

fn capability_business_action_call(
    capability: &str,
    operation: &str,
    input: Value,
    idempotency_key: Option<&str>,
) -> Result<BusinessActionCall> {
    let version = input
        .pointer("/action_ref/version")
        .and_then(Value::as_str)
        .or_else(|| capability_version(capability))
        .unwrap_or("0.1.0")
        .to_string();
    let contract_hash = input
        .pointer("/action_ref/contract_hash")
        .and_then(Value::as_str)
        .unwrap_or("sha256:0000000000000000000000000000000000000000000000000000000000000000")
        .to_string();
    let values = input.get("values").cloned().unwrap_or(input);
    Ok(BusinessActionCall {
        id: operation.to_string(),
        version,
        contract_hash,
        values,
        idempotency_key: idempotency_key.map(str::to_string),
    })
}

fn business_action_capability(id: &str, version: &str) -> String {
    format!(
        "cap://greentic/sorx/actions/{}/versions/{version}",
        id.replace('_', "-")
    )
}

fn capability_version(capability: &str) -> Option<&str> {
    let mut parts = capability.split('/');
    while let Some(part) = parts.next() {
        if part == "versions" {
            return parts.next();
        }
    }
    None
}

fn business_action_body(action: &BusinessActionCall) -> Value {
    json!({
        "action_ref": {"contract_hash": action.contract_hash},
        "values": action.values,
        "options": {"idempotency_key": action.idempotency_key}
    })
}

fn parse_http_json_response(response: &str) -> Result<Value> {
    let (head, body) = response.split_once("\r\n\r\n").ok_or_else(|| {
        OperaxError::new("invalid_sorx_response", "SORX response had no HTTP body")
    })?;
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(500);
    let value: Value = if body.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(body)?
    };
    if status >= 400 {
        let code = value
            .pointer("/error/code")
            .and_then(Value::as_str)
            .unwrap_or("sorx_http_error");
        return Err(OperaxError::new(code, format!("SORX HTTP status {status}")));
    }
    Ok(value)
}

fn string_field(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| OperaxError::new("invalid_sorx_response", format!("missing {field}")))
}

#[derive(Debug)]
struct ParsedHttpUrl {
    host: String,
    port: u16,
}

impl ParsedHttpUrl {
    fn parse(input: &str) -> Result<Self> {
        let without_scheme = input.strip_prefix("http://").ok_or_else(|| {
            OperaxError::new(
                "unsupported_sorx_url",
                "only http:// SORX URLs are supported",
            )
        })?;
        let host_port = without_scheme.split('/').next().unwrap_or(without_scheme);
        let (host, port) = if let Some((host, port)) = host_port.rsplit_once(':') {
            (host.to_string(), port.parse::<u16>().unwrap_or(80))
        } else {
            (host_port.to_string(), 80)
        };
        if host.is_empty() {
            return Err(OperaxError::new(
                "invalid_sorx_url",
                "SORX URL host is empty",
            ));
        }
        Ok(Self { host, port })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use operax_core::{ActionOperation, ProposedAction};

    #[test]
    fn mock_records_calls() {
        let mock = MockSorxClient::default();
        let ctx = OperaxContext::new("demo".into(), None, None, "sha256:x".into()).unwrap();
        mock.health(&ctx).unwrap();
        mock.routes(&ctx).unwrap();
        assert_eq!(mock.calls.lock().unwrap().len(), 2);
    }

    #[test]
    fn converts_business_action() {
        let action = ProposedAction {
            sorx_target: SorxTarget::BusinessAction {
                id: "record_rent_payment".into(),
                version: "0.1.0".into(),
                contract_hash:
                    "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
            },
            operation: ActionOperation::Invoke,
            values: serde_json::Map::new(),
            idempotency_key: Some("key".into()),
        };
        let call = action_to_business_call(&action).unwrap().unwrap();
        assert_eq!(call.id, "record_rent_payment");
    }

    #[test]
    fn capability_adapter_invokes_business_action() {
        let mock = MockSorxClient::default();
        let capability = SorxCapabilityClient::new(&mock);
        let ctx = OperaxContext::new("demo".into(), None, None, "sha256:x".into()).unwrap();

        capability
            .invoke(
                "cap://greentic/sorx/actions/record-rent-payment/versions/0.2.0",
                "record_rent_payment",
                json!({
                    "action_ref": {
                        "version": "0.2.0",
                        "contract_hash": "sha256:abc",
                    },
                    "values": {"amount": 1250},
                }),
                &ctx,
                Some("bank_tx_001-record_rent_payment"),
            )
            .unwrap();

        assert_eq!(
            mock.calls.lock().unwrap().as_slice(),
            ["invoke_business_action:record_rent_payment"]
        );
    }

    #[test]
    fn capability_call_preserves_action_reference() {
        let call = capability_business_action_call(
            "cap://greentic/sorx/actions/record-rent-payment/versions/0.2.0",
            "record_rent_payment",
            json!({
                "action_ref": {
                    "version": "0.2.0",
                    "contract_hash": "sha256:abc",
                },
                "values": {"amount": 1250},
            }),
            Some("bank_tx_001-record_rent_payment"),
        )
        .unwrap();

        assert_eq!(call.version, "0.2.0");
        assert_eq!(call.contract_hash, "sha256:abc");
        assert_eq!(call.values, json!({"amount": 1250}));
        assert_eq!(
            call.idempotency_key.as_deref(),
            Some("bank_tx_001-record_rent_payment")
        );
    }

    #[test]
    fn action_capability_helper_skips_generated_routes() {
        let mock = MockSorxClient::default();
        let capability = SorxCapabilityClient::new(&mock);
        let ctx = OperaxContext::new("demo".into(), None, None, "sha256:x".into()).unwrap();
        let action = ProposedAction {
            sorx_target: SorxTarget::GeneratedRoute {
                endpoint_id: "case.create".into(),
                operation_id: None,
                method: "POST".into(),
                path: "/v1/cases".into(),
            },
            operation: ActionOperation::Invoke,
            values: serde_json::Map::new(),
            idempotency_key: None,
        };

        let result = invoke_action_capability(&capability, &ctx, &action, None).unwrap();

        assert!(result.is_none());
        assert!(mock.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn parse_http_json_response_ok() {
        let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"ok\":true}";
        let value = parse_http_json_response(response).unwrap();
        assert_eq!(value, json!({"ok": true}));
    }

    #[test]
    fn parse_http_json_response_no_body_separator() {
        let err = parse_http_json_response("HTTP/1.1 200 OK").unwrap_err();
        assert_eq!(err.code, "invalid_sorx_response");
    }

    #[test]
    fn parse_http_json_response_empty_body_returns_empty_object() {
        let response = "HTTP/1.1 200 OK\r\n\r\n   ";
        let value = parse_http_json_response(response).unwrap();
        assert_eq!(value, json!({}));
    }

    #[test]
    fn parse_http_json_response_error_status_with_code() {
        let response = "HTTP/1.1 400 Bad Request\r\n\r\n{\"error\":{\"code\":\"bad_input\"}}";
        let err = parse_http_json_response(response).unwrap_err();
        assert_eq!(err.code, "bad_input");
        assert!(err.message.contains("400"));
    }

    #[test]
    fn parse_http_json_response_error_status_no_code() {
        let response = "HTTP/1.1 500 Internal Server Error\r\n\r\n{\"message\":\"oops\"}";
        let err = parse_http_json_response(response).unwrap_err();
        assert_eq!(err.code, "sorx_http_error");
        assert!(err.message.contains("500"));
    }

    #[test]
    fn parse_http_json_response_missing_status_code_defaults_500() {
        let response = "GARBAGE LINE\r\n\r\n{\"ok\":true}";
        let err = parse_http_json_response(response).unwrap_err();
        assert!(err.message.contains("500"));
    }

    #[test]
    fn parse_http_json_response_status_399_is_ok() {
        let response = "HTTP/1.1 399 Custom\r\n\r\n{\"v\":1}";
        let value = parse_http_json_response(response).unwrap();
        assert_eq!(value, json!({"v": 1}));
    }

    #[test]
    fn string_field_extracts_string() {
        let value = json!({"name": "hello"});
        assert_eq!(string_field(&value, "name").unwrap(), "hello");
    }

    #[test]
    fn string_field_missing_returns_error() {
        let value = json!({"other": "x"});
        let err = string_field(&value, "name").unwrap_err();
        assert_eq!(err.code, "invalid_sorx_response");
        assert!(err.message.contains("name"));
    }

    #[test]
    fn string_field_non_string_returns_error() {
        let value = json!({"count": 42});
        let err = string_field(&value, "count").unwrap_err();
        assert!(err.message.contains("count"));
    }

    #[test]
    fn parsed_http_url_valid() {
        let url = ParsedHttpUrl::parse("http://127.0.0.1:8787").unwrap();
        assert_eq!(url.host, "127.0.0.1");
        assert_eq!(url.port, 8787);
    }

    #[test]
    fn parsed_http_url_no_port_defaults_80() {
        let url = ParsedHttpUrl::parse("http://example.com").unwrap();
        assert_eq!(url.host, "example.com");
        assert_eq!(url.port, 80);
    }

    #[test]
    fn parsed_http_url_with_path() {
        let url = ParsedHttpUrl::parse("http://host:9090/v1/api").unwrap();
        assert_eq!(url.host, "host");
        assert_eq!(url.port, 9090);
    }

    #[test]
    fn parsed_http_url_rejects_https() {
        let err = ParsedHttpUrl::parse("https://host:443").unwrap_err();
        assert_eq!(err.code, "unsupported_sorx_url");
    }

    #[test]
    fn parsed_http_url_rejects_empty_host() {
        let err = ParsedHttpUrl::parse("http://:8787").unwrap_err();
        assert_eq!(err.code, "invalid_sorx_url");
    }

    #[test]
    fn parsed_http_url_invalid_port_defaults_80() {
        let url = ParsedHttpUrl::parse("http://host:notaport").unwrap();
        assert_eq!(url.port, 80);
    }

    #[test]
    fn capability_version_extracts_version() {
        assert_eq!(
            capability_version("cap://greentic/sorx/actions/pay/versions/0.2.0"),
            Some("0.2.0")
        );
    }

    #[test]
    fn capability_version_no_versions_segment() {
        assert_eq!(capability_version("cap://greentic/sorx/actions/pay"), None);
    }

    #[test]
    fn capability_version_versions_at_end() {
        assert_eq!(capability_version("a/versions"), None);
    }

    #[test]
    fn business_action_capability_formats_correctly() {
        assert_eq!(
            business_action_capability("record_rent_payment", "0.1.0"),
            "cap://greentic/sorx/actions/record-rent-payment/versions/0.1.0"
        );
    }

    #[test]
    fn business_action_body_structure() {
        let call = BusinessActionCall {
            id: "pay".into(),
            version: "0.1.0".into(),
            contract_hash: "sha256:abc".into(),
            values: json!({"amount": 100}),
            idempotency_key: Some("key-1".into()),
        };
        let body = business_action_body(&call);
        assert_eq!(body["action_ref"]["contract_hash"], "sha256:abc");
        assert_eq!(body["values"]["amount"], 100);
        assert_eq!(body["options"]["idempotency_key"], "key-1");
    }

    #[test]
    fn business_action_body_null_idempotency_key() {
        let call = BusinessActionCall {
            id: "pay".into(),
            version: "0.1.0".into(),
            contract_hash: "sha256:abc".into(),
            values: json!({}),
            idempotency_key: None,
        };
        let body = business_action_body(&call);
        assert!(body["options"]["idempotency_key"].is_null());
    }

    #[test]
    fn action_to_generated_route_converts() {
        let action = ProposedAction {
            sorx_target: SorxTarget::GeneratedRoute {
                endpoint_id: "case.create".into(),
                operation_id: Some("create_case".into()),
                method: "POST".into(),
                path: "/v1/cases".into(),
            },
            operation: ActionOperation::Invoke,
            values: {
                let mut m = serde_json::Map::new();
                m.insert("name".into(), json!("test"));
                m
            },
            idempotency_key: Some("idem-1".into()),
        };
        let route = action_to_generated_route(&action).unwrap().unwrap();
        assert_eq!(route.method, "POST");
        assert_eq!(route.path, "/v1/cases");
        assert_eq!(route.values["name"], "test");
        assert_eq!(route.idempotency_key.as_deref(), Some("idem-1"));
    }

    #[test]
    fn action_to_generated_route_returns_none_for_business_action() {
        let action = ProposedAction {
            sorx_target: SorxTarget::BusinessAction {
                id: "pay".into(),
                version: "0.1.0".into(),
                contract_hash: "sha256:x".into(),
            },
            operation: ActionOperation::Invoke,
            values: serde_json::Map::new(),
            idempotency_key: None,
        };
        assert!(action_to_generated_route(&action).unwrap().is_none());
    }

    #[test]
    fn action_to_business_call_returns_none_for_generated_route() {
        let action = ProposedAction {
            sorx_target: SorxTarget::GeneratedRoute {
                endpoint_id: "ep".into(),
                operation_id: None,
                method: "GET".into(),
                path: "/v1/x".into(),
            },
            operation: ActionOperation::DryRun,
            values: serde_json::Map::new(),
            idempotency_key: None,
        };
        assert!(action_to_business_call(&action).unwrap().is_none());
    }

    #[test]
    fn invoke_action_capability_invokes_with_declared_capability() {
        let mock = MockSorxClient::default();
        let capability = SorxCapabilityClient::new(&mock);
        let ctx = OperaxContext::new("demo".into(), None, None, "sha256:x".into()).unwrap();
        let action = ProposedAction {
            sorx_target: SorxTarget::BusinessAction {
                id: "record_rent_payment".into(),
                version: "0.1.0".into(),
                contract_hash: "sha256:abc".into(),
            },
            operation: ActionOperation::Invoke,
            values: {
                let mut m = serde_json::Map::new();
                m.insert("amount".into(), json!(500));
                m
            },
            idempotency_key: Some("key-1".into()),
        };
        let result =
            invoke_action_capability(&capability, &ctx, &action, Some("cap://custom/capability"))
                .unwrap();
        assert!(result.is_some());
        assert_eq!(
            mock.calls.lock().unwrap().as_slice(),
            ["invoke_business_action:record_rent_payment"]
        );
    }

    #[test]
    fn invoke_action_capability_uses_fallback_capability() {
        let mock = MockSorxClient::default();
        let capability = SorxCapabilityClient::new(&mock);
        let ctx = OperaxContext::new("demo".into(), None, None, "sha256:x".into()).unwrap();
        let action = ProposedAction {
            sorx_target: SorxTarget::BusinessAction {
                id: "record_rent_payment".into(),
                version: "0.1.0".into(),
                contract_hash: "sha256:abc".into(),
            },
            operation: ActionOperation::Invoke,
            values: serde_json::Map::new(),
            idempotency_key: None,
        };
        let result = invoke_action_capability(&capability, &ctx, &action, None).unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn capability_call_defaults_version_from_capability() {
        let call = capability_business_action_call(
            "cap://greentic/sorx/actions/pay/versions/1.0.0",
            "pay",
            json!({"values": {"x": 1}}),
            None,
        )
        .unwrap();
        assert_eq!(call.version, "1.0.0");
        assert_eq!(call.values, json!({"x": 1}));
        assert!(call.idempotency_key.is_none());
    }

    #[test]
    fn capability_call_defaults_version_to_0_1_0() {
        let call = capability_business_action_call(
            "cap://greentic/sorx/actions/pay",
            "pay",
            json!({"values": {"x": 1}}),
            None,
        )
        .unwrap();
        assert_eq!(call.version, "0.1.0");
    }

    #[test]
    fn capability_call_defaults_contract_hash() {
        let call =
            capability_business_action_call("cap://x", "op", json!({"values": {}}), None).unwrap();
        assert!(call.contract_hash.starts_with("sha256:"));
        assert_eq!(call.contract_hash.len(), 71);
    }

    #[test]
    fn capability_call_uses_input_as_values_when_no_values_key() {
        let call =
            capability_business_action_call("cap://x", "op", json!({"amount": 100}), None).unwrap();
        assert_eq!(call.values, json!({"amount": 100}));
    }

    #[test]
    fn mock_business_actions_returns_one() {
        let mock = MockSorxClient::default();
        let ctx = OperaxContext::new("demo".into(), None, None, "sha256:x".into()).unwrap();
        let actions = mock.business_actions(&ctx).unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].id, "record_rent_payment");
        assert_eq!(actions[0].version, "0.1.0");
        assert!(actions[0].contract_hash.is_some());
    }

    #[test]
    fn mock_dry_run_returns_valid() {
        let mock = MockSorxClient::default();
        let ctx = OperaxContext::new("demo".into(), None, None, "sha256:x".into()).unwrap();
        let call = BusinessActionCall {
            id: "pay".into(),
            version: "0.1.0".into(),
            contract_hash: "sha256:x".into(),
            values: json!({}),
            idempotency_key: None,
        };
        let result = mock.dry_run_business_action(&ctx, call).unwrap();
        assert_eq!(result["valid"], true);
        assert_eq!(
            mock.calls.lock().unwrap().as_slice(),
            ["dry_run_business_action:pay"]
        );
    }

    #[test]
    fn mock_invoke_business_action_returns_ok() {
        let mock = MockSorxClient::default();
        let ctx = OperaxContext::new("demo".into(), None, None, "sha256:x".into()).unwrap();
        let call = BusinessActionCall {
            id: "pay".into(),
            version: "0.1.0".into(),
            contract_hash: "sha256:x".into(),
            values: json!({}),
            idempotency_key: None,
        };
        let result = mock.invoke_business_action(&ctx, call).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["action_ref"]["id"], "pay");
    }

    #[test]
    fn mock_invoke_generated_route_returns_ok() {
        let mock = MockSorxClient::default();
        let ctx = OperaxContext::new("demo".into(), None, None, "sha256:x".into()).unwrap();
        let route = GeneratedRouteCall {
            method: "POST".into(),
            path: "/v1/cases".into(),
            values: json!({}),
            idempotency_key: None,
        };
        let result = mock.invoke_generated_route(&ctx, route).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(
            mock.calls.lock().unwrap().as_slice(),
            ["invoke_generated_route:/v1/cases"]
        );
    }

    #[test]
    fn http_client_new_trims_trailing_slash() {
        let client = HttpSorxClient::new("http://localhost:8080/", None);
        assert_eq!(client.base_url, "http://localhost:8080");
    }

    #[test]
    fn http_client_new_preserves_token() {
        let client = HttpSorxClient::new("http://localhost:8080", Some("secret".into()));
        assert_eq!(client.token.as_deref(), Some("secret"));
    }
}
