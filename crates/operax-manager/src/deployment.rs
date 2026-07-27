//! Multi-deployment registry for the `operax serve` daemon.
//!
//! One `DeploymentSlot` per operala deployment wraps an `Arc<ManagerRuntime>`
//! (the active pack version) plus persisted metadata. `run`/`test`/`events`/
//! `presence` are unaffected.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The whole persisted registry file.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeploymentRegistry {
    pub deployments: Vec<DeploymentRecord>,
}

/// Persisted per-deployment metadata (the runtime is rebuilt from `active.gtpack_path`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentRecord {
    pub id: String,
    pub tenant: String,
    pub team: Option<String>,
    pub locale: Option<String>,
    pub sorx_url: String,
    pub active: DeploymentVersion,
    /// Previous versions, newest-first, capped (see `HISTORY_CAP`).
    #[serde(default)]
    pub history: Vec<DeploymentVersion>,
}

/// One deployed pack version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentVersion {
    pub version: u64,
    pub gtpack_path: PathBuf,
    pub pack_digest: String,
    pub deployed_at_unix: u64,
}

/// In-memory readiness of a slot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DeploymentStatus {
    Ready,
    Failed { error: String },
}

/// Max previous versions retained for a future rollback slice.
pub const HISTORY_CAP: usize = 5;

use crate::ManagerRuntime;
use crate::deployment_store::OperaxDeploymentStore;
use operax_sorx_http::SorxClient;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub type SorxClientBuilder =
    Box<dyn Fn(&str, Option<&str>) -> Arc<dyn SorxClient + Send + Sync> + Send + Sync>;

pub struct DeploymentSlot {
    pub record: DeploymentRecord,
    /// `None` when the slot is `Failed` (its pack could not be loaded); `run`
    /// rejects such slots before ever dereferencing this.
    pub runtime: Option<Arc<ManagerRuntime>>,
    pub status: DeploymentStatus,
}

pub struct DeploymentManager {
    slots: RwLock<HashMap<String, DeploymentSlot>>,
    store: OperaxDeploymentStore,
    token: Option<String>,
    client_builder: SorxClientBuilder,
}

