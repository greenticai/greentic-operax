# OperaX Hardening — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Land the 4 deferred follow-ups from feature #4: NATS reconnect, `caller_role="business-event"` on routed runs, concurrent event fan-out, opt-in environment discrimination.

**Architecture:** All additive except the intentional `SorxResolver::resolve` trait break. `environment: Option<String>` with `None`=wildcard keeps back-compat. Reconnect via a shared backoff combinator in operax-cli. `caller_role` via a `run_as` variant using the existing `OperaxContext::with_caller_role`.

**Tech Stack:** Rust edition 2024; `async_nats` 0.46, `tokio`, `futures` (events-gated in operax-cli).

## Global Constraints

- Base `main` (post feature #4). `greentic-types 1.1`, no git-deps — NOT on the release-train.
- No `unwrap()`/`panic!()`/`expect()` outside `#[cfg(test)]`; poison → error/empty. `operax_core::{Result,OperaxError}` in operax-core/operax-manager (no anyhow); anyhow only in operax-cli events-gated code.
- Clippy-clean `-D warnings` (let-chains; `is_none_or`/`is_some_and`; no needless clone). English only; Conventional Commits; no AI-authorship trailers.
- **No local cargo** (no network): validate via CI. **rustfmt offline** — run before each commit. `perf` = `--all-features` builds events-gated code; `ci` may not.
- **Verified facts:**
  - `OperaxContext { caller_role: String, ... }`; `OperaxContext::new(tenant, team, locale, pack_digest) -> Result<Self>` defaults `caller_role="service"`; `OperaxContext::with_caller_role(self, impl Into<String>) -> Result<Self>` (validates non-empty) EXISTS.
  - `ManagerRuntime::run_with_client(&self, input: Value, dry_run: bool, return_card: bool, client: &(dyn SorxClient+Send+Sync)) -> Result<ManagerRunResult>` builds ctx via `OperaxContext::new`; `run_input` delegates.
  - `DeploymentManager::run(&self, id, input, dry_run) -> Result<ManagerRunResult, DeployError>`; discover branch calls `resolver.resolve(&tenant, &sor)` and `rt.run_with_client(...)`, static calls `rt.run_input(...)`. `route_event(&self, tenant, topic, input, dry_run) -> Vec<RouteOutcome>`.
  - `SorxResolver::resolve(&self, tenant: &str, sor: &str) -> Option<String>` (operax-manager); `PresenceResolver::resolve` delegates to `operax_core`-free `resolve_endpoint(dir, tenant, sor)` (presence.rs); `resolve_endpoint` filters `reachable && presence.tenant==tenant && presence.sor==sor`.
  - `DeploySpec`/`DeploymentRecord`: `{ id, tenant, team, locale, sorx_url: Option, sor: Option, active, history }` (record has serde defaults on sorx_url/sor). `DeployBody` in serve.rs maps to `DeploySpec`.
  - `EventEnvelope.tenant: TenantCtx { env: EnvId, tenant: TenantId, ... }`; event env = `env.tenant.env`; `EnvId` is a string newtype (use `.as_str()`).
  - Subscribers: `run_presence_subscriber(config, Arc<RwLock<Directory>>)`, `run_event_router(nats_url, Arc<DeploymentManager>)`, `run_subscriber(config)` — each `rt.block_on(async { connect; subscribe; while-let; Ok })`, own current-thread runtime with `.enable_all()`.

---

## File Structure
- `crates/operax-manager/src/deployment.rs` — `environment` on DeploySpec/DeploymentRecord; `run_as`; `matching_deployments`; `route_event` gains `event_env`; resolver call site.
- `crates/operax-manager/src/lib.rs` — `ManagerRuntime::run_with_client_as`.
- `crates/operax-manager/src/serve.rs` — `DeployBody.environment`.
- `crates/operax-cli/src/presence.rs` — `resolve_endpoint(env,...)`; `PresenceResolver`; reconnect on `run_presence_subscriber`.
- `crates/operax-cli/src/business_events.rs` — concurrent fan-out + env in `run_event_router`; reconnect on it + `run_subscriber`.
- `crates/operax-cli/src/nats_reconnect.rs` (CREATE) — the backoff combinator.
- `crates/operax-cli/src/lib.rs` — declare `nats_reconnect` module.
- README + e2e test.

---

## Task 1: `environment` field on the deploy contract

**Files:** `crates/operax-manager/src/deployment.rs`, `crates/operax-manager/src/serve.rs`.

**Interfaces:** `DeploySpec.environment: Option<String>`; `DeploymentRecord.environment: Option<String>` (`#[serde(default)]`); `DeployBody.environment: Option<String>` (`#[serde(default)]`).

- [ ] **Step 1: failing test** — add to deployment.rs tests:
```rust
#[test]
fn deploy_carries_environment() {
    let mgr = test_manager();
    let mut spec = deploy_spec("envdep");
    spec.environment = Some("prod".to_string());
    mgr.deploy(spec).expect("deploy");
    let detail = mgr.get("envdep").expect("exists");
    assert_eq!(detail.record.environment.as_deref(), Some("prod"));
}
```
- [ ] **Step 2: run to verify fail** — `cargo test -p operax-manager deploy_carries_environment` → FAIL (no field).
- [ ] **Step 3: implement** — add `environment: Option<String>` to `DeploySpec` and to `DeploymentRecord` (with `#[serde(default)]`); thread `environment` from spec into the record in `deploy` (and preserve it in `upgrade`, which rebuilds from the existing record). Update the `deploy_spec` test helper to set `environment: None`, and every `DeploymentRecord { ... }` / `DeploySpec { ... }` literal in the tests (the 3 fixtures + the `deployment_store` roundtrip test in `deployment_store.rs`) to include `environment: None`. In `serve.rs`, add `#[serde(default)] environment: Option<String>` to `DeployBody` and pass it into the `DeploySpec` it builds.
- [ ] **Step 4: run to verify pass** — PASS + the existing deploy tests still compile with the new field.
- [ ] **Step 5: rustfmt + commit**
```bash
rustfmt --edition 2024 crates/operax-manager/src/deployment.rs crates/operax-manager/src/serve.rs crates/operax-manager/src/deployment_store.rs
git add crates/operax-manager/src/deployment.rs crates/operax-manager/src/serve.rs crates/operax-manager/src/deployment_store.rs
git commit -m "feat(operax): add optional environment to the deploy contract"
```

---

## Task 2: `SorxResolver::resolve` gains `env`; env-aware `resolve_endpoint`

**Files:** `crates/operax-manager/src/deployment.rs` (trait + `run` call site + StubResolver test), `crates/operax-cli/src/presence.rs` (`resolve_endpoint` + `PresenceResolver` + test).

**Interfaces:** `SorxResolver::resolve(&self, env: Option<&str>, tenant: &str, sor: &str) -> Option<String>`; `resolve_endpoint(dir, env: Option<&str>, tenant, sor) -> Option<String>`.

- [ ] **Step 1: failing test** — update `presence.rs`'s `resolve_picks_freshest_reachable` (or add) to cover env filtering:
```rust
#[test]
fn resolve_filters_by_environment() {
    let mut dir = Directory::new();
    // two reachable entries, same tenant+sor, different environment
    apply_presence(&mut dir, pres_env("i1", "t1", "orders", "http://prod", "prod", true), 10);
    apply_presence(&mut dir, pres_env("i2", "t1", "orders", "http://staging", "staging", true), 20);
    assert_eq!(resolve_endpoint(&dir, Some("prod"), "t1", "orders").as_deref(), Some("http://prod"));
    // env: None -> wildcard, freshest wins
    assert_eq!(resolve_endpoint(&dir, None, "t1", "orders").as_deref(), Some("http://staging"));
}
```
(Add a `pres_env(instance, tenant, sor, url, environment, reachable)` helper, or extend the existing `pres`/`presence_with_url` helper with an `environment` arg.)
- [ ] **Step 2: run to verify fail** — `cargo test -p operax-cli --features events presence::tests::resolve_filters_by_environment` → FAIL.
- [ ] **Step 3: implement**
  - `resolve_endpoint`: add `env: Option<&str>` param; extend the filter with `&& env.is_none_or(|e| entry.presence.environment == e)`.
  - `PresenceResolver::resolve(&self, env: Option<&str>, tenant, sor)` → `resolve_endpoint(&dir, env, tenant, sor)`.
  - `SorxResolver` trait (deployment.rs): `fn resolve(&self, env: Option<&str>, tenant: &str, sor: &str) -> Option<String>;`.
  - Update the `run` discover call site (deployment.rs) to `resolver.resolve(record_environment.as_deref(), &tenant, &sor)` — capture `record.environment.clone()` into a local under the read lock alongside `sor`/`tenant`.
  - Update the `StubResolver` test impl (deployment.rs) and `presence_resolver_delegates` test (presence.rs) to the new 3-arg `resolve` signature (StubResolver can ignore `env`).
- [ ] **Step 4: run to verify pass** — PASS.
- [ ] **Step 5: rustfmt + commit**
```bash
rustfmt --edition 2024 crates/operax-manager/src/deployment.rs crates/operax-cli/src/presence.rs
git add crates/operax-manager/src/deployment.rs crates/operax-cli/src/presence.rs
git commit -m "feat(operax): environment-aware SoRX resolution"
```

---

## Task 3: `caller_role` — `run_with_client_as` + `run_as`

**Files:** `crates/operax-manager/src/lib.rs` (`run_with_client_as`), `crates/operax-manager/src/deployment.rs` (`run_as`).

**Interfaces:** `ManagerRuntime::run_with_client_as(&self, input, dry_run, return_card, caller_role: Option<&str>, client: &(dyn SorxClient+Send+Sync)) -> Result<ManagerRunResult>`; `DeploymentManager::run_as(&self, id, input, dry_run, caller_role: Option<&str>) -> Result<ManagerRunResult, DeployError>`.

- [ ] **Step 1: failing test** (operax-manager, using the `runtime()` fixture + a dry-run so the report is produced):
```rust
#[test]
fn run_with_client_as_stamps_caller_role() {
    // If RunReport/ctx exposes caller_role, assert it. Otherwise assert the two
    // paths (Some vs None) both succeed on the same input. Read the ManagerRunResult /
    // RunReport shape to pick the strongest available assertion.
}
```
(Note to implementer: pick the strongest assertion the types allow — if `caller_role` is not observable from `ManagerRunResult`, assert both `Some("business-event")` and `None` runs succeed, and rely on Task 4/5's routing tests + code review for the stamp; state the choice in the report.)
- [ ] **Step 2: run to verify fail** — FAIL until the method exists.
- [ ] **Step 3: implement**
  - `run_with_client_as`: body of `run_with_client` but `let ctx = OperaxContext::new(...)?; let ctx = match caller_role { Some(r) => ctx.with_caller_role(r)?, None => ctx };` then `run_loaded_pack(&self.pack, &ctx, ...)`. `run_with_client` delegates: `self.run_with_client_as(input, dry_run, return_card, None, client)`.
  - `run_as`: copy `run`'s body but call `rt.run_with_client_as(input, dry_run, false, caller_role, client.as_ref())` (discover) / `rt.run_with_client_as(input, dry_run, false, caller_role, rt_client)` — for the static branch, `run_input` uses the frozen client; add a static path via `run_with_client_as(..., caller_role, self.client_of(rt))`. SIMPLER: have `run` delegate to `run_as(id, input, dry_run, None)`, and `run_as` implement the full branching (static + discover) calling `run_with_client_as` in both arms (static arm builds no fresh client — it needs the runtime's own client; expose it or add a `ManagerRuntime::run_input_as(input, dry_run, return_card, caller_role)` that delegates to `run_with_client_as(..., self.client.as_ref())`). Add `ManagerRuntime::run_input_as(&self, input, dry_run, return_card, caller_role: Option<&str>) -> Result<ManagerRunResult>` = `self.run_with_client_as(input, dry_run, return_card, caller_role, self.client.as_ref())`; `run_input` delegates with `None`. Then `run_as` static arm = `rt.run_input_as(input, dry_run, false, caller_role)`, discover arm = `rt.run_with_client_as(input, dry_run, false, caller_role, client.as_ref())`. `run` = `self.run_as(id, input, dry_run, None)`.
- [ ] **Step 4: run to verify pass** — PASS; existing `run` tests still green (delegates None).
- [ ] **Step 5: rustfmt + commit**
```bash
rustfmt --edition 2024 crates/operax-manager/src/lib.rs crates/operax-manager/src/deployment.rs
git add crates/operax-manager/src/lib.rs crates/operax-manager/src/deployment.rs
git commit -m "feat(operax): run_as/run_with_client_as thread caller_role"
```

---

## Task 4: `matching_deployments` + env-aware `route_event`

**Files:** `crates/operax-manager/src/deployment.rs`.

**Interfaces:** `matching_deployments(&self, event_env: &str, tenant: &str, topic: &str) -> Vec<String>`; `route_event(&self, event_env: &str, tenant: &str, topic: &str, input: Value, dry_run: bool) -> Vec<RouteOutcome>` (now takes `event_env`, delegates match to `matching_deployments`, runs via `run_as(..., Some("business-event"))`).

- [ ] **Step 1: failing test** — update the existing route_event test to pass an env + add an env-filter case:
```rust
#[test]
fn matching_deployments_filters_env_tenant_topic() {
    let mgr = test_manager();
    mgr.deploy(deploy_spec("recon")).expect("deploy");        // env None (wildcard)
    let mut prod = deploy_spec("recon-prod");
    prod.environment = Some("prod".to_string());
    mgr.deploy(prod).expect("deploy prod");
    // "prod" event matches both the wildcard and the prod deployment
    let mut ids = mgr.matching_deployments("prod", "demo", "sorla.tenancy.payment-recorded");
    ids.sort();
    assert_eq!(ids, vec!["recon".to_string(), "recon-prod".to_string()]);
    // "staging" event matches only the wildcard
    assert_eq!(mgr.matching_deployments("staging", "demo", "sorla.tenancy.payment-recorded"), vec!["recon"]);
}
```
Also update the existing `route_event_runs_only_matching_same_tenant_deployments` test to pass an `event_env` arg (e.g. `"prod"`), keeping its assertions.
- [ ] **Step 2: run to verify fail** — FAIL.
- [ ] **Step 3: implement**
  - `matching_deployments`: read-lock; filter `slot.record.tenant == tenant && matches!(status, Ready) && slot.record.environment.as_deref().is_none_or(|e| e == event_env) && slot.runtime.as_ref().is_some_and(|rt| rt.consumes().iter().any(|s| operax_core::topic_matches(&s.capability, topic)))`; collect ids; drop lock.
  - `route_event(event_env, tenant, topic, input, dry_run)`: `matching_deployments(event_env, tenant, topic)` then per id `self.run_as(&id, input.clone(), dry_run, Some("business-event"))` → `RouteOutcome`.
- [ ] **Step 4: run to verify pass** — PASS.
- [ ] **Step 5: rustfmt + commit** — `feat(operax): matching_deployments + env-aware route_event stamping business-event`.

---

## Task 5: concurrent fan-out + env in `run_event_router`

**Files:** `crates/operax-cli/src/business_events.rs`.

- [ ] **Step 1: implement** (no unit test — needs a broker; covered by Task 4 + e2e). In `run_event_router`'s loop, replace the single `spawn_blocking(route_event)` with:
```rust
            let event_env = env.tenant.env.as_str().to_string();
            let tenant = env.tenant.tenant.to_string();
            let topic = env.topic.clone();
            let payload = env.payload.clone();
            let matched = manager.matching_deployments(&event_env, &tenant, &topic);
            if matched.is_empty() {
                eprintln!("[operax serve] event {topic} matched no deployments");
                continue;
            }
            let handles: Vec<_> = matched
                .into_iter()
                .map(|id| {
                    let mgr = manager.clone();
                    let payload = payload.clone();
                    let topic = topic.clone();
                    tokio::task::spawn_blocking(move || {
                        let result = mgr.run_as(&id, payload, false, Some("business-event"));
                        (topic, id, result)
                    })
                })
                .collect();
            for joined in futures::future::join_all(handles).await {
                match joined {
                    Ok((topic, id, Ok(_))) => eprintln!("[operax serve] routed {topic} -> {id}: ok"),
                    Ok((topic, id, Err(e))) => eprintln!("[operax serve] routed {topic} -> {id}: failed: {e:?}"),
                    Err(join_err) => eprintln!("[operax serve] route task panicked: {join_err}"),
                }
            }
```
Verify `env.tenant.env` access + `EnvId::as_str()` against greentic-types (adjust to `.to_string()` if `as_str` isn't available). `futures::future::join_all` — `futures` is already a dep.
- [ ] **Step 2: rustfmt + commit** — `rustfmt --edition 2024 crates/operax-cli/src/business_events.rs`; commit `feat(operax): concurrent env-scoped event fan-out in run_event_router`.

---

## Task 6: NATS reconnect combinator + apply to all three subscribers

**Files:** `crates/operax-cli/src/nats_reconnect.rs` (CREATE), `crates/operax-cli/src/lib.rs` (module decl), `crates/operax-cli/src/{presence.rs, business_events.rs}`.

**Interfaces:** `pub async fn run_with_reconnect<F, Fut>(mut session: F) where F: FnMut() -> Fut, Fut: Future<Output = anyhow::Result<()>>` — loops forever: run `session()`, log its return/error, `tokio::time::sleep(backoff)`, escalate backoff (1s→30s cap), reset to 1s after a session that ran past a threshold. Keep the backoff schedule a small pure helper `fn next_backoff(current: Duration) -> Duration` unit-tested.

- [ ] **Step 1: failing test** (nats_reconnect.rs) — unit-test `next_backoff`: `1s→2s→4s→…→30s (cap)`; and a `run_with_reconnect` test with a `session` that returns `Ok` after incrementing a counter, using an injected no-op sleep or a max-iterations guard so it doesn't loop forever (structure `run_with_reconnect` to accept a `sleep: impl Fn(Duration) -> Fut2` OR bound iterations behind a test-only cap — pick whichever keeps it testable without real sleeping; document the choice).
- [ ] **Step 2: run to verify fail** — `cargo test -p operax-cli --features events nats_reconnect` → FAIL.
- [ ] **Step 3: implement** `next_backoff` + `run_with_reconnect`; add `#[cfg(feature="events")] mod nats_reconnect;` to lib.rs. Then wrap each subscriber's `block_on` body: hoist one-time setup (the pack load in `run_subscriber`) outside, and make the connect+subscribe+while-let the `session` closure passed to `run_with_reconnect`. On session end/err → the combinator logs + backs off + retries (replace the current terminal "subscription ended" log/return with the retry).
- [ ] **Step 4: run to verify pass** — PASS.
- [ ] **Step 5: rustfmt + commit** — `feat(operax): NATS reconnect with capped backoff for all subscribers`.

---

## Task 7: environment-scoped routing e2e

**Files:** `crates/operax-manager/tests/env_routing_e2e.rs` (CREATE).

- [ ] **Step 1: failing test** — mirror `event_routing_e2e.rs`: build a manager (StubClient builder + unique temp registry), deploy the tenancy pack with `environment: Some("prod")` (tenant `demo`); assert `route_event("prod", "demo", "sorla.tenancy.payment-recorded", <tenancy input>, true)` runs it (1 outcome, Ok), and `route_event("staging", "demo", <same topic>, ..., true)` is empty (env mismatch). Read the current `DeploySpec` fields (now incl. `environment`).
- [ ] **Step 2: run to verify fail** — `cargo test -p operax-manager --test env_routing_e2e` → FAIL.
- [ ] **Step 3: complete wiring** (inline StubClient, unique registry).
- [ ] **Step 4: run to verify pass** — PASS.
- [ ] **Step 5: rustfmt + commit** — `test(operax): environment-scoped event routing e2e`.

---

## Task 8: docs

**Files:** `crates/operax-cli/README.md`.

- [ ] **Step 1:** Document: the deploy body accepts optional `environment` (omit = wildcard, matches all environments; set = strict scoping for both discovery and event routing); routed runs are stamped `caller_role="business-event"`; the daemon's NATS subscribers reconnect automatically with backoff; event fan-out is concurrent. Remove the now-resolved follow-ups from the S3 section's "follow-ups" note (reconnect, caller_role, concurrent, env-discrimination are DONE).
- [ ] **Step 2: commit** — `docs(operax): document environment scoping, business-event caller_role, reconnect`.

---

## Self-Review

**Spec coverage:** reconnect (T6); caller_role (T3 + used in T4/T5); concurrent fan-out (T4 split + T5 router); environment (T1 contract + T2 resolve + T4 route + T7 e2e). ✓

**Placeholder scan:** T3's caller_role unit test and T5/T6 note that a strong assertion may not be observable / needs a broker — flagged with the fallback (code review + routing tests + compilation). T6's `run_with_reconnect` testability (injected sleep or iteration cap) is left to the implementer with the requirement stated. No fabricated placeholders.

**Type consistency:** `SorxResolver::resolve(Option<&str>, &str, &str)` identical in T2 (trait/impl/StubResolver) and consumed in `run` (T2 call site). `resolve_endpoint(dir, Option<&str>, &str, &str)` T2. `run_as`/`run_with_client_as`/`run_input_as` with `caller_role: Option<&str>` T3, called in T4. `matching_deployments(event_env, tenant, topic)` T4, called in T5. `route_event(event_env, tenant, topic, input, dry_run)` T4. `environment: Option<String>` threaded T1 (DeploySpec/Record/DeployBody) → T2 (resolve arg) → T4 (route filter).

## Execution Notes
No local cargo → CI (`perf` = `--all-features` builds events code). rustfmt offline per commit. Batch CI after T2, T4, T7. Verify `EnvId::as_str()` and `TenantCtx.env` access against greentic-types at T5 impl (the one unverified access).
