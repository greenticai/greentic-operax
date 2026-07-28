//! End-to-end integration test: deploys a real tenancy pack as a static
//! deployment and drives `DeploymentManager::route_event` directly (no HTTP
//! server, no NATS broker) to confirm business-event routing matches the
//! deployed pack's declared `consumes` capability and runs it.

use std::sync::Arc;

use operax_manager::deployment::{DeploySpec, DeploymentManager, SorxClientBuilder};
use operax_manager::deployment_store::OperaxDeploymentStore;

// No-op SorxClient stub; dry_run never reaches SoRX, so these bodies are
// unreachable for this test's routing (dry_run=true throughout).
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
    p.push(format!(
        "operax-event-routing-e2e-{}-{}.json",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn route_event_runs_matching_deployment() {
    let builder: SorxClientBuilder = Box::new(|_url, _token| {
        Arc::new(StubClient) as Arc<dyn operax_sorx_http::SorxClient + Send + Sync>
    });
    let manager = DeploymentManager::new(
        OperaxDeploymentStore::new(unique_registry_path()),
        None,
        builder,
    );

    // Fixtures live at the repo root `examples/`; from this test's manifest dir
    // (`crates/operax-manager`) that is `../../examples`.
    let examples = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");
    let handoff = format!("{examples}/tenancy/handoff");

    manager
        .deploy(DeploySpec {
            id: "recon".into(),
            gtpack_path: Some(handoff.into()),
            reference: None,
            tenant: "demo".into(),
            team: Some("property-ops".into()),
            locale: None,
            sorx_url: Some("http://127.0.0.1:8099".into()),
            sor: None,
            environment: None,
        })
        .expect("deploy tenancy pack");

    let input: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(format!(
            "{examples}/tenancy/banking/daily-transactions.json"
        ))
        .expect("read tenancy input fixture"),
    )
    .expect("parse tenancy input fixture");

    let outcomes = manager.route_event(
        "prod",
        "demo",
        "sorla.tenancy.payment-recorded",
        input,
        true,
    );
    assert_eq!(
        outcomes.len(),
        1,
        "expected exactly one matching deployment"
    );
    assert_eq!(outcomes[0].deployment_id, "recon");
    assert!(
        outcomes[0].result.is_ok(),
        "run should succeed: {:?}",
        outcomes[0].result
    );

    let empty = manager.route_event(
        "prod",
        "demo",
        "sorla.tenancy.nope",
        serde_json::json!({}),
        true,
    );
    assert!(empty.is_empty(), "non-matching topic should route nowhere");
}
