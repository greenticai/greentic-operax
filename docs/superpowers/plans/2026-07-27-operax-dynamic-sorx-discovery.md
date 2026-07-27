# OperaX Dynamic SoRX Discovery — Implementation Plan (Feature #4, Slice 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Let an `operax serve` deployment resolve its SoRX endpoint dynamically from the presence directory by `(tenant, sor)` — per run — instead of a static `sorx_url`.

**Architecture:** Add a `SorxResolver` trait (operax-manager, NATS-free) consulted per-run in discover mode; the CLI hosts the presence subscriber behind the `events` feature, shares its `Directory` via `Arc<RwLock<_>>`, wraps it in a `PresenceResolver`, and injects it into `DeploymentManager`. `run` in discover mode resolves → builds a fresh client → executes via a new `ManagerRuntime::run_with_client`. Static mode is unchanged from S1.

**Tech Stack:** Rust edition 2024; reuse `run_loaded_pack`, the `SorxClientBuilder` seam, and `presence.rs`.

## Global Constraints

- Base branch `main` (post-S1). `greentic-types 1.1`, no git-deps — NOT on the release-train. Do not add git/path deps or bump greentic-types.
- No `unwrap()`/`panic!()`/`expect()` outside `#[cfg(test)]`; poisoned lock → error, not panic. Error type `operax_core::{Result, OperaxError}` (NO anyhow) in operax-manager. `operax-cli` may use `anyhow` ONLY inside `#[cfg(feature="events")]` code (that's where anyhow already lives).
- Clippy-clean under `-D warnings` (no `collapsible_if` → let-chains; no needless clone). English only; Conventional Commits; no AI-authorship trailers.
- **No local cargo** (no network): validation = push branch → read `greentic-operax` CI. **rustfmt runs offline** — run `rustfmt --edition 2024 <file>` before every commit (CI's first gate is `cargo fmt --check`). The `perf` CI job runs `cargo test --workspace --all-features`, so `#[cfg(feature="events")]` code IS built+tested there; the `ci` job may not enable `events` — do not rely on `ci` alone for events-gated code.
- **Verified signatures (use verbatim):**
  - `run_loaded_pack<C: SorxClient + ?Sized>(pack: &OperationalPack, ctx: &OperaxContext, input_json: Value, dry_run: bool, audit_dir: Option<&Path>, client: &C) -> operax_core::Result<RunReport>` (operax-runtime, `pub`).
  - `ManagerRuntime` fields (private): `pack, tenant, team, locale, audit_dir, client: Arc<dyn SorxClient + Send + Sync>, runs`. `run_input` builds `OperaxContext::new(tenant, team, locale, pack.pack_digest)?` then `run_loaded_pack(&pack, &ctx, input, dry_run, audit_dir.as_deref(), client.as_ref())`, then `push_run`, then `ManagerRunResult{ schema:"greentic.operax.run-result.v1", run_id, report, card }`.
  - presence.rs: `pub type Directory = HashMap<String, DirectoryEntry>`; `DirectoryEntry { presence: SorxPresence, last_seen: u64 }`; `SorxPresence { schema, instance_id, tenant, environment, sor, pack_version, base_url, reachable, offers, ts }` (all `pub`, derives Deserialize+Clone); `pub fn apply_presence(&mut Directory, SorxPresence, u64)`; `pub fn evict_stale(&mut Directory, u64, u64)`; `pub fn run_presence_subscriber(PresenceSubscriberConfig) -> anyhow::Result<()>`; `pub struct PresenceSubscriberConfig { nats_url: String, tenant: Option<String> }`. Module is `#[cfg(feature="events")]`.
  - `DeploymentManager::load(store, token, builder) -> Self` and `::new(store, token, builder) -> Self` — do NOT change their arity (tests depend on it); add the resolver via a chaining `with_resolver`.
  - `SorxClientBuilder = Box<dyn Fn(&str, Option<&str>) -> Arc<dyn operax_sorx_http::SorxClient + Send + Sync> + Send + Sync>`.

---

## File Structure
- `crates/operax-cli/src/presence.rs` (MODIFY) — `resolve_endpoint`; refactor `run_presence_subscriber` to shared `Arc<RwLock<Directory>>`; `PresenceResolver`.
- `crates/operax-cli/src/lib.rs` (MODIFY) — `run_serve` hosts the subscriber (events-gated) + injects the resolver.
- `crates/operax-manager/src/lib.rs` (MODIFY) — `ManagerRuntime::run_with_client`.
- `crates/operax-manager/src/deployment.rs` (MODIFY) — `SorxResolver` trait, `resolver` field + `with_resolver`, `sor`/optional `sorx_url`, discover-mode `run`, new `DeployError` variants + validation.
- `crates/operax-manager/src/serve.rs` (MODIFY) — `DeployBody` `sor` + optional `sorx_url`; error mapping 503/422.
- `crates/operax-manager/tests/discovery_e2e.rs` (CREATE) — discover deploy+run over HTTP with a stub resolver.
- README (MODIFY) — document discover mode.

---

## Task 1: `resolve_endpoint` over the presence directory

**Files:** Modify `crates/operax-cli/src/presence.rs` (+ its `#[cfg(test)]`).

**Interfaces:**
- Produces: `pub fn resolve_endpoint(dir: &Directory, tenant: &str, sor: &str) -> Option<String>` — returns the `base_url` of the freshest (max `last_seen`) entry whose `presence.tenant == tenant && presence.sor == sor && presence.reachable`.

- [ ] **Step 1: failing test** (append to presence.rs `#[cfg(test)]`)
```rust
#[test]
fn resolve_picks_freshest_reachable() {
    fn pres(instance: &str, tenant: &str, sor: &str, url: &str, reachable: bool) -> SorxPresence {
        SorxPresence {
            schema: "greentic.sorx.presence.v1".into(),
            instance_id: instance.into(),
            tenant: tenant.into(),
            environment: "prod".into(),
            sor: sor.into(),
            pack_version: None,
            base_url: url.into(),
            reachable,
            offers: serde_json::Value::Null,
            ts: "t".into(),
        }
    }
    let mut dir = Directory::new();
    apply_presence(&mut dir, pres("i1", "t1", "orders", "http://old", true), 10);
    apply_presence(&mut dir, pres("i2", "t1", "orders", "http://new", true), 20);
    apply_presence(&mut dir, pres("i3", "t1", "orders", "http://down", false), 30);
    apply_presence(&mut dir, pres("i4", "t1", "billing", "http://other", true), 40);
    assert_eq!(resolve_endpoint(&dir, "t1", "orders").as_deref(), Some("http://new"));
    assert_eq!(resolve_endpoint(&dir, "t1", "unknown"), None);
    assert_eq!(resolve_endpoint(&dir, "t2", "orders"), None);
}
```
- [ ] **Step 2: run to verify fail** — `cargo test -p operax-cli --features events presence::tests::resolve_picks_freshest_reachable` → FAIL (undefined). (Note `--features events`: presence is events-gated.)
- [ ] **Step 3: implement** (add to presence.rs)
```rust
/// Resolve the freshest reachable SoRX `base_url` for a (tenant, sor) pair.
pub fn resolve_endpoint(dir: &Directory, tenant: &str, sor: &str) -> Option<String> {
    dir.values()
        .filter(|e| e.presence.reachable && e.presence.tenant == tenant && e.presence.sor == sor)
        .max_by_key(|e| e.last_seen)
        .map(|e| e.presence.base_url.clone())
}
```
- [ ] **Step 4: run to verify pass** — same command → PASS.
- [ ] **Step 5: rustfmt + commit**
```bash
rustfmt --edition 2024 crates/operax-cli/src/presence.rs
git add crates/operax-cli/src/presence.rs
git commit -m "feat(operax): resolve_endpoint over presence directory"
```

---

## Task 2: `ManagerRuntime::run_with_client`

**Files:** Modify `crates/operax-manager/src/lib.rs`.

**Interfaces:**
- Produces: `pub fn run_with_client(&self, input: Value, dry_run: bool, return_card: bool, client: &(dyn SorxClient + Send + Sync)) -> Result<ManagerRunResult>`. `run_input` is refactored to delegate to it with `self.client.as_ref()`.

- [ ] **Step 1: failing test** (in lib.rs `#[cfg(test)]`, or add one if none — match existing test style; if the crate has no unit tests for ManagerRuntime, add a minimal module)
```rust
// Parity: run_with_client with the runtime's own client == run_input.
// (Requires a loadable pack + a SorxClient; reuse the repo-root fixture and a dry-run
// so SoRX is never actually called. If ManagerRuntime construction in a unit test is
// heavy, this parity is instead covered by the deployment.rs discover tests in Task 5 —
// in that case, SKIP adding a lib.rs test here and note it in the report.)
```
Note to implementer: if a direct `ManagerRuntime` unit test is impractical (no existing harness), it is acceptable to rely on Task 5's discover-run test for coverage — state that choice in the report. The refactor's correctness is still gated by CI compiling + the existing `run_input`-based tests passing.
- [ ] **Step 2: run to verify fail** (if a test was added) — `cargo test -p operax-manager run_with_client` → FAIL.
- [ ] **Step 3: implement** — extract the body of `run_input` into `run_with_client`, taking `client` as a parameter:
```rust
pub fn run_with_client(
    &self,
    input: Value,
    dry_run: bool,
    return_card: bool,
    client: &(dyn SorxClient + Send + Sync),
) -> Result<ManagerRunResult> {
    let ctx = OperaxContext::new(
        self.tenant.clone(),
        self.team.clone(),
        self.locale.clone(),
        self.pack.pack_digest.clone(),
    )?;
    let report = run_loaded_pack(
        &self.pack,
        &ctx,
        input,
        dry_run,
        self.audit_dir.as_deref(),
        client,
    )?;
    let run_id = format!("run_{}", ctx.request_id.replace('-', "_"));
    self.push_run(run_id.clone(), report.clone());
    let card = return_card.then(|| self.decision_card_for_report(&run_id, &report));
    Ok(ManagerRunResult {
        schema: "greentic.operax.run-result.v1".to_string(),
        run_id,
        report,
        card,
    })
}

pub fn run_input(&self, input: Value, dry_run: bool, return_card: bool) -> Result<ManagerRunResult> {
    self.run_with_client(input, dry_run, return_card, self.client.as_ref())
}
```
(Match `ManagerRunResult`'s ACTUAL field set from the current file — copy it verbatim; the fields above mirror the current `run_input`.)
- [ ] **Step 4: run to verify pass** — CI (compile + existing run_input tests still green).
- [ ] **Step 5: rustfmt + commit**
```bash
rustfmt --edition 2024 crates/operax-manager/src/lib.rs
git add crates/operax-manager/src/lib.rs
git commit -m "feat(operax): ManagerRuntime::run_with_client; run_input delegates"
```

---

## Task 3: `SorxResolver` trait + `DeploymentManager` resolver field

**Files:** Modify `crates/operax-manager/src/deployment.rs`.

**Interfaces:**
- Produces: `pub trait SorxResolver: Send + Sync { fn resolve(&self, tenant: &str, sor: &str) -> Option<String>; }`; `DeploymentManager` gains `resolver: Option<Arc<dyn SorxResolver>>` (default `None` in `new`/`load`); `pub fn with_resolver(mut self, resolver: Option<Arc<dyn SorxResolver>>) -> Self`.

- [ ] **Step 1: failing test**
```rust
struct StubResolver(Option<String>);
impl SorxResolver for StubResolver {
    fn resolve(&self, _t: &str, _s: &str) -> Option<String> { self.0.clone() }
}

#[test]
fn with_resolver_sets_resolver() {
    let mgr = test_manager().with_resolver(Some(Arc::new(StubResolver(Some("http://x".into())))));
    assert!(mgr.resolver.is_some());
}
```
- [ ] **Step 2: run to verify fail** — `cargo test -p operax-manager with_resolver_sets_resolver` → FAIL.
- [ ] **Step 3: implement**
```rust
pub trait SorxResolver: Send + Sync {
    fn resolve(&self, tenant: &str, sor: &str) -> Option<String>;
}
```
Add field `resolver: Option<Arc<dyn SorxResolver>>` to `DeploymentManager`; initialise to `None` in `new` (and therefore `load`). Add:
```rust
pub fn with_resolver(mut self, resolver: Option<Arc<dyn SorxResolver>>) -> Self {
    self.resolver = resolver;
    self
}
```
- [ ] **Step 4: run to verify pass** — PASS.
- [ ] **Step 5: rustfmt + commit**
```bash
rustfmt --edition 2024 crates/operax-manager/src/deployment.rs
git add crates/operax-manager/src/deployment.rs
git commit -m "feat(operax): SorxResolver trait + DeploymentManager resolver injection"
```

---

## Task 4: Deploy contract — optional `sorx_url`, new `sor`, frozen-client URL, error variants

**Files:** Modify `crates/operax-manager/src/deployment.rs`.

**Interfaces:**
- `DeploySpec` / `DeploymentRecord`: `sorx_url: Option<String>` (was `String`), `sor: Option<String>` (new, `#[serde(default)]` on the record). `DeployError` gains `Unresolved(String)` and `DiscoveryUnavailable`.
- `build_runtime`/`deploy`/`upgrade` build the frozen client from `record.sorx_url.as_deref().unwrap_or("http://unresolved.discover")` (the placeholder is unused in discover mode).

- [ ] **Step 1: failing test** (update existing `deploy_spec` helper to the new shape + add discover cases)
```rust
// deploy_spec now yields Some(sorx_url), sor: None (static, back-compat).
// Add:
#[test]
fn deploy_requires_url_or_sor() {
    let mgr = test_manager();
    let mut spec = deploy_spec("nofields");
    spec.sorx_url = None;
    spec.sor = None;
    assert!(matches!(mgr.deploy(spec).unwrap_err(), DeployError::BadRequest(_)));
}

#[test]
fn deploy_discover_without_resolver_is_unavailable() {
    let mgr = test_manager(); // no resolver
    let mut spec = deploy_spec("disc");
    spec.sorx_url = None;
    spec.sor = Some("orders".into());
    assert!(matches!(mgr.deploy(spec).unwrap_err(), DeployError::DiscoveryUnavailable));
}

#[test]
fn deploy_discover_with_resolver_ok() {
    let mgr = test_manager().with_resolver(Some(Arc::new(StubResolver(Some("http://sorx".into())))));
    let mut spec = deploy_spec("disc2");
    spec.sorx_url = None;
    spec.sor = Some("orders".into());
    let summary = mgr.deploy(spec).expect("discover deploy ok");
    assert_eq!(summary.active_version, 1);
}
```
(Add `DeployError::BadRequest(String)` if not already present — check first; it may be new here.)
- [ ] **Step 2: run to verify fail** — `cargo test -p operax-manager deploy_` → FAIL/compile error (shape change).
- [ ] **Step 3: implement**
  - Change `DeploySpec.sorx_url` / `DeploymentRecord.sorx_url` to `Option<String>`; add `sor: Option<String>` (`#[serde(default)]` on `DeploymentRecord`). Update `DeployBody→DeploySpec` mapping is done in Task 7 (serve.rs); here update the struct + the internal deploy/upgrade/build_runtime.
  - Add `DeployError::{BadRequest(String), Unresolved(String), DiscoveryUnavailable}`.
  - In `deploy` (and `upgrade` where it validates), at the top: `if spec.sorx_url.is_none() && spec.sor.is_none() { return Err(DeployError::BadRequest("deploy requires sorx_url or sor".into())); }` and `if spec.sor.is_some() && self.resolver.is_none() { return Err(DeployError::DiscoveryUnavailable); }`.
  - Frozen client URL at all three build sites: `let frozen_url = record.sorx_url.as_deref().unwrap_or("http://unresolved.discover"); let client = (self.client_builder)(frozen_url, self.token.as_deref());`.
  - `DeploySpec → DeploymentRecord` now carries `sor`.
- [ ] **Step 4: run to verify pass** — PASS (the three new tests + the existing deploy tests updated for the Option shape).
- [ ] **Step 5: rustfmt + commit** — `feat(operax): optional sorx_url + sor discover field + deploy validation`.

---

## Task 5: `DeploymentManager::run` discover mode

**Files:** Modify `crates/operax-manager/src/deployment.rs`.

**Interfaces:**
- `run` unchanged signature `(&self, id, input, dry_run) -> Result<ManagerRunResult, DeployError>`; discover branch resolves + dispatches via `run_with_client`.

- [ ] **Step 1: failing test**
```rust
#[test]
fn run_discover_resolves_and_dispatches() {
    let mgr = test_manager().with_resolver(Some(Arc::new(StubResolver(Some("http://sorx".into())))));
    let mut spec = deploy_spec("run-disc");
    spec.sorx_url = None;
    spec.sor = Some("orders".into());
    mgr.deploy(spec).expect("deploy");
    let input_path = repo_examples().join("tenancy/banking/daily-transactions.json");
    let input: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(input_path).expect("read")).expect("parse");
    let result = mgr.run("run-disc", input, true).expect("run ok");
    assert_eq!(result.report.input_count, 3);
}

#[test]
fn run_discover_unresolved_is_error() {
    let mgr = test_manager().with_resolver(Some(Arc::new(StubResolver(None))));
    let mut spec = deploy_spec("run-unres");
    spec.sorx_url = None;
    spec.sor = Some("orders".into());
    mgr.deploy(spec).expect("deploy");
    let err = mgr.run("run-unres", serde_json::json!([]), true).unwrap_err();
    assert!(matches!(err, DeployError::Unresolved(_)));
}
```
- [ ] **Step 2: run to verify fail** — FAIL.
- [ ] **Step 3: implement** — in `run`, after obtaining the `Arc<ManagerRuntime>` (rt) and dropping the read lock, branch on the record's `sor`. You need `record.sor` + `record.tenant`; read them under the lock into locals before dropping (do not hold the lock across resolution/run):
```rust
// inside run(), while holding the read lock:
let slot = slots.get(id).ok_or(DeployError::NotFound)?;
let rt = match (&slot.status, &slot.runtime) {
    (DeploymentStatus::Ready, Some(rt)) => rt.clone(),
    _ => return Err(DeployError::DeploymentFailed),
};
let discover = slot.record.sor.clone();
let tenant = slot.record.tenant.clone();
drop(slots);

match discover {
    Some(sor) => {
        let resolver = self.resolver.as_ref().ok_or(DeployError::DiscoveryUnavailable)?;
        let url = resolver
            .resolve(&tenant, &sor)
            .ok_or_else(|| DeployError::Unresolved(format!("no reachable SoRX for {tenant}/{sor}")))?;
        let client = (self.client_builder)(&url, self.token.as_deref());
        rt.run_with_client(input, dry_run, false, client.as_ref())
            .map_err(|e| DeployError::Internal(e.to_string()))
    }
    None => rt
        .run_input(input, dry_run, false)
        .map_err(|e| DeployError::Internal(e.to_string())),
}
```
- [ ] **Step 4: run to verify pass** — PASS.
- [ ] **Step 5: rustfmt + commit** — `feat(operax): per-run SoRX discovery in DeploymentManager::run`.

---

## Task 6: HTTP layer — `DeployBody` `sor`, optional `sorx_url`, error mapping

**Files:** Modify `crates/operax-manager/src/serve.rs`.

**Interfaces:**
- `DeployBody`: `sorx_url: Option<String>`, `sor: Option<String>` (`#[serde(default)]` both). `deploy_error_reply` maps `BadRequest→400`, `DiscoveryUnavailable→422 OPERAX_DISCOVERY_UNAVAILABLE`, `Unresolved→503 OPERAX_SORX_UNRESOLVED`.

- [ ] **Step 1: failing test** (add to serve.rs `#[cfg(test)]`, reusing `mgr()` + `unique_path()`; add a stub resolver builder to `mgr()` variants or a new `mgr_with_resolver`)
```rust
#[test]
fn deploy_accepts_sor_only() {
    let m = mgr().with_resolver(Some(Arc::new(SResolver))); // SResolver: returns Some url
    let body = serde_json::to_vec(&serde_json::json!({
        "id": "d-sor", "tenant": "demo", "sor": "orders"
    })).unwrap();
    let r = handle_deployment_request("POST", "/v1/operax/deployments", &body, &m);
    assert_eq!(r.status, 201);
}

#[test]
fn run_unresolved_returns_503() {
    let m = mgr().with_resolver(Some(Arc::new(SResolverNone))); // returns None
    let body = serde_json::to_vec(&serde_json::json!({
        "id": "d-un", "tenant": "demo", "sor": "orders"
    })).unwrap();
    assert_eq!(handle_deployment_request("POST", "/v1/operax/deployments", &body, &m).status, 201);
    let run = serde_json::to_vec(&serde_json::json!({"input": [], "dry_run": true})).unwrap();
    let r = handle_deployment_request("POST", "/v1/operax/deployments/d-un/run", &run, &m);
    assert_eq!(r.status, 503);
    assert_eq!(r.body["error"]["code"], "OPERAX_SORX_UNRESOLVED");
}
```
(Define `SResolver`/`SResolverNone` as tiny `SorxResolver` impls in the test module; `mgr()` currently returns a `DeploymentManager` — chain `.with_resolver(...)`.)
- [ ] **Step 2: run to verify fail** — FAIL.
- [ ] **Step 3: implement** — make `DeployBody.sorx_url: Option<String>` + add `sor: Option<String>` (`#[serde(default)]`), pass both into `DeploySpec`. Extend `deploy_error_reply`:
```rust
DeployError::BadRequest(m) => HttpReply::error(400, "OPERAX_BAD_REQUEST", m),
DeployError::DiscoveryUnavailable => HttpReply::error(422, "OPERAX_DISCOVERY_UNAVAILABLE", "discovery not available (daemon built without events / no resolver)"),
DeployError::Unresolved(m) => HttpReply::error(503, "OPERAX_SORX_UNRESOLVED", m),
```
- [ ] **Step 4: run to verify pass** — PASS.
- [ ] **Step 5: rustfmt + commit** — `feat(operax): HTTP deploy accepts sor; 503/422 discovery errors`.

---

## Task 7: presence.rs — shared-directory subscriber

**Files:** Modify `crates/operax-cli/src/presence.rs`.

**Interfaces:**
- `run_presence_subscriber` refactored to `pub fn run_presence_subscriber(config: PresenceSubscriberConfig, directory: std::sync::Arc<std::sync::RwLock<Directory>>) -> anyhow::Result<()>` — applies each decoded presence into the SHARED directory (write-lock), evicts stale, optionally logs. The existing `presence subscribe` CLI path constructs a fresh `Arc<RwLock<Directory>>` and passes it.

- [ ] **Step 1: failing test**
```rust
#[test]
fn apply_into_shared_directory() {
    use std::sync::{Arc, RwLock};
    let dir = Arc::new(RwLock::new(Directory::new()));
    { // simulate what the loop does per message
        let mut g = dir.write().unwrap();
        apply_presence(&mut g, /* build a SorxPresence via the pres() helper */ pres_orders(), 10);
    }
    let g = dir.read().unwrap();
    assert_eq!(resolve_endpoint(&g, "t1", "orders").as_deref(), Some("http://new"));
}
```
(Provide a small `pres_orders()` helper or reuse the `pres` closure from Task 1's test; keep it consistent.)
- [ ] **Step 2: run to verify fail** — `cargo test -p operax-cli --features events presence::tests::apply_into_shared_directory` → FAIL until the shared type is threaded (this specific test mostly checks the lock pattern compiles + resolve works on shared state).
- [ ] **Step 3: implement** — change `run_presence_subscriber` to take `directory: Arc<RwLock<Directory>>`; inside the async loop, replace the local `let mut directory = Directory::new()` with writes through the shared lock: on each message `if let Ok(p) = decode_presence(&payload) { let now = now_ticks(); let mut g = directory.write().map_err(|_| anyhow::anyhow!("presence dir poisoned"))?; apply_presence(&mut g, p, now); evict_stale(&mut g, now, PRESENCE_TTL_SECS); }` then optionally `log_directory(&g)` (drop the guard before logging or log under it). Update the caller `run_presence_subscribe` (the `presence subscribe` command handler) to build `Arc::new(RwLock::new(Directory::new()))` and pass it.
- [ ] **Step 4: run to verify pass** — PASS.
- [ ] **Step 5: rustfmt + commit** — `refactor(operax): presence subscriber writes into a shared directory`.

---

## Task 8: `PresenceResolver` + host in `run_serve`

**Files:** Modify `crates/operax-cli/src/presence.rs` (PresenceResolver) and `crates/operax-cli/src/lib.rs` (`run_serve`).

**Interfaces:**
- `presence.rs`: `pub struct PresenceResolver { directory: Arc<RwLock<Directory>> }` implementing `operax_manager::deployment::SorxResolver` via `resolve_endpoint`.
- `lib.rs run_serve`: when built with `events`, construct the shared directory, spawn the subscriber on a dedicated thread, build a `PresenceResolver`, and `.with_resolver(Some(Arc::new(resolver)))` on the manager. Without `events`, inject `None`.

- [ ] **Step 1: failing test** (presence.rs)
```rust
#[test]
fn presence_resolver_delegates() {
    use std::sync::{Arc, RwLock};
    let dir = Arc::new(RwLock::new(Directory::new()));
    { let mut g = dir.write().unwrap(); apply_presence(&mut g, pres_orders(), 10); }
    let r = PresenceResolver { directory: dir };
    assert_eq!(operax_manager::deployment::SorxResolver::resolve(&r, "t1", "orders").as_deref(), Some("http://new"));
}
```
- [ ] **Step 2: run to verify fail** — `--features events` → FAIL.
- [ ] **Step 3: implement**
```rust
// presence.rs
use operax_manager::deployment::SorxResolver;
pub struct PresenceResolver {
    pub directory: std::sync::Arc<std::sync::RwLock<Directory>>,
}
impl SorxResolver for PresenceResolver {
    fn resolve(&self, tenant: &str, sor: &str) -> Option<String> {
        let dir = self.directory.read().ok()?;
        resolve_endpoint(&dir, tenant, sor)
    }
}
```
Then in `run_serve` (lib.rs), gate the wiring:
```rust
#[cfg(feature = "events")]
let manager = {
    use std::sync::{Arc, RwLock};
    let directory = Arc::new(RwLock::new(crate::presence::Directory::new()));
    // spawn subscriber on its own thread (it owns a current-thread tokio runtime)
    let nats_url = std::env::var("OPERAX_PRESENCE_NATS_URL").ok();
    if let Some(nats_url) = nats_url {
        let dir_for_sub = directory.clone();
        std::thread::spawn(move || {
            let cfg = crate::presence::PresenceSubscriberConfig { nats_url, tenant: None };
            if let Err(e) = crate::presence::run_presence_subscriber(cfg, dir_for_sub) {
                eprintln!("[operax serve] presence subscriber ended: {e}");
            }
        });
    } else {
        eprintln!("[operax serve] OPERAX_PRESENCE_NATS_URL unset; discovery inert");
    }
    let resolver = Arc::new(crate::presence::PresenceResolver { directory });
    manager.with_resolver(Some(resolver))
};
```
Restructure `run_serve` so `manager` is built (via `DeploymentManager::load(...)`) BEFORE this block, then reassigned; on `#[cfg(not(feature = "events"))]` leave `manager` as-is (resolver stays `None`). Wrap the final `manager` in `Arc` AFTER resolver injection (adjust the existing `Arc::new(...load...)` to inject first, then `Arc::new`). `PresenceSubscriberConfig`'s exact field names/visibility must be `pub` — make them `pub` in Task 7 if not already.
- [ ] **Step 4: run to verify pass** — PASS (events build).
- [ ] **Step 5: rustfmt + commit** — `feat(operax): host presence subscriber in operax serve, inject PresenceResolver`.

---

## Task 9: discovery integration test (HTTP, stub resolver)

**Files:** Create `crates/operax-manager/tests/discovery_e2e.rs`.

**Interfaces:** consumes the public `DeploymentManager` (+ `with_resolver`), `SorxResolver`, `start_deployment_server`. Uses an inline stub resolver + stub client; no NATS.

- [ ] **Step 1: failing test** — spawn the daemon (reuse S1's `deployment_server_e2e.rs` request() helper + StubClient + unique registry; add an inline `struct Res; impl SorxResolver for Res { fn resolve(..) -> Some(handoff-independent URL) }`), inject via `.with_resolver`. Flow: `POST /v1/operax/deployments {id, tenant, sor:"orders"}` → 201 → `POST .../{id}/run {input: <tenancy daily>, dry_run:true}` → 200 with `report.input_count == 3`. Bind a fresh high port; unique temp registry. (Full wiring mirrors S1's e2e — copy its scaffolding.)
- [ ] **Step 2: run to verify fail** — `cargo test -p operax-manager --test discovery_e2e` → FAIL until written.
- [ ] **Step 3: complete the wiring** as above.
- [ ] **Step 4: run to verify pass** — PASS.
- [ ] **Step 5: rustfmt + commit** — `test(operax): discover-mode deploy+run e2e over HTTP`.

---

## Task 10: docs

**Files:** Modify `crates/operax-cli/README.md` (the S1 `operax serve` section).

- [ ] **Step 1:** Under `operax serve`, document **discover mode**: deploy body may supply `sor` (instead of/alongside `sorx_url`); the daemon resolves the SoRX endpoint per run from the presence directory; requires the daemon built with the `events` feature and `OPERAX_PRESENCE_NATS_URL` set; errors: `422 OPERAX_DISCOVERY_UNAVAILABLE` (no resolver), `503 OPERAX_SORX_UNRESOLVED` (no reachable SoRX). Note environment discrimination + health-probing are follow-ups.
- [ ] **Step 2: commit** — `docs(operax): document discover mode for operax serve`.

---

## Self-Review

**Spec coverage:** resolver trait+inject (T3), shared directory+resolve (T1,T7), host subscriber (T8), run_with_client (T2), per-run discover run (T5), deploy contract sor/optional-url + validation (T4,T6), errors 400/422/503 (T4,T6), e2e (T9), docs (T10). ✓

**Placeholder scan:** Task 2's unit test is conditionally deferred to Task 5 coverage (explicitly flagged, not a silent gap) because a standalone `ManagerRuntime` unit harness may not exist — the refactor is still gated by CI + existing tests. All other steps carry real code.

**Type consistency:** `DeployError` variants used in `deploy_error_reply` (T6) — `BadRequest/DiscoveryUnavailable/Unresolved` introduced in T4. `sorx_url: Option<String>` threaded through DeploySpec/Record (T4), DeployBody (T6). `SorxResolver::resolve(&self, &str, &str) -> Option<String>` identical in T3/T5/T8. `run_with_client(input, dry_run, return_card, &dyn client)` (T2) called in T5. `resolve_endpoint(&Directory, &str, &str)` (T1) used by PresenceResolver (T8) and the shared subscriber (T7).

## Execution Notes
No local cargo → validate via CI (`perf` = `--all-features`, which builds the `events`-gated presence/resolver code; `ci` may not). rustfmt offline before each commit. Events-gated tests must be run with `-p operax-cli --features events` locally-conceptually / relied on `perf` in CI. Batch CI pushes at natural points (after T2, T6, T9).
