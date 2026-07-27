//! End-to-end integration test: spawns the `operax serve` daemon on a real
//! TCP port and drives the full deployment lifecycle (deploy -> run -> upgrade
//! -> delete -> get) over raw HTTP, exercising the public crate API exactly as
//! the `operax serve` binary would.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use operax_manager::deployment::{DeploymentManager, SorxClientBuilder};
use operax_manager::deployment_store::OperaxDeploymentStore;
use operax_manager::serve::start_deployment_server;

// Minimal HTTP/1.1 request helper (no external HTTP client dep).
fn request(addr: &str, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    stream.read_to_string(&mut resp).expect("read");
    let status = resp
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .expect("status");
    let body = resp.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

// No-op SorxClient stub; dry_run never reaches SoRX, so these bodies are
// unreachable for this test's lifecycle (deploy/run-dry-run/upgrade/delete/get).
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

// Unique registry path per run so repeated/parallel test invocations never
// collide on the same persisted-registry file.
fn unique_registry_path() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!("operax-e2e-{}-{}.json", std::process::id(), n));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn full_lifecycle_over_http() {
    // Build a manager with a stub client, bind a fixed high port, spawn the server.
    let builder: SorxClientBuilder = Box::new(|_url, _token| {
        Arc::new(StubClient) as Arc<dyn operax_sorx_http::SorxClient + Send + Sync>
    });
    let mgr = Arc::new(DeploymentManager::new(
        OperaxDeploymentStore::new(unique_registry_path()),
        None,
        builder,
    ));

    let addr = "127.0.0.1:8137"; // fixed high port for the test; adjust if flaky
    std::thread::spawn(move || {
        let _ = start_deployment_server(mgr, addr, None);
    });
    // give it a moment to bind:
    std::thread::sleep(std::time::Duration::from_millis(200));

    // Fixtures live at the repo root `examples/`; from this test's manifest dir
    // (`crates/operax-manager`) that is `../../examples`.
    let examples = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");
    let handoff = format!("{examples}/tenancy/handoff");
    let deploy = format!(
        r#"{{"id":"e2e","gtpack_path":"{handoff}","tenant":"demo","team":"property-ops","sorx_url":"http://localhost:8088"}}"#
    );
    let (s, _) = request(addr, "POST", "/v1/operax/deployments", &deploy);
    assert_eq!(s, 201);

    let input = std::fs::read_to_string(format!(
        "{examples}/tenancy/banking/daily-transactions.json"
    ))
    .expect("read tenancy input fixture");
    let run_body = format!(r#"{{"input":{input},"dry_run":true}}"#);
    let (s, b) = request(addr, "POST", "/v1/operax/deployments/e2e/run", &run_body);
    assert_eq!(s, 200, "run body: {b}");

    let (s, _) = request(
        addr,
        "PUT",
        "/v1/operax/deployments/e2e",
        &format!(r#"{{"gtpack_path":"{handoff}"}}"#),
    );
    assert_eq!(s, 200);

    let (s, _) = request(addr, "DELETE", "/v1/operax/deployments/e2e", "");
    assert_eq!(s, 204);

    let (s, _) = request(addr, "GET", "/v1/operax/deployments/e2e", "");
    assert_eq!(s, 404);
}
