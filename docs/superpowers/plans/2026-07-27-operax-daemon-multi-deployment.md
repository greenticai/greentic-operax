# OperaX Daemon — Multi-Deployment Registry + Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an `operax serve` daemon to `greentic-operax` that holds N operala deployments and exposes deploy/list/get/remove/run/upgrade over HTTP, with a disk-persisted registry.

**Architecture:** Additive layer in `operax-manager` that reuses `ManagerRuntime` wholesale — one `Arc<ManagerRuntime>` per deployment. `DeploymentManager` owns a `RwLock<HashMap<id, DeploymentSlot>>` + a JSON store; a hand-rolled TCP server dispatches HTTP routes to it. `run`/`test`/`events`/`presence` are untouched.

**Tech Stack:** Rust (edition 2024), `serde`/`serde_json`, `std::net::TcpListener` (hand-rolled HTTP/1.1, mirroring the existing `start_manager_server`), reuse of `operax_pack_loader::load_operational_pack` + `ManagerRuntime`.

## Global Constraints

- Base branch: `main`. `greentic-types = 1.1` (registry pin), sibling `greentic-pack` path dep, **no git-deps** — this repo is NOT on the release-train. Do not add git/path deps or bump `greentic-types`.
- `#![forbid(unsafe_code)]` stays. **No `unwrap()`/`panic!()`/`expect()` on daemon runtime paths** — every fallible step returns `Result` mapped to an HTTP status; a poisoned lock maps to `500`, never a panic across the accept loop. (`unwrap`/`expect` are allowed only inside `#[cfg(test)]`.)
- English only in source, tests, comments, commit messages. Conventional Commits (`feat:`, `test:`, `refactor:`). No AI-authorship trailers.
- All new crate-level code lives in `operax-manager` except the CLI subcommand wiring in `operax-cli`. `operax-manager` already depends on `operax-sorx-http`; the SoRX client is still injected via a builder closure so unit tests can pass a stub without constructing a real HTTP client. Error handling uses `operax_core::{Result, OperaxError}` (this crate's convention — NOT anyhow).
- **Validation is via CI build-oracle:** the dev sandbox has no network, so `cargo build`/`test`/`clippy` cannot run locally. Each task's "run test" step is the canonical command; actual green/red is confirmed by pushing the branch and reading `greentic-operax` CI. Batch several tasks per CI push (see Execution Notes at the end).
- **rustfmt DOES run offline.** Before committing each task, run `rustfmt --edition 2024 <each new/modified .rs file>`. CI's `ci/local_check.sh` runs `cargo fmt --check` as its FIRST gate and fails the whole job on any diff. The code blocks in this plan are illustrative and NOT guaranteed rustfmt-clean; always rustfmt after transcribing.
- Deployment identity is a **client-provided stable `id`**. `sorx_url` is **static per deployment** (dynamic discovery is a later slice). A single process-wide SoRX token is sourced once from `--sorx-token-env` (default `SORX_TOKEN`).

---

## File Structure

- `crates/operax-manager/src/deployment.rs` (CREATE) — registry types + `DeploymentManager` (deploy/upgrade/remove/get/list/run, history cap, boot/reload).
- `crates/operax-manager/src/deployment_store.rs` (CREATE) — `OperaxDeploymentStore` (atomic JSON load/save).
- `crates/operax-manager/src/serve.rs` (CREATE) — `handle_deployment_request` (pure dispatch) + `start_deployment_server` (TCP loop + auth).
- `crates/operax-manager/src/lib.rs` (MODIFY) — declare the three new modules.
- `crates/operax-cli/src/lib.rs` (MODIFY) — add `Serve(ServeArgs)` subcommand + handler.
- `crates/operax-manager/tests/deployment_server_e2e.rs` (CREATE) — end-to-end server test.

---

## Task 1: Registry data types

**Files:**
- Create: `crates/operax-manager/src/deployment.rs`
- Modify: `crates/operax-manager/src/lib.rs` (add `pub mod deployment;`)

**Interfaces:**
- Produces: `DeploymentRegistry { deployments: Vec<DeploymentRecord> }`; `DeploymentRecord { id, tenant, team, locale, sorx_url, active: DeploymentVersion, history: Vec<DeploymentVersion> }`; `DeploymentVersion { version: u64, gtpack_path: PathBuf, pack_digest: String, deployed_at_unix: u64 }`; `DeploymentStatus { Ready, Failed { error: String } }`. All serde-derived (records) except `DeploymentStatus` also serde-derived for API output.

- [ ] **Step 1: Write the failing test**

Append to `crates/operax-manager/src/deployment.rs`:

```rust
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
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p operax-manager deployment::tests::registry_serde_roundtrip`
Expected: FAIL — types not defined.

- [ ] **Step 3: Write minimal implementation**

Prepend to `crates/operax-manager/src/deployment.rs`:

```rust
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p operax-manager deployment::tests::registry_serde_roundtrip`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/operax-manager/src/deployment.rs crates/operax-manager/src/lib.rs
git commit -m "feat(operax): deployment registry data types"
```

---

## Task 2: Persistence store (atomic JSON load/save)

**Files:**
- Create: `crates/operax-manager/src/deployment_store.rs`
- Modify: `crates/operax-manager/src/lib.rs` (add `pub mod deployment_store;`)
- Test: same file, `#[cfg(test)]`

**Interfaces:**
- Consumes: `DeploymentRegistry` (Task 1).
- Produces: `OperaxDeploymentStore { path: PathBuf }` with `new(path) -> Self`, `load(&self) -> operax_core::Result<DeploymentRegistry>` (empty registry if the file is absent), `save(&self, &DeploymentRegistry) -> operax_core::Result<()>` (write-tmp-then-rename).
- Error type: this crate uses `operax_core::{Result, OperaxError}` (NOT anyhow — anyhow is not a dependency). `OperaxError::new(code, message)`; `From<std::io::Error>` and `From<serde_json::Error>` are implemented, so `?` works directly on `std::fs`/`serde_json` calls.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::deployment::{DeploymentRecord, DeploymentRegistry, DeploymentVersion};
    use std::path::PathBuf;

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("operax-store-test-{}-{name}.json", std::process::id()));
        p
    }

    #[test]
    fn load_missing_returns_empty() {
        let store = OperaxDeploymentStore::new(tmp_path("missing"));
        let reg = store.load().expect("load");
        assert!(reg.deployments.is_empty());
    }

    #[test]
    fn save_then_load_roundtrip() {
        let path = tmp_path("roundtrip");
        let _ = std::fs::remove_file(&path);
        let store = OperaxDeploymentStore::new(path.clone());
        let reg = DeploymentRegistry {
            deployments: vec![DeploymentRecord {
                id: "d1".into(),
                tenant: "t".into(),
                team: None,
                locale: None,
                sorx_url: "http://x".into(),
                active: DeploymentVersion {
                    version: 2,
                    gtpack_path: PathBuf::from("/tmp/p.gtpack"),
                    pack_digest: "sha256:z".into(),
                    deployed_at_unix: 42,
                },
                history: vec![],
            }],
        };
        store.save(&reg).expect("save");
        let back = store.load().expect("load");
        assert_eq!(back.deployments.len(), 1);
        assert_eq!(back.deployments[0].active.version, 2);
        let _ = std::fs::remove_file(&path);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p operax-manager deployment_store`
Expected: FAIL — `OperaxDeploymentStore` not defined.

- [ ] **Step 3: Write minimal implementation**

Prepend to `crates/operax-manager/src/deployment_store.rs`:

```rust
//! Disk persistence for the deployment registry (single JSON file), mirroring
//! greentic-sorx's `LocalDeploymentRegistryStore`.

use crate::deployment::DeploymentRegistry;
use operax_core::{OperaxError, Result};
use std::path::{Path, PathBuf};

pub struct OperaxDeploymentStore {
    path: PathBuf,
}

impl OperaxDeploymentStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load the registry; an absent file yields an empty registry.
    pub fn load(&self) -> Result<DeploymentRegistry> {
        match std::fs::read(&self.path) {
            // `?` converts serde_json::Error via operax_core's From impl.
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(DeploymentRegistry::default())
            }
            Err(err) => Err(OperaxError::new(
                "registry_read_failed",
                format!("reading deployment registry at {}: {err}", self.path.display()),
            )),
        }
    }

    /// Persist the registry via write-tmp-then-rename for atomicity.
    pub fn save(&self, registry: &DeploymentRegistry) -> Result<()> {
        // Collapsed let-chain (clippy::collapsible_if; let-chains are stable in edition 2024).
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| {
                OperaxError::new(
                    "registry_dir_failed",
                    format!("creating registry dir {}: {e}", parent.display()),
                )
            })?;
        }
        // `?` converts serde_json::Error via operax_core's From impl.
        let bytes = serde_json::to_vec_pretty(registry)?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes).map_err(|e| {
            OperaxError::new("registry_write_failed", format!("writing {}: {e}", tmp.display()))
        })?;
        std::fs::rename(&tmp, &self.path).map_err(|e| {
            OperaxError::new(
                "registry_rename_failed",
                format!("renaming into {}: {e}", self.path.display()),
            )
        })?;
        Ok(())
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p operax-manager deployment_store`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add crates/operax-manager/src/deployment_store.rs crates/operax-manager/src/lib.rs
git commit -m "feat(operax): atomic JSON store for deployment registry"
```

