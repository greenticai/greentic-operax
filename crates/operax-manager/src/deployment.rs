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
    /// The slot exists but has no runnable runtime (e.g. its pack failed to
    /// load at startup and it is stuck in `DeploymentStatus::Failed`).
    DeploymentFailed,
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

    /// Construct a manager and rebuild slots from the persisted registry.
    /// A record whose pack fails to load becomes a `Failed` slot (kept, not run).
    pub fn load(
        store: OperaxDeploymentStore,
        token: Option<String>,
        client_builder: SorxClientBuilder,
    ) -> Self {
        let mgr = Self::new(store, token, client_builder);
        let registry = match mgr.store.load() {
            Ok(r) => r,
            Err(err) => {
                eprintln!("[operax serve] failed to load registry: {err}; starting empty");
                DeploymentRegistry::default()
            }
        };
        if let Ok(mut slots) = mgr.slots.write() {
            for record in registry.deployments {
                // A `Failed` slot keeps its record with no runnable runtime
                // (`runtime: None`); the daemon never crashes on a bad pack.
                let (runtime, status) = match mgr.build_runtime(&record) {
                    Ok(rt) => (Some(rt), DeploymentStatus::Ready),
                    Err(err) => (
                        None,
                        DeploymentStatus::Failed {
                            error: format!("{err:?}"),
                        },
                    ),
                };
                slots.insert(
                    record.id.clone(),
                    DeploymentSlot {
                        record,
                        runtime,
                        status,
                    },
                );
            }
        }
        mgr
    }

    /// Build a `ManagerRuntime` for a record's active version by loading its pack.
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

    /// Load a new pack version and swap it in as active, keeping the old
    /// version at the front of the (capped) history. The new pack is loaded
    /// BEFORE any mutation, so a bad path leaves the old version active with
    /// no downtime.
    pub fn upgrade(
        &self,
        id: &str,
        gtpack_path: PathBuf,
    ) -> Result<DeploymentSummary, DeployError> {
        let mut slots = self
            .slots
            .write()
            .map_err(|_| DeployError::Internal("lock poisoned".into()))?;
        if !slots.contains_key(id) {
            return Err(DeployError::NotFound);
        }
        // Load the NEW pack first; on failure the old version stays active.
        let pack = operax_pack_loader::load_operational_pack(&gtpack_path)
            .map_err(|e| DeployError::PackLoad(e.to_string()))?;
        let digest = pack.pack_digest.clone();

        let slot = slots.get_mut(id).ok_or(DeployError::NotFound)?;
        let client = (self.client_builder)(&slot.record.sorx_url, self.token.as_deref());
        // ManagerRuntime::new is infallible (returns Self).
        let runtime = ManagerRuntime::new(
            pack,
            slot.record.tenant.clone(),
            slot.record.team.clone(),
            slot.record.locale.clone(),
            None,
            client,
        );

        let next_version = slot.record.active.version + 1;
        let new_active = DeploymentVersion {
            version: next_version,
            gtpack_path,
            pack_digest: digest,
            deployed_at_unix: now_unix(),
        };
        let old_active = std::mem::replace(&mut slot.record.active, new_active);
        slot.record.history.insert(0, old_active);
        slot.record.history.truncate(HISTORY_CAP);
        slot.runtime = Some(Arc::new(runtime));
        slot.status = DeploymentStatus::Ready;

        let summary = DeploymentSummary {
            id: slot.record.id.clone(),
            tenant: slot.record.tenant.clone(),
            active_version: next_version,
            status: DeploymentStatus::Ready,
        };
        self.persist_locked(&slots)?;
        Ok(summary)
    }

    /// Drop a deployment slot entirely. Missing id is `NotFound`.
    pub fn remove(&self, id: &str) -> Result<(), DeployError> {
        let mut slots = self
            .slots
            .write()
            .map_err(|_| DeployError::Internal("lock poisoned".into()))?;
        if slots.remove(id).is_none() {
            return Err(DeployError::NotFound);
        }
        self.persist_locked(&slots)
    }

    /// Delegate a run request to the deployment's `ManagerRuntime`.
    /// `return_card` is always `false`: the daemon returns the run report,
    /// not a manager card.
    pub fn run(
        &self,
        id: &str,
        input: serde_json::Value,
        dry_run: bool,
    ) -> Result<crate::ManagerRunResult, DeployError> {
        let slots = self
            .slots
            .read()
            .map_err(|_| DeployError::Internal("lock poisoned".into()))?;
        let slot = slots.get(id).ok_or(DeployError::NotFound)?;
        let runtime = match (&slot.status, &slot.runtime) {
            (DeploymentStatus::Ready, Some(rt)) => rt.clone(),
            _ => return Err(DeployError::DeploymentFailed),
        };
        // Drop the read lock before running so other deployments proceed.
        drop(slots);
        runtime
            .run_input(input, dry_run, false)
            .map_err(|e| DeployError::Internal(e.to_string()))
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

    fn unique_registry_path() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut p = std::env::temp_dir();
        p.push(format!("operax-mgr-{}-{}.json", std::process::id(), n));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn test_manager() -> DeploymentManager {
        DeploymentManager::new(
            crate::deployment_store::OperaxDeploymentStore::new(unique_registry_path()),
            None,
            Box::new(|_url, _tok| {
                Arc::new(StubClient) as Arc<dyn operax_sorx_http::SorxClient + Send + Sync>
            }),
        )
    }

    // The tenancy fixtures live at the REPO ROOT `examples/` (not under any crate).
    fn repo_examples() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples")
    }

    fn fixture_gtpack() -> PathBuf {
        repo_examples().join("tenancy/handoff")
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

    #[test]
    fn upgrade_bumps_version_and_pushes_history() {
        let mgr = test_manager();
        mgr.deploy(deploy_spec("up")).expect("deploy");
        let summary = mgr.upgrade("up", fixture_gtpack()).expect("upgrade");
        assert_eq!(summary.active_version, 2);
        let detail = mgr.get("up").expect("exists");
        assert_eq!(detail.record.active.version, 2);
        assert_eq!(detail.record.history.len(), 1);
        assert_eq!(detail.record.history[0].version, 1);
    }

    #[test]
    fn upgrade_bad_path_keeps_old_active() {
        let mgr = test_manager();
        mgr.deploy(deploy_spec("keep")).expect("deploy");
        let err = mgr
            .upgrade("keep", std::path::PathBuf::from("/nonexistent/x.gtpack"))
            .unwrap_err();
        assert!(matches!(err, DeployError::PackLoad(_)));
        let detail = mgr.get("keep").expect("still there");
        assert_eq!(detail.record.active.version, 1);
        assert!(matches!(detail.status, DeploymentStatus::Ready));
    }

    #[test]
    fn upgrade_unknown_id_is_not_found() {
        let mgr = test_manager();
        let err = mgr.upgrade("ghost", fixture_gtpack()).unwrap_err();
        assert!(matches!(err, DeployError::NotFound));
    }

    #[test]
    fn history_is_capped() {
        let mgr = test_manager();
        mgr.deploy(deploy_spec("cap")).expect("deploy"); // v1
        for _ in 0..(HISTORY_CAP + 2) {
            mgr.upgrade("cap", fixture_gtpack()).expect("upgrade");
        }
        let detail = mgr.get("cap").expect("exists");
        assert_eq!(detail.record.history.len(), HISTORY_CAP);
        // newest-first: the most recent previous version sits at index 0.
        assert!(detail.record.history[0].version > detail.record.history[1].version);
    }

    #[test]
    fn load_rebuilds_ready_and_marks_failed() {
        // Seed a registry file with one good record and one bad-path record.
        let path = {
            let mut p = std::env::temp_dir();
            p.push(format!("operax-boot-{}.json", std::process::id()));
            p
        };
        let _ = std::fs::remove_file(&path);
        let good = DeploymentRecord {
            id: "good".into(),
            tenant: "demo".into(),
            team: None,
            locale: None,
            sorx_url: "http://x".into(),
            active: DeploymentVersion {
                version: 1,
                gtpack_path: fixture_gtpack(),
                pack_digest: "sha256:g".into(),
                deployed_at_unix: 1,
            },
            history: vec![],
        };
        let bad = DeploymentRecord {
            id: "bad".into(),
            tenant: "demo".into(),
            team: None,
            locale: None,
            sorx_url: "http://x".into(),
            active: DeploymentVersion {
                version: 1,
                gtpack_path: std::path::PathBuf::from("/nonexistent/x.gtpack"),
                pack_digest: "sha256:b".into(),
                deployed_at_unix: 1,
            },
            history: vec![],
        };
        let store = crate::deployment_store::OperaxDeploymentStore::new(path.clone());
        store
            .save(&DeploymentRegistry {
                deployments: vec![good, bad],
            })
            .expect("seed");

        let mgr = DeploymentManager::load(
            crate::deployment_store::OperaxDeploymentStore::new(path.clone()),
            None,
            Box::new(|_u, _t| {
                Arc::new(StubClient) as Arc<dyn operax_sorx_http::SorxClient + Send + Sync>
            }),
        );
        let good_detail = mgr.get("good").expect("good present");
        assert!(matches!(good_detail.status, DeploymentStatus::Ready));
        let bad_detail = mgr.get("bad").expect("bad present");
        assert!(matches!(bad_detail.status, DeploymentStatus::Failed { .. }));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn remove_drops_deployment() {
        let mgr = test_manager();
        mgr.deploy(deploy_spec("gone")).expect("deploy");
        mgr.remove("gone").expect("remove");
        assert!(mgr.get("gone").is_none());
        assert!(matches!(
            mgr.remove("gone").unwrap_err(),
            DeployError::NotFound
        ));
    }

    #[test]
    fn run_dry_run_returns_report() {
        let mgr = test_manager();
        mgr.deploy(deploy_spec("run1")).expect("deploy");
        // Read the real tenancy input at runtime (repo_examples() defined above).
        let input_path = repo_examples().join("tenancy/banking/daily-transactions.json");
        let input: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(input_path).expect("read fixture input"))
                .expect("parse fixture input");
        let result = mgr.run("run1", input, true).expect("run ok");
        // The tenancy fixture yields three decisions (see customer_pilot_demo test).
        assert_eq!(result.report.input_count, 3);
    }

    #[test]
    fn run_unknown_id_is_not_found() {
        let mgr = test_manager();
        let err = mgr.run("ghost", serde_json::json!([]), true).unwrap_err();
        assert!(matches!(err, DeployError::NotFound));
    }
}
