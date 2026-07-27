//! HTTP surface for the `operax serve` daemon. `handle_deployment_request` is a
//! pure dispatch function (unit-tested); `start_deployment_server` (Task 8) wraps
//! it in a hand-rolled TCP loop mirroring `start_manager_server`.

use crate::deployment::{DeployError, DeploySpec, DeploymentManager};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

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
    gtpack_path: PathBuf,
    tenant: String,
    #[serde(default)]
    team: Option<String>,
    #[serde(default)]
    locale: Option<String>,
    sorx_url: String,
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
        DeployError::DeploymentFailed => HttpReply::error(
            409,
            "OPERAX_DEPLOYMENT_FAILED",
            "deployment failed to load its pack",
        ),
        DeployError::Persist(m) => HttpReply::error(500, "OPERAX_INTERNAL", m),
        DeployError::Internal(m) => HttpReply::error(500, "OPERAX_INTERNAL", m),
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
                    tenant: b.tenant,
                    team: b.team,
                    locale: b.locale,
                    sorx_url: b.sorx_url,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deployment::SorxClientBuilder;
    use crate::deployment_store::OperaxDeploymentStore;
    use std::sync::Arc;

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

    #[test]
    fn duplicate_deploy_conflicts() {
        let m = mgr();
        handle_deployment_request("POST", "/v1/operax/deployments", &deploy_body(), &m);
        let r = handle_deployment_request("POST", "/v1/operax/deployments", &deploy_body(), &m);
        assert_eq!(r.status, 409);
        assert_eq!(r.body["error"]["code"], "OPERAX_DEPLOYMENT_EXISTS");
    }

    #[test]
    fn health_is_ok() {
        let m = mgr();
        assert_eq!(
            handle_deployment_request("GET", "/healthz", b"", &m).status,
            200
        );
    }
}