---

## Task 3: DeploymentManager — deploy / get / list

**Files:**
- Modify: `crates/operax-manager/src/deployment.rs` (add `DeploymentSlot`, `DeploymentManager`, deploy/get/list)

**Interfaces:**
- Consumes: `OperaxDeploymentStore` (Task 2); `operax_pack_loader::load_operational_pack`; `ManagerRuntime::new` (infallible — returns `Self`, NOT `Result`); the `SorxClient` trait from **`operax_sorx_http`** (verified: `use operax_sorx_http::SorxClient;`, not `operax_core`). `operax-manager` already depends on `operax-core`, `operax-pack-loader`, `operax-runtime`, and `operax-sorx-http` — no Cargo.toml changes needed.
- Produces:
  - `type SorxClientBuilder = Box<dyn Fn(&str, Option<&str>) -> std::sync::Arc<dyn operax_sorx_http::SorxClient + Send + Sync> + Send + Sync>`
  - `DeploymentManager::new(store: OperaxDeploymentStore, token: Option<String>, client_builder: SorxClientBuilder) -> Self`
  - `DeploymentManager::deploy(&self, spec: DeploySpec) -> Result<DeploymentSummary, DeployError>`
  - `DeploymentManager::get(&self, id: &str) -> Option<DeploymentDetail>`; `list(&self) -> Vec<DeploymentSummary>`
  - `DeploySpec { id, gtpack_path, tenant, team, locale, sorx_url }`
  - `DeploymentSummary { id, tenant, active_version, status: DeploymentStatus }`
  - `DeploymentDetail { record: DeploymentRecord, status: DeploymentStatus }`
  - `enum DeployError { AlreadyExists, PackLoad(String), Persist(String) }`

