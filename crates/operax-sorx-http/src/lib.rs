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
) -> Result<Option<Value>> {
    let SorxTarget::BusinessAction {
        id,
        version,
        contract_hash,
    } = &action.sorx_target
    else {
        return Ok(None);
    };
    Ok(Some(client.invoke(
        &business_action_capability(id, version),
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

        let result = invoke_action_capability(&capability, &ctx, &action).unwrap();

        assert!(result.is_none());
        assert!(mock.calls.lock().unwrap().is_empty());
    }
}
