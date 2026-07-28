//! HTTP surface for the `operax serve` daemon. `handle_deployment_request` is a
//! pure dispatch function (unit-tested); `start_deployment_server` wraps it in
//! a hand-rolled TCP loop mirroring `start_manager_server` in `lib.rs`, adding
//! shared-secret auth (`is_authorized`) in front of every route except the
//! health checks.

use crate::deployment::{DeployError, DeploySpec, DeploymentManager};
use operax_core::OperaxError;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;

pub struct HttpReply {
    pub status: u16,
    pub body: Value,
}

impl HttpReply {
    fn new(status: u16, body: Value) -> Self {
        Self { status, body }
    }
    fn error(status: u16, code: &str, message: impl Into<String>) -> Self {
        Self::new(
            status,
            json!({ "error": { "code": code, "message": message.into() } }),
        )
    }
}

#[derive(Deserialize)]
struct DeployBody {
    id: String,
    #[serde(default)]
    gtpack_path: Option<PathBuf>,
    #[serde(default)]
    reference: Option<String>,
    tenant: String,
    #[serde(default)]
    team: Option<String>,
    #[serde(default)]
    locale: Option<String>,
    #[serde(default)]
    sorx_url: Option<String>,
    #[serde(default)]
    sor: Option<String>,
    #[serde(default)]
    environment: Option<String>,
}

#[derive(Deserialize)]
struct UpgradeBody {
    gtpack_path: PathBuf,
}

#[derive(Deserialize)]
struct RunBody {
    input: Value,
    #[serde(default)]
    dry_run: bool,
}

fn deploy_error_reply(err: DeployError) -> HttpReply {
    match err {
        DeployError::AlreadyExists => HttpReply::error(
            409,
            "OPERAX_DEPLOYMENT_EXISTS",
            "deployment id already exists",
        ),
        DeployError::NotFound => {
            HttpReply::error(404, "OPERAX_DEPLOYMENT_NOT_FOUND", "deployment not found")
        }
        DeployError::PackLoad(m) => HttpReply::error(422, "OPERAX_PACK_LOAD_FAILED", m),
        DeployError::Fetch(m) => HttpReply::error(502, "OPERAX_PACK_FETCH_FAILED", m),
        DeployError::DeploymentFailed => HttpReply::error(
            409,
            "OPERAX_DEPLOYMENT_FAILED",
            "deployment failed to load its pack",
        ),
        DeployError::Persist(m) => HttpReply::error(500, "OPERAX_INTERNAL", m),
        DeployError::Internal(m) => HttpReply::error(500, "OPERAX_INTERNAL", m),
        DeployError::BadRequest(m) => HttpReply::error(400, "OPERAX_BAD_REQUEST", m),
        DeployError::DiscoveryUnavailable => HttpReply::error(
            422,
            "OPERAX_DISCOVERY_UNAVAILABLE",
            "discovery not available (daemon built without events / no resolver)",
        ),
        DeployError::Unresolved(m) => HttpReply::error(503, "OPERAX_SORX_UNRESOLVED", m),
    }
}

fn parse<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, HttpReply> {
    serde_json::from_slice(body)
        .map_err(|e| HttpReply::error(400, "OPERAX_BAD_REQUEST", e.to_string()))
}