Note (verified against the crate): `SorxClient` is `operax_sorx_http::SorxClient`; `ManagerRuntime::new(pack, tenant, team, locale, audit_dir, client) -> Self` is **infallible** (no `Result`, no `?`). The stub client in tests must implement all seven trait methods — `health`, `routes`, `business_actions`, `dry_run_business_action`, `invoke_business_action`, `invoke_generated_route`, `invoke` — with `unimplemented!()` bodies (test-only; never called on the dry-run path). Read `crates/operax-sorx-http/src/lib.rs` for their exact signatures before writing the stub.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `deployment.rs`:

```rust
use std::sync::Arc;

// A no-op SorxClient stub; deploy/get/list never call SoRX (only run does, and
// only in non-dry-run), so the stub body is unreachable here.
struct StubClient;
impl operax_sorx_http::SorxClient for StubClient {
    // Fill the trait's required methods with `unimplemented!()` bodies — this is
    // test-only code, so panics are acceptable. Match the real trait signature.
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
        Box::new(|_url, _tok| Arc::new(StubClient) as Arc<dyn operax_sorx_http::SorxClient + Send + Sync>),
    )
}

fn fixture_gtpack() -> std::path::PathBuf {
    // The tenancy handoff dir the existing CLI tests use.
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../operax-cli/examples/tenancy/handoff")
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
    spec.gtpack_path = std::path::PathBuf::from("/nonexistent/x.gtpack");
    let err = mgr.deploy(spec).unwrap_err();
    assert!(matches!(err, DeployError::PackLoad(_)));
    assert!(mgr.get("bad").is_none());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p operax-manager deployment::tests::deploy_`
Expected: FAIL — `DeploymentManager`/`DeploySpec` not defined. (First resolve the `StubClient` trait body against the real `SorxClient` trait — see the note above.)

- [ ] **Step 3: Write minimal implementation**

Add to `deployment.rs` (below the types):

```rust
use crate::deployment_store::OperaxDeploymentStore;
use crate::ManagerRuntime;
use operax_sorx_http::SorxClient;
use std::collections::HashMap;
use std::path::PathBuf;
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
        let mut slots = self.slots.write().map_err(|_| DeployError::Internal("lock poisoned".into()))?;
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
            DeploymentSlot { record, runtime: Some(Arc::new(runtime)), status: DeploymentStatus::Ready },
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
```

No `Cargo.toml` change is needed: `operax-manager` already declares `operax-core`, `operax-pack-loader`, `operax-runtime`, and `operax-sorx-http` as workspace dependencies (verified).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p operax-manager deployment::tests::deploy_`
Expected: PASS (three tests).

- [ ] **Step 5: Commit**

```bash
git add crates/operax-manager/src/deployment.rs
git commit -m "feat(operax): DeploymentManager deploy/get/list"
```

---

## Task 4: DeploymentManager — upgrade / remove / history cap

**Files:**
- Modify: `crates/operax-manager/src/deployment.rs`

**Interfaces:**
- Produces: `DeploymentManager::upgrade(&self, id: &str, gtpack_path: PathBuf) -> Result<DeploymentSummary, DeployError>`; `remove(&self, id: &str) -> Result<(), DeployError>`.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
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
fn remove_drops_deployment() {
    let mgr = test_manager();
    mgr.deploy(deploy_spec("gone")).expect("deploy");
    mgr.remove("gone").expect("remove");
    assert!(mgr.get("gone").is_none());
    assert!(matches!(mgr.remove("gone").unwrap_err(), DeployError::NotFound));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p operax-manager deployment::tests`
