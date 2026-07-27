//! End-to-end integration test: spawns the `operax serve` daemon on a real
//! TCP port and drives the discover-mode deployment lifecycle (deploy -> run)
//! over raw HTTP, exercising dynamic SoRX endpoint discovery via an injected
//! `SorxResolver` instead of a static `sorx_url`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use operax_manager::deployment::{DeploymentManager, SorxClientBuilder, SorxResolver};
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
// unreachable for this test's lifecycle (deploy/run-dry-run).
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

// Stub resolver: always resolves to a fixed URL regardless of tenant/sor.
// The StubClient above is never actually invoked under dry_run, so the
// resolved URL just needs to be present for `deploy` to succeed.
struct StubResolver;
impl SorxResolver for StubResolver {
    fn resolve(&self, _tenant: &str, _sor: &str) -> Option<String> {
        Some("http://127.0.0.1:8099".into())
    }
}

// Unique registry path per run so repeated/parallel test invocations never
// collide on the same persisted-registry file.
fn unique_registry_path() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "operax-discovery-e2e-{}-{}.json",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn discover_mode_deploy_and_run() {
    // Build a manager with a stub client + stub resolver, bind a fixed high
    // port distinct from the S1 e2e's, and spawn the server.
    let builder: SorxClientBuilder = Box::new(|_url, _token| {
        Arc::new(StubClient) as Arc<dyn operax_sorx_http::SorxClient + Send + Sync>
    });
    let mgr = Arc::new(
        DeploymentManager::new(
            OperaxDeploymentStore::new(unique_registry_path()),
            None,
            builder,
        )
        .with_resolver(Some(Arc::new(StubResolver))),
    );

    let addr = "127.0.0.1:8141"; // fixed high port for the test; distinct from deployment_server_e2e.rs
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
        r#"{{"id":"disc-e2e","gtpack_path":"{handoff}","tenant":"demo","team":"property-ops","sor":"orders"}}"#
    );
    let (s, b) = request(addr, "POST", "/v1/operax/deployments", &deploy);
    assert_eq!(s, 201, "deploy body: {b}");

    let input = std::fs::read_to_string(format!(
        "{examples}/tenancy/banking/daily-transactions.json"
    ))
    .expect("read tenancy input fixture");
    let run_body = format!(r#"{{"input":{input},"dry_run":true}}"#);
    let (s, b) = request(
        addr,
        "POST",
        "/v1/operax/deployments/disc-e2e/run",
        &run_body,
    );
    assert_eq!(s, 200, "run body: {b}");
    assert!(
        b.contains("\"input_count\":3"),
        "expected input_count 3 in run response: {b}"
    );
}