pub fn handle_deployment_request(
    method: &str,
    path: &str,
    body: &[u8],
    mgr: &DeploymentManager,
) -> HttpReply {
    // Health (pre-auth in the server; harmless here).
    if (method, path) == ("GET", "/healthz") || (method, path) == ("GET", "/readyz") {
        return HttpReply::new(200, json!({ "status": "ok" }));
    }

    // Collection routes.
    if path == "/v1/operax/deployments" {
        match method {
            "GET" => {
                return HttpReply::new(200, json!(mgr.list()));
            }
            "POST" => {
                let b: DeployBody = match parse(body) {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                let spec = DeploySpec {
                    id: b.id,
                    gtpack_path: b.gtpack_path,
                    reference: b.reference,
                    tenant: b.tenant,
                    team: b.team,
                    locale: b.locale,
                    sorx_url: b.sorx_url,
                    sor: b.sor,
                    environment: b.environment,
                };
                return match mgr.deploy(spec) {
                    Ok(summary) => HttpReply::new(201, json!(summary)),
                    Err(e) => deploy_error_reply(e),
                };
            }
            _ => return HttpReply::error(405, "OPERAX_BAD_REQUEST", "method not allowed"),
        }
    }

    // Item routes: /v1/operax/deployments/{id}[/run]
    if let Some(rest) = path.strip_prefix("/v1/operax/deployments/") {
        if let Some(id) = rest.strip_suffix("/run") {
            if method != "POST" {
                return HttpReply::error(405, "OPERAX_BAD_REQUEST", "method not allowed");
            }
            let b: RunBody = match parse(body) {
                Ok(b) => b,
                Err(r) => return r,
            };
            return match mgr.run(id, b.input, b.dry_run) {
                Ok(result) => HttpReply::new(200, json!(result)),
                Err(e) => deploy_error_reply(e),
            };
        }
        let id = rest;
        match method {
            "GET" => {
                return match mgr.get(id) {
                    Some(detail) => HttpReply::new(200, json!(detail)),
                    None => deploy_error_reply(DeployError::NotFound),
                };
            }
            "PUT" => {
                let b: UpgradeBody = match parse(body) {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                return match mgr.upgrade(id, b.gtpack_path) {
                    Ok(summary) => HttpReply::new(200, json!(summary)),
                    Err(e) => deploy_error_reply(e),
                };
            }
            "DELETE" => {
                return match mgr.remove(id) {
                    Ok(()) => HttpReply::new(204, Value::Null),
                    Err(e) => deploy_error_reply(e),
                };
            }
            _ => return HttpReply::error(405, "OPERAX_BAD_REQUEST", "method not allowed"),
        }
    }

    HttpReply::error(404, "OPERAX_DEPLOYMENT_NOT_FOUND", "route not found")
}

/// Request headers keyed by lower-cased header name.
pub type HttpHeaders = HashMap<String, String>;

/// Shared-secret auth: accepts `Authorization: Bearer <secret>` or
/// `X-Greentic-SorX-Secret: <secret>`. With no secret configured the daemon
/// is open (local-dev convenience).
pub fn is_authorized(headers: &HttpHeaders, secret: Option<&str>) -> bool {
    let Some(secret) = secret else {
        return true; // no secret configured -> open (local-dev)
    };
    // Collapsed let-chain (clippy::collapsible_if).
    if let Some(bearer) = headers.get("authorization")
        && bearer.strip_prefix("Bearer ").map(str::trim) == Some(secret)
    {
        return true;
    }
    headers.get("x-greentic-sorx-secret").map(String::as_str) == Some(secret)
}

/// Hand-rolled thread-per-connection TCP server for the deployment daemon.
/// Mirrors `start_manager_server`'s structure in `lib.rs`: bind, accept loop,
/// one thread per connection. Every route except `/healthz`/`/readyz` is
/// gated by `is_authorized`.
pub fn start_deployment_server(
    mgr: Arc<DeploymentManager>,
    bind: &str,
    secret: Option<String>,
) -> operax_core::Result<()> {
    let listener = TcpListener::bind(bind)
        .map_err(|err| OperaxError::new("manager_bind_failed", err.to_string()))?;
    println!("OperaX deployment daemon listening on http://{bind}");
    for stream in listener.incoming() {
        let stream = stream.map_err(OperaxError::from)?;
        let mgr = mgr.clone();
        let secret = secret.clone();
        std::thread::spawn(move || {
            let _ = handle_deployment_stream(&mgr, stream, secret.as_deref());
        });
    }
    Ok(())
}

fn handle_deployment_stream(
    mgr: &DeploymentManager,
    mut stream: TcpStream,
    secret: Option<&str>,
) -> operax_core::Result<()> {
    let mut buffer = [0u8; 1024 * 256];
    let read = stream.read(&mut buffer)?;
    let (head, body) = split_request(&buffer[..read]);
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let raw_path = parts.next().unwrap_or("/");
    let path = raw_path.split('?').next().unwrap_or(raw_path);

    if method == "OPTIONS" {
        return write_response(&mut stream, 204, &Value::Null);
    }

    let headers = parse_headers(lines);
    let reply = if matches!(path, "/healthz" | "/readyz") || is_authorized(&headers, secret) {
        handle_deployment_request(method, path, body, mgr)
    } else {
        HttpReply::error(401, "OPERAX_UNAUTHORIZED", "missing or invalid credentials")
    };

    write_response(&mut stream, reply.status, &reply.body)
}

/// Splits a raw request buffer into its head (decoded as lossy UTF-8; headers
/// are expected to be ASCII) and its body, kept as raw bytes so JSON payloads
/// survive intact.
fn split_request(buf: &[u8]) -> (String, &[u8]) {
    match buf.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(pos) => (
            String::from_utf8_lossy(&buf[..pos]).into_owned(),
            &buf[pos + 4..],
        ),
        None => (String::from_utf8_lossy(buf).into_owned(), &[]),
    }
}

fn parse_headers<'a>(lines: impl Iterator<Item = &'a str>) -> HttpHeaders {
    let mut headers = HttpHeaders::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    headers
}