pub struct DeploySpec {
    pub id: String,
    pub gtpack_path: PathBuf,
    pub tenant: String,
    pub team: Option<String>,
    pub locale: Option<String>,
    pub sorx_url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeploymentSummary {
    pub id: String,
    pub tenant: String,
    pub active_version: u64,
    pub status: DeploymentStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeploymentDetail {
    pub record: DeploymentRecord,
    pub status: DeploymentStatus,
}

#[derive(Debug)]
pub enum DeployError {
    AlreadyExists,
    NotFound,
    PackLoad(String),
    Persist(String),
    Internal(String),
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl DeploymentManager {
    pub fn new(
        store: OperaxDeploymentStore,
        token: Option<String>,
        client_builder: SorxClientBuilder,
    ) -> Self {
        Self {
            slots: RwLock::new(HashMap::new()),
            store,
            token,
            client_builder,
        }
    }

    /// Build a `ManagerRuntime` for a record's active version by loading its pack.
    #[allow(dead_code)]
    fn build_runtime(&self, record: &DeploymentRecord) -> Result<Arc<ManagerRuntime>, DeployError> {
        let pack = operax_pack_loader::load_operational_pack(&record.active.gtpack_path)
            .map_err(|e| DeployError::PackLoad(e.to_string()))?;
        let client = (self.client_builder)(&record.sorx_url, self.token.as_deref());
        // ManagerRuntime::new is infallible (returns Self).
        let runtime = ManagerRuntime::new(
            pack,
            record.tenant.clone(),
            record.team.clone(),
            record.locale.clone(),
            None, // audit_dir: none for the daemon in slice 1
            client,
        );
        Ok(Arc::new(runtime))
    }

    fn persist_locked(&self, slots: &HashMap<String, DeploymentSlot>) -> Result<(), DeployError> {
        let registry = DeploymentRegistry {
            deployments: slots.values().map(|s| s.record.clone()).collect(),
        };
        self.store
            .save(&registry)
            .map_err(|e| DeployError::Persist(e.to_string()))
    }

    pub fn deploy(&self, spec: DeploySpec) -> Result<DeploymentSummary, DeployError> {
        let mut slots = self
            .slots
            .write()
            .map_err(|_| DeployError::Internal("lock poisoned".into()))?;
        if slots.contains_key(&spec.id) {
            return Err(DeployError::AlreadyExists);
        }
        // Load the pack first so a bad path fails BEFORE we mutate anything.
        let pack = operax_pack_loader::load_operational_pack(&spec.gtpack_path)
            .map_err(|e| DeployError::PackLoad(e.to_string()))?;
        let digest = pack.pack_digest.clone();
        let client = (self.client_builder)(&spec.sorx_url, self.token.as_deref());
        // ManagerRuntime::new is infallible (returns Self).
        let runtime = ManagerRuntime::new(
            pack,
            spec.tenant.clone(),
            spec.team.clone(),
            spec.locale.clone(),
            None,
            client,
        );
        let record = DeploymentRecord {
            id: spec.id.clone(),
            tenant: spec.tenant,
            team: spec.team,
            locale: spec.locale,
            sorx_url: spec.sorx_url,
            active: DeploymentVersion {
                version: 1,
                gtpack_path: spec.gtpack_path,
                pack_digest: digest,
                deployed_at_unix: now_unix(),
            },
            history: vec![],
        };
        let summary = DeploymentSummary {
            id: record.id.clone(),
            tenant: record.tenant.clone(),
            active_version: 1,
            status: DeploymentStatus::Ready,
        };
        slots.insert(
            record.id.clone(),
            DeploymentSlot {
                record,
                runtime: Some(Arc::new(runtime)),
                status: DeploymentStatus::Ready,
            },
        );
        self.persist_locked(&slots)?;
        Ok(summary)
    }

    pub fn get(&self, id: &str) -> Option<DeploymentDetail> {
        let slots = self.slots.read().ok()?;
        slots.get(id).map(|s| DeploymentDetail {
            record: s.record.clone(),
            status: s.status.clone(),
        })
    }

    pub fn list(&self) -> Vec<DeploymentSummary> {
        let slots = match self.slots.read() {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        slots
            .values()
            .map(|s| DeploymentSummary {
                id: s.record.id.clone(),
                tenant: s.record.tenant.clone(),
                active_version: s.record.active.version,
                status: s.status.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample_version(v: u64) -> DeploymentVersion {
        DeploymentVersion {
            version: v,
            gtpack_path: PathBuf::from("/tmp/x.gtpack"),
            pack_digest: "sha256:abc".to_string(),
            deployed_at_unix: 1_700_000_000,
        }
    }

    #[test]
    fn registry_serde_roundtrip() {
        let reg = DeploymentRegistry {
            deployments: vec![DeploymentRecord {
                id: "rent-recon".to_string(),
                tenant: "demo".to_string(),
                team: Some("property-ops".to_string()),
                locale: None,
                sorx_url: "http://localhost:8088".to_string(),
                active: sample_version(1),
                history: vec![],
            }],
        };
        let json = serde_json::to_string(&reg).expect("serialize");
        let back: DeploymentRegistry = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.deployments.len(), 1);
        assert_eq!(back.deployments[0].id, "rent-recon");
        assert_eq!(back.deployments[0].active.version, 1);
    }

    // A no-op SorxClient stub; deploy/get/list never call SoRX (only run does,
    // and only in non-dry-run), so the stub bodies are unreachable here.
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

    fn test_manager() -> DeploymentManager {
        let path = {
            let mut p = std::env::temp_dir();
            p.push(format!("operax-mgr-{}.json", std::process::id()));
            let _ = std::fs::remove_file(&p);
            p
        };
        DeploymentManager::new(
            crate::deployment_store::OperaxDeploymentStore::new(path),
            None,
            Box::new(|_url, _tok| {
                Arc::new(StubClient) as Arc<dyn operax_sorx_http::SorxClient + Send + Sync>
            }),
        )
    }

    /// Build a minimal valid handoff-directory pack, mirroring the fixture
    /// `write_handoff` helper in `operax-pack-loader`'s own tests. The repo has
    /// no pre-built `.gtpack`/handoff fixture on disk yet, so tests construct
    /// one on the fly; `TempDir::keep` leaks it (test-only) so the directory
    /// outlives this helper and remains readable by `deploy`.
    fn fixture_gtpack() -> PathBuf {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        std::fs::write(
            root.join("operala.yaml"),
            r#"
schema: greentic.operala.handoff.v1
capability: reconciliation
extension: greentic.operala.reconciliation.v1
tenant_required: true
team_optional: true
sorla:
  source_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
sorx:
  transport: http
  url: runtime-provided
"#,
        )
        .expect("write operala.yaml");
        std::fs::write(root.join("operala-handoff.json"), r#"{"ok":true}"#)
            .expect("write operala-handoff.json");
        std::fs::create_dir(root.join("flows")).expect("create flows dir");
        std::fs::write(
            root.join("flows/ingest-transaction.flow.yaml"),
            "name: ingest",
        )
        .expect("write flow");
        std::fs::create_dir(root.join("schemas")).expect("create schemas dir");
        std::fs::write(root.join("schemas/bank-transaction.schema.json"), "{}")
            .expect("write schema");
        temp.keep()
    }

    fn deploy_spec(id: &str) -> DeploySpec {
        DeploySpec {
            id: id.to_string(),
            gtpack_path: fixture_gtpack(),
            tenant: "demo".to_string(),
            team: Some("property-ops".to_string()),
            locale: None,
            sorx_url: "http://localhost:8088".to_string(),
        }
    }

    #[test]
    fn deploy_registers_version_one() {
        let mgr = test_manager();
        let summary = mgr.deploy(deploy_spec("rent-recon")).expect("deploy ok");
        assert_eq!(summary.id, "rent-recon");
        assert_eq!(summary.active_version, 1);
        assert!(matches!(summary.status, DeploymentStatus::Ready));
        assert_eq!(mgr.list().len(), 1);
        assert!(mgr.get("rent-recon").is_some());
    }

    #[test]
    fn deploy_duplicate_id_rejected() {
        let mgr = test_manager();
        mgr.deploy(deploy_spec("dup")).expect("first ok");
        let err = mgr.deploy(deploy_spec("dup")).unwrap_err();
        assert!(matches!(err, DeployError::AlreadyExists));
    }

    #[test]
    fn deploy_bad_path_fails_without_registering() {
        let mgr = test_manager();
        let mut spec = deploy_spec("bad");
        spec.gtpack_path = PathBuf::from("/nonexistent/x.gtpack");
        let err = mgr.deploy(spec).unwrap_err();
        assert!(matches!(err, DeployError::PackLoad(_)));
        assert!(mgr.get("bad").is_none());
    }
}