Expected: FAIL — `upgrade`/`remove` not defined.

- [ ] **Step 3: Write minimal implementation**

Add to `impl DeploymentManager`:

```rust
    pub fn upgrade(&self, id: &str, gtpack_path: PathBuf) -> Result<DeploymentSummary, DeployError> {
        let mut slots = self.slots.write().map_err(|_| DeployError::Internal("lock poisoned".into()))?;
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

    pub fn remove(&self, id: &str) -> Result<(), DeployError> {
        let mut slots = self.slots.write().map_err(|_| DeployError::Internal("lock poisoned".into()))?;
        if slots.remove(id).is_none() {
            return Err(DeployError::NotFound);
        }
        self.persist_locked(&slots)
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p operax-manager deployment::tests`
Expected: PASS (all).

- [ ] **Step 5: Commit**

```bash
git add crates/operax-manager/src/deployment.rs
git commit -m "feat(operax): DeploymentManager upgrade/remove with capped history"
```

---

## Task 5: Boot/reload from the persisted store

**Files:**
- Modify: `crates/operax-manager/src/deployment.rs`

**Interfaces:**
- Produces: `DeploymentManager::load(store, token, client_builder) -> Self` — constructs the manager AND rebuilds slots from the persisted registry, marking a slot `Failed { error }` when its pack fails to load (never panics/aborts).

- [ ] **Step 1: Write the failing test**

```rust
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
        .save(&DeploymentRegistry { deployments: vec![good, bad] })
        .expect("seed");

    let mgr = DeploymentManager::load(
        crate::deployment_store::OperaxDeploymentStore::new(path.clone()),
        None,
        Box::new(|_u, _t| Arc::new(StubClient) as Arc<dyn operax_sorx_http::SorxClient + Send + Sync>),
    );
    let good_detail = mgr.get("good").expect("good present");
    assert!(matches!(good_detail.status, DeploymentStatus::Ready));
    let bad_detail = mgr.get("bad").expect("bad present");
    assert!(matches!(bad_detail.status, DeploymentStatus::Failed { .. }));
    let _ = std::fs::remove_file(&path);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p operax-manager deployment::tests::load_rebuilds`
Expected: FAIL — `DeploymentManager::load` not defined.

- [ ] **Step 3: Write minimal implementation**

Add to `impl DeploymentManager`:

```rust
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
                    Err(err) => (None, DeploymentStatus::Failed { error: format!("{err:?}") }),
                };
                slots.insert(record.id.clone(), DeploymentSlot { record, runtime, status });
            }
        }
        mgr
    }
```

`build_runtime` (from Task 3) returns `Result<Arc<ManagerRuntime>, DeployError>`; here its `Ok` is wrapped in `Some` and any error becomes a `Failed` slot with `runtime: None`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p operax-manager deployment::tests::load_rebuilds`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/operax-manager/src/deployment.rs
git commit -m "feat(operax): rebuild deployment slots on boot, Failed on bad pack"
```

---

## Task 6: run delegation

**Files:**
- Modify: `crates/operax-manager/src/deployment.rs`

