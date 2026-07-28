//! End-to-end integration test: deploys a real tenancy pack purely by
//! `file://` directory reference (no `gtpack_path`) and drives
//! `DeploymentManager::deploy`/`get`/`run` directly (no HTTP server, no NATS
//! broker) to confirm the reference resolves through `fetch_pack_ref` into
//! the handoff directory, the resulting deployment is `Ready`, provenance is
//! recorded on the active version, and the loaded pack actually runs.

use std::sync::Arc;

use operax_manager::deployment::{DeploySpec, DeploymentManager, SorxClientBuilder};
use operax_manager::deployment_store::OperaxDeploymentStore;

// No-op SorxClient stub; dry_run never reaches SoRX, so these bodies are
// unreachable for this test's run (dry_run=true throughout).
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
        "operax-deploy-by-ref-e2e-{}-{}.json",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn deploy_by_file_reference() {
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
    let reference = format!("file://{handoff}");

    let summary = manager
        .deploy(DeploySpec {
            id: "byref".into(),
            gtpack_path: None,
            reference: Some(reference.clone()),
            tenant: "demo".into(),
            team: Some("property-ops".into()),
            locale: None,
            sorx_url: Some("http://127.0.0.1:8099".into()),
            sor: None,
            environment: None,
        })
        .expect("deploy by file:// reference");

    assert!(
        matches!(
            summary.status,
            operax_manager::deployment::DeploymentStatus::Ready
        ),
        "expected deployment to be Ready, got {:?}",
        summary.status
    );

    let detail = manager.get("byref").expect("deployment registered");
    assert_eq!(
        detail.record.active.source_ref.as_deref(),
        Some(reference.as_str()),
        "active version should record the file:// reference as provenance"
    );

    let input: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(format!(
            "{examples}/tenancy/banking/daily-transactions.json"
        ))
        .expect("read tenancy input fixture"),
    )
    .expect("parse tenancy input fixture");

    let result = manager
        .run("byref", input, true)
        .expect("dry-run of the deployed pack");
    assert_eq!(
        result.report.input_count, 3,
        "the pack loaded via file:// reference should process the fixture rows"
    );
}