fn write_response(stream: &mut TcpStream, status: u16, body: &Value) -> operax_core::Result<()> {
    if status == 204 {
        write!(
            stream,
            "HTTP/1.1 204 No Content\r\n{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            cors_headers()
        )?;
        return Ok(());
    }
    let body_text = serde_json::to_string(body)?;
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\n{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        status_text(status),
        cors_headers(),
        body_text.len(),
        body_text
    )?;
    Ok(())
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    }
}

/// CORS headers for the deployment daemon. Replicated locally rather than
/// imported from `lib.rs` (whose `cors_headers` is private and scoped to the
/// manager server's method/header set); extended with PUT/DELETE and the
/// SoRX secret header that this API actually uses.
fn cors_headers() -> &'static str {
    "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, PUT, DELETE, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type, Accept, Authorization, X-Greentic-SorX-Secret, X-Greentic-Tenant-Id, X-Greentic-Caller-Id, X-Greentic-Caller-Role, X-Greentic-Team, X-Greentic-Channel, X-Greentic-Locale, Accept-Language"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deployment::{SorxClientBuilder, SorxResolver};
    use crate::deployment_store::OperaxDeploymentStore;
    use std::sync::Arc;

    // Tiny resolver stubs for exercising the `sor`-only deploy path and the
    // 503 `Unresolved` mapping without a real SoRX discovery backend.
    struct SResolver;
    impl SorxResolver for SResolver {
        fn resolve(&self, _env: Option<&str>, _tenant: &str, _sor: &str) -> Option<String> {
            Some("http://localhost:8088".to_string())
        }
    }

    struct SResolverNone;
    impl SorxResolver for SResolverNone {
        fn resolve(&self, _env: Option<&str>, _tenant: &str, _sor: &str) -> Option<String> {
            None
        }
    }

    // Reuse the StubClient pattern from `deployment::tests`; a local copy keeps
    // this module self-contained. deploy/get/list/delete never call SoRX, and
    // the run test below only exercises dry-run against the flow runtime, so
    // these bodies are unreachable.
    struct StubClient;
    impl operax_sorx_http::SorxClient for StubClient {
        fn health(
            &self,
            _ctx: &operax_core::OperaxContext,
        ) -> operax_core::Result<operax_sorx_http::SorxHealth> {
            unimplemented!()
        }

        fn routes(
            &self,
            _ctx: &operax_core::OperaxContext,
        ) -> operax_core::Result<Vec<operax_sorx_http::SorxRoute>> {
            unimplemented!()
        }

        fn business_actions(
            &self,
            _ctx: &operax_core::OperaxContext,
        ) -> operax_core::Result<Vec<operax_sorx_http::SorxBusinessAction>> {
            unimplemented!()
        }

        fn dry_run_business_action(
            &self,
            _ctx: &operax_core::OperaxContext,
            _action: operax_sorx_http::BusinessActionCall,
        ) -> operax_core::Result<serde_json::Value> {
            unimplemented!()
        }

        fn invoke_business_action(
            &self,
            _ctx: &operax_core::OperaxContext,
            _action: operax_sorx_http::BusinessActionCall,
        ) -> operax_core::Result<serde_json::Value> {
            unimplemented!()
        }

        fn invoke_generated_route(
            &self,
            _ctx: &operax_core::OperaxContext,
            _route: operax_sorx_http::GeneratedRouteCall,
        ) -> operax_core::Result<serde_json::Value> {
            unimplemented!()
        }
    }

    // Unique registry path per call — parallel tests must not share a registry
    // file (races the tmp-write+rename in save()). pid alone is not unique.
    fn unique_path() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut p = std::env::temp_dir();
        p.push(format!("operax-serve-{}-{}.json", std::process::id(), n));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn mgr() -> DeploymentManager {
        let builder: SorxClientBuilder = Box::new(|_u, _t| {
            Arc::new(StubClient) as Arc<dyn operax_sorx_http::SorxClient + Send + Sync>
        });
        DeploymentManager::new(OperaxDeploymentStore::new(unique_path()), None, builder)
    }

    fn deploy_body() -> Vec<u8> {
        // The tenancy fixture lives at the REPO ROOT `examples/` (not under any
        // crate) — same path `deployment::tests` uses.
        let handoff = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/tenancy/handoff");
        serde_json::to_vec(&serde_json::json!({
            "id": "d1",
            "gtpack_path": handoff,
            "tenant": "demo",
            "team": "property-ops",
            "sorx_url": "http://localhost:8088"
        }))
        .unwrap()
    }

    #[test]
    fn deploy_then_get_then_delete() {
        let m = mgr();
        let r = handle_deployment_request("POST", "/v1/operax/deployments", &deploy_body(), &m);
        assert_eq!(r.status, 201);

        let r = handle_deployment_request("GET", "/v1/operax/deployments", b"", &m);
        assert_eq!(r.status, 200);
        assert_eq!(r.body.as_array().unwrap().len(), 1);

        let r = handle_deployment_request("GET", "/v1/operax/deployments/d1", b"", &m);
        assert_eq!(r.status, 200);

        let r = handle_deployment_request("DELETE", "/v1/operax/deployments/d1", b"", &m);
        assert_eq!(r.status, 204);

        let r = handle_deployment_request("GET", "/v1/operax/deployments/d1", b"", &m);
        assert_eq!(r.status, 404);
        assert_eq!(r.body["error"]["code"], "OPERAX_DEPLOYMENT_NOT_FOUND");
    }

    // `sor`-only deploy body: valid `gtpack_path` (real, so the pack actually
    // loads and deploy can reach 201) but discovery via `sor` instead of a
    // static `sorx_url`. Mirrors `deploy_body()` above minus `sorx_url`.
    fn deploy_body_sor_only(id: &str) -> Vec<u8> {
        let handoff = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/tenancy/handoff");
        serde_json::to_vec(&serde_json::json!({
            "id": id,
            "gtpack_path": handoff,
            "tenant": "demo",
            "sor": "orders"
        }))
        .unwrap()
    }

    #[test]
    fn deploy_accepts_sor_only() {
        let m = mgr().with_resolver(Some(Arc::new(SResolver)));
        let r = handle_deployment_request(
            "POST",
            "/v1/operax/deployments",
            &deploy_body_sor_only("d-sor"),
            &m,
        );
        assert_eq!(r.status, 201);
    }

    #[test]
    fn run_unresolved_returns_503() {
        let m = mgr().with_resolver(Some(Arc::new(SResolverNone)));
        let r = handle_deployment_request(
            "POST",
            "/v1/operax/deployments",
            &deploy_body_sor_only("d-un"),
            &m,
        );
        assert_eq!(r.status, 201);

        let run = serde_json::to_vec(&serde_json::json!({ "input": [], "dry_run": true })).unwrap();
        let r = handle_deployment_request("POST", "/v1/operax/deployments/d-un/run", &run, &m);
        assert_eq!(r.status, 503);
        assert_eq!(r.body["error"]["code"], "OPERAX_SORX_UNRESOLVED");
    }

    #[test]
    fn duplicate_deploy_conflicts() {
        let m = mgr();
        handle_deployment_request("POST", "/v1/operax/deployments", &deploy_body(), &m);
        let r = handle_deployment_request("POST", "/v1/operax/deployments", &deploy_body(), &m);
        assert_eq!(r.status, 409);
        assert_eq!(r.body["error"]["code"], "OPERAX_DEPLOYMENT_EXISTS");
    }

    #[test]
    fn deploy_with_neither_path_nor_ref_is_400() {
        let m = mgr();
        let body = serde_json::to_vec(&serde_json::json!({"id":"x","tenant":"demo"})).unwrap();
        let r = handle_deployment_request("POST", "/v1/operax/deployments", &body, &m);
        assert_eq!(r.status, 400);
        assert_eq!(r.body["error"]["code"], "OPERAX_BAD_REQUEST");
    }

    #[test]
    fn health_is_ok() {
        let m = mgr();
        assert_eq!(
            handle_deployment_request("GET", "/healthz", b"", &m).status,
            200
        );
    }

    #[test]
    fn auth_requires_matching_secret() {
        // Represent headers as a simple map for the unit test.
        let mut h = std::collections::HashMap::new();
        assert!(!is_authorized(&h, Some("s3cret"))); // no header, secret set → deny
        h.insert("authorization".to_string(), "Bearer s3cret".to_string());
        assert!(is_authorized(&h, Some("s3cret"))); // bearer matches
        h.clear();
        h.insert("x-greentic-sorx-secret".to_string(), "s3cret".to_string());
        assert!(is_authorized(&h, Some("s3cret"))); // header matches
        assert!(is_authorized(&h, None)); // no secret configured → open
    }
}