**Interfaces:**
- Consumes: `ManagerRuntime::run_input`. VERIFIED signature: `fn run_input(&self, input: Value, dry_run: bool, return_card: bool) -> operax_core::Result<ManagerRunResult>` — and it is **private**. This task must first change it to `pub fn run_input(...)` (a one-line visibility change; behavior unchanged). There is NO locale-override parameter — locale is fixed at deployment construction, so `run` does not take one.
- Produces: `DeploymentManager::run(&self, id: &str, input: serde_json::Value, dry_run: bool) -> Result<crate::ManagerRunResult, DeployError>`; add `DeployError::DeploymentFailed`. `ManagerRunResult` is already `pub` in `operax-manager` and derives `Serialize` (it wraps `report: RunReport`) — no re-export needed.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn run_dry_run_returns_report() {
    let mgr = test_manager();
    mgr.deploy(deploy_spec("run1")).expect("deploy");
    let input: serde_json::Value = serde_json::from_str(include_str!(
        "../../operax-cli/examples/tenancy/banking/daily-transactions.json"
    ))
    .expect("fixture input");
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p operax-manager deployment::tests::run_`
Expected: FAIL — `run` not defined.

- [ ] **Step 3: Write minimal implementation**

First make `ManagerRuntime::run_input` callable from `deployment.rs`: in `crates/operax-manager/src/lib.rs`, change `fn run_input(` to `pub fn run_input(` (visibility only — do not touch its body or signature).

Add `DeploymentFailed` to `DeployError`, then add to `impl DeploymentManager`:

```rust
    pub fn run(
        &self,
        id: &str,
        input: serde_json::Value,
        dry_run: bool,
    ) -> Result<crate::ManagerRunResult, DeployError> {
        let slots = self.slots.read().map_err(|_| DeployError::Internal("lock poisoned".into()))?;
        let slot = slots.get(id).ok_or(DeployError::NotFound)?;
        let runtime = match (&slot.status, &slot.runtime) {
            (DeploymentStatus::Ready, Some(rt)) => rt.clone(),
            _ => return Err(DeployError::DeploymentFailed),
        };
        // Drop the read lock before running so other deployments proceed.
        drop(slots);
        // return_card = false: the daemon returns the run report, not a manager card.
        runtime
            .run_input(input, dry_run, false)
            .map_err(|e| DeployError::Internal(e.to_string()))
    }
```

`ManagerRunResult` is already `pub` in `operax-manager` (`crate::ManagerRunResult`); it derives `Serialize`, so Task 7 can serialize it directly.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p operax-manager deployment::tests::run_`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/operax-manager/src/deployment.rs crates/operax-manager/src/lib.rs
git commit -m "feat(operax): DeploymentManager run delegates to ManagerRuntime"
```

---

## Task 7: HTTP request dispatch (pure handler)

**Files:**
- Create: `crates/operax-manager/src/serve.rs`
- Modify: `crates/operax-manager/src/lib.rs` (add `pub mod serve;`)

**Interfaces:**
- Consumes: `DeploymentManager` (Tasks 3–6).
- Produces: `struct HttpReply { status: u16, body: serde_json::Value }`; `fn handle_deployment_request(method: &str, path: &str, body: &[u8], mgr: &DeploymentManager) -> HttpReply`. Error bodies are `{ "error": { "code", "message" } }`.

- [ ] **Step 1: Write the failing test**

Add to `serve.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::deployment::{DeploymentManager, SorxClientBuilder};
    use crate::deployment_store::OperaxDeploymentStore;
    use std::sync::Arc;

    // Reuse the StubClient pattern; a local copy keeps this module self-contained.
    struct StubClient;
    impl operax_sorx_http::SorxClient for StubClient { /* unimplemented!() bodies */ }

    fn mgr() -> DeploymentManager {
        let mut p = std::env::temp_dir();
        p.push(format!("operax-serve-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let builder: SorxClientBuilder =
            Box::new(|_u, _t| Arc::new(StubClient) as Arc<dyn operax_sorx_http::SorxClient + Send + Sync>);
        DeploymentManager::new(OperaxDeploymentStore::new(p), None, builder)
    }

    fn deploy_body() -> Vec<u8> {
        let handoff = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../operax-cli/examples/tenancy/handoff");
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
        assert_eq!(handle_deployment_request("GET", "/healthz", b"", &m).status, 200);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p operax-manager serve::tests`
Expected: FAIL — `handle_deployment_request` not defined.

- [ ] **Step 3: Write minimal implementation**

Prepend to `serve.rs`:

```rust
//! HTTP surface for the `operax serve` daemon. `handle_deployment_request` is a
//! pure dispatch function (unit-tested); `start_deployment_server` (Task 8) wraps
//! it in a hand-rolled TCP loop mirroring `start_manager_server`.

use crate::deployment::{DeployError, DeploySpec, DeploymentManager};
use serde::Deserialize;
use serde_json::{json, Value};
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
        Self::new(status, json!({ "error": { "code": code, "message": message.into() } }))
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
        DeployError::AlreadyExists => {
            HttpReply::error(409, "OPERAX_DEPLOYMENT_EXISTS", "deployment id already exists")
        }
        DeployError::NotFound => {
            HttpReply::error(404, "OPERAX_DEPLOYMENT_NOT_FOUND", "deployment not found")
        }
        DeployError::PackLoad(m) => HttpReply::error(422, "OPERAX_PACK_LOAD_FAILED", m),
        DeployError::DeploymentFailed => {
            HttpReply::error(409, "OPERAX_DEPLOYMENT_FAILED", "deployment failed to load its pack")
        }
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
```

`ManagerRunResult`, `DeploymentSummary`, and `DeploymentDetail` must be `Serialize` (Tasks 3/6). `ManagerRunResult` already derives `Serialize` in `operax-manager`; `DeploymentSummary`/`DeploymentDetail` were given `#[derive(Serialize)]` in Task 3.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p operax-manager serve::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/operax-manager/src/serve.rs crates/operax-manager/src/lib.rs
git commit -m "feat(operax): pure HTTP dispatch for deployment daemon"
```

---

## Task 8: TCP server + shared-secret auth

**Files:**
- Modify: `crates/operax-manager/src/serve.rs`

**Interfaces:**
- Produces: `fn is_authorized(headers: &HttpHeaders, secret: Option<&str>) -> bool`; `fn start_deployment_server(mgr: Arc<DeploymentManager>, bind: &str, secret: Option<String>) -> operax_core::Result<()>` (this crate's Result; mirror `start_manager_server`'s signature, which returns `operax_core::Result<()>` and maps bind failure via `OperaxError::new("manager_bind_failed", ...)`).
- Reuse the request-parsing / response-writing helpers from `start_manager_server` in `crates/operax-manager/src/lib.rs`. If they are private module functions, either call them (same crate) or factor the shared bits into `http_util.rs` and use them from both. Do NOT modify `start_manager_server`'s behavior.

- [ ] **Step 1: Write the failing test** (auth unit — the TCP loop is covered by Task 10's e2e)

```rust
#[test]
fn auth_requires_matching_secret() {
    // Represent headers as a simple map for the unit test.
    let mut h = std::collections::HashMap::new();
    assert!(!is_authorized(&h, Some("s3cret"))); // no header, secret set → deny
    h.insert("authorization".to_string(), "Bearer s3cret".to_string());
    assert!(is_authorized(&h, Some("s3cret")));   // bearer matches
    h.clear();
    h.insert("x-greentic-sorx-secret".to_string(), "s3cret".to_string());
    assert!(is_authorized(&h, Some("s3cret")));   // header matches
    assert!(is_authorized(&h, None));             // no secret configured → open
}
```

Use `type HttpHeaders = std::collections::HashMap<String, String>` (lower-cased keys) in `serve.rs`, matching however the existing manager server normalizes header names (verify and reuse its convention).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p operax-manager serve::tests::auth_requires_matching_secret`
Expected: FAIL — `is_authorized` not defined.

- [ ] **Step 3: Write minimal implementation**

Add to `serve.rs`:

```rust
use std::collections::HashMap;
use std::sync::Arc;

pub type HttpHeaders = HashMap<String, String>;

pub fn is_authorized(headers: &HttpHeaders, secret: Option<&str>) -> bool {
    let Some(secret) = secret else {
        return true; // no secret configured → open (local-dev)
    };
    // Collapsed let-chain (clippy::collapsible_if).
    if let Some(bearer) = headers.get("authorization")
        && bearer.strip_prefix("Bearer ").map(str::trim) == Some(secret)
    {
        return true;
    }
    headers.get("x-greentic-sorx-secret").map(String::as_str) == Some(secret)
}
```

Then add `start_deployment_server`, modeled on `start_manager_server` (`lib.rs:355`): `TcpListener::bind(bind)`, `for stream in listener.incoming()`, thread-per-connection, parse `(method, path, headers, body)`, enforce `is_authorized` for every path except `/healthz`/`/readyz` (return `401 OPERAX_UNAUTHORIZED` on failure), call `handle_deployment_request`, then write the `HttpReply` as `HTTP/1.1 {status}` with `Content-Type: application/json`, `Connection: close` (empty body for `204`). Reuse the existing server's byte-limit and CORS handling. Keep it under ~120 lines; extract shared parse/write helpers into `http_util.rs` only if duplication is non-trivial.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p operax-manager serve::tests::auth_requires_matching_secret`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/operax-manager/src/serve.rs crates/operax-manager/src/lib.rs
git commit -m "feat(operax): TCP server + shared-secret auth for deployment daemon"
```

---

## Task 9: CLI `serve` subcommand

**Files:**
- Modify: `crates/operax-cli/src/lib.rs`

**Interfaces:**
- Consumes: `operax_manager::deployment::DeploymentManager`, `operax_manager::deployment_store::OperaxDeploymentStore`, `operax_manager::serve::start_deployment_server`, `operax_sorx_http::HttpSorxClient`.
- Produces: clap `ServeArgs` + a `Serve(ServeArgs)` variant on `Commands` + a `run_serve(args)` handler.

- [ ] **Step 1: Write the failing test** (clap parse, mirroring existing arg tests)

```rust
#[test]
fn serve_args_parse_defaults() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["greentic-operax", "serve"]).expect("parse");
    match cli.command {
        Commands::Serve(args) => {
            assert_eq!(args.bind, "127.0.0.1:8099");
            assert_eq!(args.sorx_token_env, "SORX_TOKEN");
            assert!(args.secret.is_none());
        }
        _ => panic!("expected serve"),
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p operax-cli serve_args_parse_defaults`
Expected: FAIL — `Serve` variant not defined.

- [ ] **Step 3: Write minimal implementation**

Add the variant to `Commands`:

```rust
    /// Run the multi-deployment daemon.
    Serve(ServeArgs),
```

Add the args struct + handler:

```rust
#[derive(clap::Args, Debug)]
pub struct ServeArgs {
    /// Address to bind the HTTP server to.
    #[arg(long, default_value = "127.0.0.1:8099")]
    pub bind: String,
    /// Path to the persisted deployment registry JSON.
    #[arg(long)]
    pub registry: Option<std::path::PathBuf>,
    /// Optional shared secret; when set, all non-health routes require it.
    #[arg(long)]
    pub secret: Option<String>,
    /// Env var holding the SoRX bearer token.
    #[arg(long, default_value = "SORX_TOKEN")]
    pub sorx_token_env: String,
}

fn default_registry_path() -> std::path::PathBuf {
    // ~/.greentic/operax/deployments.json, falling back to CWD-relative if HOME unset.
    let base = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join(".greentic/operax/deployments.json")
}

pub fn run_serve(args: ServeArgs) -> Result<()> {
    use std::sync::Arc;
    let registry_path = args.registry.unwrap_or_else(default_registry_path);
    let token = std::env::var(&args.sorx_token_env).ok();
    let builder: operax_manager::deployment::SorxClientBuilder = Box::new(|url, tok| {
        Arc::new(operax_sorx_http::HttpSorxClient::new(url.to_string(), tok.map(str::to_string)))
            as Arc<dyn operax_sorx_http::SorxClient + Send + Sync>
    });
    let manager = Arc::new(operax_manager::deployment::DeploymentManager::load(
        operax_manager::deployment_store::OperaxDeploymentStore::new(registry_path),
        token,
        builder,
    ));
    eprintln!("[operax serve] listening on {}", args.bind);
    operax_manager::serve::start_deployment_server(manager, &args.bind, args.secret)
}
```

Wire the dispatch arm where the other subcommands are handled (near `lib.rs:233`):

```rust
        Commands::Serve(args) => run_serve(args),
```

`serve` is NOT gated behind the `events` feature (it needs no NATS). Ensure `operax-cli/Cargo.toml` has non-optional deps on `operax-manager`, `operax-sorx-http`, and `operax-core` (they already exist for `test`/`run`).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p operax-cli serve_args_parse_defaults`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/operax-cli/src/lib.rs crates/operax-cli/Cargo.toml
git commit -m "feat(operax): operax serve subcommand wiring"
```

---

## Task 10: End-to-end server integration test

**Files:**
- Create: `crates/operax-manager/tests/deployment_server_e2e.rs`

**Interfaces:**
- Consumes: `start_deployment_server`, the public `DeploymentManager`/`OperaxDeploymentStore` API, and a real `HttpSorxClient` (dev-dependency on `operax-sorx-http`) or the stub — dry-run does not touch SoRX, so a stub suffices.

- [ ] **Step 1: Write the failing test**

```rust
// Spawns the daemon on an ephemeral port and drives the full lifecycle over HTTP.
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

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

#[test]
fn full_lifecycle_over_http() {
    // Build a manager with a stub client, bind an ephemeral port, spawn the server.
    // (Reuse the crate's public API; a stub SorxClient is defined inline.)
    // ... construct manager + Arc, pick 127.0.0.1:0 → resolve actual port ...
    // The exact wiring mirrors Task 8's start_deployment_server; see note.

    let addr = "127.0.0.1:8137"; // fixed high port for the test; adjust if flaky
    // spawn: std::thread::spawn(move || start_deployment_server(mgr, addr, None));
    // give it a moment to bind:
    std::thread::sleep(std::time::Duration::from_millis(200));

    let handoff = concat!(env!("CARGO_MANIFEST_DIR"), "/../operax-cli/examples/tenancy/handoff");
    let deploy = format!(
        r#"{{"id":"e2e","gtpack_path":"{handoff}","tenant":"demo","team":"property-ops","sorx_url":"http://localhost:8088"}}"#
    );
    let (s, _) = request(addr, "POST", "/v1/operax/deployments", &deploy);
    assert_eq!(s, 201);

    let input = include_str!("../../operax-cli/examples/tenancy/banking/daily-transactions.json");
    let run_body = format!(r#"{{"input":{input},"dry_run":true}}"#);
    let (s, b) = request(addr, "POST", "/v1/operax/deployments/e2e/run", &run_body);
    assert_eq!(s, 200, "run body: {b}");

    let (s, _) = request(addr, "PUT", "/v1/operax/deployments/e2e", &format!(r#"{{"gtpack_path":"{handoff}"}}"#));
    assert_eq!(s, 200);

    let (s, _) = request(addr, "DELETE", "/v1/operax/deployments/e2e", "");
    assert_eq!(s, 204);

    let (s, _) = request(addr, "GET", "/v1/operax/deployments/e2e", "");
    assert_eq!(s, 404);
}
```

Note: fill in the manager construction + server spawn at the top (the `// ...` block) using the same stub-client pattern as the unit tests and `start_deployment_server(Arc::new(mgr), addr, None)` on a spawned thread. Use a unique registry temp path per run.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p operax-manager --test deployment_server_e2e`
Expected: FAIL — compile error until the construction block is filled and the server spawns.

- [ ] **Step 3: Complete the test wiring**

Fill the construction block: build `OperaxDeploymentStore` on a temp path, `DeploymentManager::new(store, None, stub_builder)`, wrap in `Arc`, `std::thread::spawn(move || { let _ = start_deployment_server(mgr, addr, None); })`. Define the inline `StubClient` matching `operax_sorx_http::SorxClient`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p operax-manager --test deployment_server_e2e`
Expected: PASS (deploy 201 → run 200 → upgrade 200 → delete 204 → get 404).

- [ ] **Step 5: Commit**

```bash
git add crates/operax-manager/tests/deployment_server_e2e.rs
git commit -m "test(operax): end-to-end deployment daemon lifecycle over HTTP"
```

---

## Task 11: Docs + local-check green

**Files:**
- Modify: `crates/operax-cli/README.md` or the repo `README.md` (document `operax serve`)
- Verify: whole-workspace fmt + clippy + tests

- [ ] **Step 1: Document the subcommand**

Add an `operax serve` section: purpose (multi-deployment daemon), flags (`--bind`, `--registry`, `--secret`, `--sorx-token-env`), the seven routes, and a one-paragraph note that dynamic SoRX discovery (S2) and business-event routing (S3) are follow-on slices.

- [ ] **Step 2: Run the full gate**

Run: `cargo fmt --all -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-targets`
Expected: PASS. Fix any fmt/clippy issues (e.g. `clippy::result_large_err` on `DeployError` — if flagged, box the large variant or `#[allow]` with justification).

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "docs(operax): document operax serve multi-deployment daemon"
```

---

## Self-Review

**Spec coverage:**
- `operax serve` daemon + N deployments → Tasks 3–9. ✓
- deploy/list/get/remove/run/upgrade HTTP → Tasks 7 (dispatch) + 3/4/6 (logic) + 8 (server). ✓
- client-provided stable id → `DeploySpec.id`, Task 3. ✓
- upgrade = versioned replace, retain capped history, zero-downtime on bad pack → Task 4. ✓
- persistence + restart, Failed-on-bad-pack, no crash → Tasks 2 + 5. ✓
- static sorx_url per deployment; process-wide token from `--sorx-token-env` → Tasks 3 + 9. ✓
- shared-secret auth mirroring sorx; tenant is a deployment property not a request header → Task 8 + `DeploySpec`. ✓
- no unwrap/panic on daemon paths; poisoned-lock → 500 → Tasks 3–6 (`Internal`). ✓
- Non-goals (discovery/event-routing/rollback/upload/store-fetch/per-tenant-auth) → not implemented; interface seams noted in the spec. ✓
- Testing incl. e2e mirroring customer_pilot_demo → Task 10. ✓

**Placeholder scan:** `DeploymentSlot.runtime` is `Option<Arc<ManagerRuntime>>` from Task 3 onward; `deploy`/`upgrade` set `Some(..)`, `load` sets `None` for `Failed` slots, and Task 6's `run` matches `(Ready, Some(rt))`. No fabricated placeholders. The only deferred detail is the `SorxClient` trait method bodies in test stubs — the seven method names are in Task 3's note; the implementer copies their signatures from `crates/operax-sorx-http/src/lib.rs` and fills `unimplemented!()` bodies (never called on dry-run). Task 10's e2e leaves the manager-construction block to be filled from the same stub pattern as the unit tests. All other steps carry real code.

**Signatures verified against the crate (post-planning correction):** `ManagerRuntime::new(...) -> Self` is infallible; `run_input(&self, Value, dry_run: bool, return_card: bool) -> operax_core::Result<ManagerRunResult>` (made `pub` in Task 6); `SorxClient` lives in `operax_sorx_http`; error type is `operax_core::{Result, OperaxError}` (no anyhow); `operax-manager` already deps `operax-{core,pack-loader,runtime,sorx-http}`. Tasks 2–9 updated to match.

**Type consistency:** `DeployError` variants (`AlreadyExists`/`NotFound`/`PackLoad`/`DeploymentFailed`/`Persist`/`Internal`) are introduced across Tasks 3–6 and consumed in Task 7's `deploy_error_reply` — all six mapped. `DeploymentSummary`/`DeploymentDetail`/`RunReport` are `Serialize` and serialized in Task 7. `SorxClientBuilder` signature is identical in Tasks 3, 7, 9. `run_input` / `ManagerRuntime::new` / `SorxClient` trait path carry explicit "verify against the crate" notes because their exact signatures were not read line-by-line during planning.

---

## Execution Notes (network-blocked sandbox)

The dev sandbox cannot run `cargo` (no network for the registry/index). Do **not** expect local green. Two workable execution modes:

1. **Inline execution with CI-batch checkpoints (recommended here):** implement Tasks 1–N, push the branch, read `greentic-operax` CI (authoring/build/test jobs) as the oracle, fix, repeat. Batch ~3–4 tasks per push to amortize CI time.
2. **Subagent-driven:** a fresh subagent per task, but since subagents also can't build, each returns code for review and CI validates the batch — the per-task "run test" gate becomes a per-batch CI gate.

Either way, the **verify-before-code** notes (exact `SorxClient` trait body, `ManagerRuntime::new` / `run_input` signatures, header-normalization convention, `RunReport` `Serialize`) must be resolved by reading the actual crate at implementation time — they are the most likely source of first-CI-red.
