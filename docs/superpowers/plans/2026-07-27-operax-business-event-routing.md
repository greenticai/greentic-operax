# OperaX Business-Event → Deployment Routing — Implementation Plan (Feature #4, Slice 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** The `operax serve` daemon routes each incoming business event to every deployment whose pack `consumes` the topic (same tenant), running each.

**Architecture:** Extract the cap→topic matcher to a pure `operax_core::topic_matches`; add `ManagerRuntime::consumes` + a NATS-free `DeploymentManager::route_event(tenant, topic, input, dry_run)` fan-out reusing the S1/S2 `run` path; host a `greentic.events.>` subscriber in the daemon (events-gated) that calls `route_event`. `operax-manager` stays NATS-free.

**Tech Stack:** Rust edition 2024; reuse `business_events.rs`'s subscriber shape, `DeploymentManager::run`, `EventEnvelope` (greentic-types).

## Global Constraints

- Base `main` (post-S1+S2). `greentic-types 1.1`, no git-deps — NOT on the release-train.
- No `unwrap()`/`panic!()`/`expect()` outside `#[cfg(test)]`; poison → error/empty, not panic. `operax_core::{Result, OperaxError}` in operax-manager/operax-core (NO anyhow); `anyhow` only in operax-cli events-gated code.
- Clippy-clean `-D warnings` (let-chains not collapsible_if; no needless clone). English only; Conventional Commits; no AI-authorship trailers.
- **No local cargo** (no network): validate via `greentic-operax` CI. **rustfmt offline** — run before every commit. `perf` = `cargo test --workspace --all-features` builds events-gated code; `ci` may not.
- **Verified facts:**
  - Matcher (currently `business_events.rs:19-84`) uses ONLY `capability: &str` + `topic: &str`. `EventSubscription` (operax-core) = `{ id, capability, mode, metadata }`. `EventEnvelope` (greentic-types): `env.tenant.tenant` (TenantId), `env.topic: String`, `env.payload: Value`.
  - `OperationalPack.metadata.consumes: Vec<EventSubscription>` (`operax-core`).
  - `ManagerRuntime.pack` is private; `DeploymentManager.slots: RwLock<HashMap<String, DeploymentSlot>>` private; `DeploymentSlot { record, runtime: Option<Arc<ManagerRuntime>>, status }`.
  - `DeploymentManager::run(&self, id: &str, input: Value, dry_run: bool) -> Result<ManagerRunResult, DeployError>` (handles static + discover).
  - Fixture: `examples/tenancy/handoff/operala.yaml` declares `consumes[0].capability = cap://greentic/events/tenancy/v1/payment-recorded` → `topic_matches` yields topic `sorla.tenancy.payment-recorded`.
  - `run_serve` (operax-cli) already hosts the S2 presence thread under `#[cfg(feature="events")]`; the manager is `Arc::new`'d before `start_deployment_server`.

---

## File Structure
- `crates/operax-core/src/lib.rs` (MODIFY) — `topic_matches` + moved `cap_domain_name`/`sanitize_segment`/`EVENTS_CAP_PREFIX` + tests.
- `crates/operax-cli/src/business_events.rs` (MODIFY) — `event_matches` delegates to `operax_core::topic_matches`; remove the moved helpers; `run_event_router`.
- `crates/operax-manager/src/lib.rs` (MODIFY) — `ManagerRuntime::consumes`.
- `crates/operax-manager/src/deployment.rs` (MODIFY) — `RouteOutcome` + `route_event`.
- `crates/operax-cli/src/lib.rs` (MODIFY) — host the router in `run_serve`.
- `crates/operax-manager/tests/event_routing_e2e.rs` (CREATE) — route a deployed pack.
- README (MODIFY).

---

## Task 1: `operax_core::topic_matches`

**Files:** Modify `crates/operax-core/src/lib.rs` (+ `#[cfg(test)]`).

**Interfaces:**
- Produces: `pub fn topic_matches(capability: &str, topic: &str) -> bool`.

- [ ] **Step 1: failing test**
```rust
#[test]
fn topic_matches_entity_and_command_forms() {
    // command form: cap .../tenancy/v1/payment-recorded -> sorla.tenancy.payment-recorded
    assert!(topic_matches(
        "cap://greentic/events/tenancy/v1/payment-recorded",
        "sorla.tenancy.payment-recorded"
    ));
    // non-events cap -> never matches
    assert!(!topic_matches("cap://greentic/other/x", "sorla.tenancy.payment-recorded"));
    // wrong topic -> no match
    assert!(!topic_matches(
        "cap://greentic/events/tenancy/v1/payment-recorded",
        "sorla.tenancy.other"
    ));
    // entity form: name with a dot -> sorla.<domain>.<Entity>.<op>
    assert!(topic_matches(
        "cap://greentic/events/tenancy/Payment.created",
        "sorla.tenancy.Payment.created"
    ));
}
```
- [ ] **Step 2: run to verify fail** — `cargo test -p operax-core topic_matches_entity_and_command_forms` → FAIL (undefined).
- [ ] **Step 3: implement** — add to `operax-core/src/lib.rs` (these are the exact helpers from `business_events.rs:19-84`, adapted to take strings):
```rust
const EVENTS_CAP_PREFIX: &str = "cap://greentic/events/";

/// Keep `[A-Za-z0-9_-]`, map every other char (incl. `.`) to `-` — mirrors SoRX's
/// topic-segment sanitization.
fn sanitize_segment(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Parse an events cap into `(domain, name)`, tolerating an optional `vN` version
/// segment (`cap://greentic/events/<domain>/[v1/]<name>`). `None` for non-events caps.
fn cap_domain_name(cap: &str) -> Option<(String, String)> {
    let rest = cap.strip_prefix(EVENTS_CAP_PREFIX)?;
    let mut segs: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() < 2 {
        return None;
    }
    let name = segs.pop()?;
    if segs.len() > 1
        && segs.last().is_some_and(|s| {
            s.len() >= 2 && s.starts_with('v') && s[1..].chars().all(|c| c.is_ascii_digit())
        })
    {
        segs.pop();
    }
    let domain = segs.join("-");
    Some((domain, name.to_string()))
}

/// Returns `true` when a business event on `topic` is a delivery for the SoRLa
/// capability `capability`. Tries both entity-lifecycle and command publish shapes.
pub fn topic_matches(capability: &str, topic: &str) -> bool {
    let Some((domain, name)) = cap_domain_name(capability) else {
        return false;
    };
    let san_domain = sanitize_segment(&domain);
    let entity_tail = format!(
        "{san_domain}.{}",
        name.split('.').map(sanitize_segment).collect::<Vec<_>>().join(".")
    );
    let command_tail = format!("{san_domain}.{}", sanitize_segment(&name));
    [entity_tail, command_tail]
        .into_iter()
        .any(|tail| topic == format!("sorla.{tail}"))
}
```
- [ ] **Step 4: run to verify pass** — PASS.
- [ ] **Step 5: rustfmt + commit**
```bash
rustfmt --edition 2024 crates/operax-core/src/lib.rs
git add crates/operax-core/src/lib.rs
git commit -m "feat(operax): operax_core::topic_matches (cap->topic matcher)"
```

---

## Task 2: `business_events::event_matches` delegates to `topic_matches`

**Files:** Modify `crates/operax-cli/src/business_events.rs`.

**Interfaces:** `event_matches(sub, env)` keeps its signature but delegates; the moved helpers (`sanitize_segment`, `cap_domain_name`, `EVENTS_CAP_PREFIX`) are removed from this file.

- [ ] **Step 1: keep the existing `event_matches` tests** (they already live in `business_events.rs`'s `#[cfg(test)]`) — they must stay green after the delegation. Do NOT delete them.
- [ ] **Step 2: run to verify current pass** — `cargo test -p operax-cli --features events business_events` (they pass today).
- [ ] **Step 3: implement** — replace the body of `event_matches` with:
```rust
pub fn event_matches(sub: &EventSubscription, env: &EventEnvelope) -> bool {
    operax_core::topic_matches(&sub.capability, &env.topic)
}
```
and DELETE the now-unused `sanitize_segment`, `cap_domain_name`, and `EVENTS_CAP_PREFIX` from `business_events.rs` (they live in operax-core now). Keep `EventSubscription`/`EventEnvelope` imports (still used by `event_matches`'s signature + the subscriber). Verify `operax-cli` depends on `operax-core` (it does).
- [ ] **Step 4: run to verify pass** — the existing `event_matches` tests still pass (now via the delegated impl).
- [ ] **Step 5: rustfmt + commit**
```bash
rustfmt --edition 2024 crates/operax-cli/src/business_events.rs
git add crates/operax-cli/src/business_events.rs
git commit -m "refactor(operax): event_matches delegates to operax_core::topic_matches"
```

---

## Task 3: `ManagerRuntime::consumes` accessor

**Files:** Modify `crates/operax-manager/src/lib.rs`.

**Interfaces:** `pub fn consumes(&self) -> &[operax_core::EventSubscription]` on `ManagerRuntime`, returning `&self.pack.metadata.consumes`.

- [ ] **Step 1: failing test** (reuse the crate's existing `runtime()` test fixture that builds a `ManagerRuntime`; if that fixture's pack has no `consumes`, this test just asserts the slice is returned — length may be 0)
```rust
#[test]
fn consumes_accessor_returns_pack_subscriptions() {
    let rt = runtime(); // existing helper
    // Accessor compiles + returns the pack's consumes slice (len depends on fixture).
    let _subs: &[operax_core::EventSubscription] = rt.consumes();
}
```
- [ ] **Step 2: run to verify fail** — `cargo test -p operax-manager consumes_accessor_returns_pack_subscriptions` → FAIL (no method).
- [ ] **Step 3: implement**
```rust
/// The business-event subscriptions declared by this deployment's active pack.
pub fn consumes(&self) -> &[operax_core::EventSubscription] {
    &self.pack.metadata.consumes
}
```
(Confirm the path to `EventSubscription` — it is `operax_core::EventSubscription`; if `OperaHandoffMetadata.consumes`'s element type is re-exported differently, match the actual type.)
- [ ] **Step 4: run to verify pass** — PASS.
- [ ] **Step 5: rustfmt + commit**
```bash
rustfmt --edition 2024 crates/operax-manager/src/lib.rs
git add crates/operax-manager/src/lib.rs
git commit -m "feat(operax): ManagerRuntime::consumes accessor"
```

---

## Task 4: `DeploymentManager::route_event`

**Files:** Modify `crates/operax-manager/src/deployment.rs`.

**Interfaces:**
- `pub struct RouteOutcome { pub deployment_id: String, pub result: Result<crate::ManagerRunResult, DeployError> }`
- `pub fn route_event(&self, tenant: &str, topic: &str, input: serde_json::Value, dry_run: bool) -> Vec<RouteOutcome>`

- [ ] **Step 1: failing test** (reuse `test_manager`, `deploy_spec`, `repo_examples`, `StubResolver`; the tenancy fixture's consumes matches topic `sorla.tenancy.payment-recorded`)
```rust
#[test]
fn route_event_runs_only_matching_same_tenant_deployments() {
    let mgr = test_manager();
    // static-mode deploy of the tenancy pack (declares consumes payment-recorded)
    mgr.deploy(deploy_spec("recon")).expect("deploy");           // tenant "demo"
    // a wrong-tenant deployment of the same pack
    let mut other = deploy_spec("recon-other");
    other.tenant = "other-tenant".to_string();
    mgr.deploy(other).expect("deploy other");

    let payload = serde_json::json!({ "example": true });
    // matching topic for tenant "demo" -> exactly "recon" runs
    let outcomes = mgr.route_event("demo", "sorla.tenancy.payment-recorded", payload.clone(), true);
    let ids: Vec<&str> = outcomes.iter().map(|o| o.deployment_id.as_str()).collect();
    assert_eq!(ids, vec!["recon"]);
    assert!(outcomes[0].result.is_ok());

    // non-matching topic -> no deployments
    assert!(mgr.route_event("demo", "sorla.tenancy.nope", payload.clone(), true).is_empty());
    // matching topic but wrong tenant string -> no deployments
    assert!(mgr.route_event("nobody", "sorla.tenancy.payment-recorded", payload, true).is_empty());
}
```
(`deploy_spec` yields tenant `"demo"` — confirm; the run is `dry_run=true` so SoRX is never called.)
- [ ] **Step 2: run to verify fail** — `cargo test -p operax-manager route_event_runs_only_matching` → FAIL.
- [ ] **Step 3: implement**
```rust
#[derive(Debug)]
pub struct RouteOutcome {
    pub deployment_id: String,
    pub result: Result<crate::ManagerRunResult, DeployError>,
}

pub fn route_event(
    &self,
    tenant: &str,
    topic: &str,
    input: serde_json::Value,
    dry_run: bool,
) -> Vec<RouteOutcome> {
    // Collect matching ids under the read lock, then release it before running.
    let matched: Vec<String> = match self.slots.read() {
        Ok(slots) => slots
            .values()
            .filter(|slot| {
                slot.record.tenant == tenant
                    && matches!(slot.status, DeploymentStatus::Ready)
                    && slot
                        .runtime
                        .as_ref()
                        .is_some_and(|rt| {
                            rt.consumes()
                                .iter()
                                .any(|sub| operax_core::topic_matches(&sub.capability, topic))
                        })
            })
            .map(|slot| slot.record.id.clone())
            .collect(),
        Err(_) => return Vec::new(),
    };
    matched
        .into_iter()
        .map(|id| {
            let result = self.run(&id, input.clone(), dry_run);
            RouteOutcome { deployment_id: id, result }
        })
        .collect()
}
```
- [ ] **Step 4: run to verify pass** — PASS.
- [ ] **Step 5: rustfmt + commit** — `feat(operax): DeploymentManager::route_event fan-out over consumes`.

---

## Task 5: `run_event_router` daemon subscriber

**Files:** Modify `crates/operax-cli/src/business_events.rs`.

**Interfaces:** `pub fn run_event_router(nats_url: String, manager: std::sync::Arc<operax_manager::deployment::DeploymentManager>) -> anyhow::Result<()>`.

- [ ] **Step 1: failing test** — a full loop needs a NATS broker (out of scope). Add NO unit test here; the routing logic is covered by Task 4's `route_event` test and Task 7's e2e. State this in the report. (If a pure helper is factored out — e.g. a function computing the subject string — a trivial unit test for it is welcome but not required.)
- [ ] **Step 2: (n/a — covered by Task 4 + Task 7)**
- [ ] **Step 3: implement** — mirror `run_subscriber`'s shape (own current-thread tokio runtime, `async_nats::connect`, subscribe **`"greentic.events.>"`**, decode `EventEnvelope` skip-and-log), but route into the manager:
```rust
pub fn run_event_router(
    nats_url: String,
    manager: std::sync::Arc<operax_manager::deployment::DeploymentManager>,
) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async move {
        let client = async_nats::connect(&nats_url).await?;
        let mut subscriber = client.subscribe("greentic.events.>").await?;
        eprintln!("[operax serve] event router subscribed to greentic.events.>");
        while let Some(msg) = subscriber.next().await {
            let env: EventEnvelope = match serde_json::from_slice(&msg.payload) {
                Ok(env) => env,
                Err(err) => {
                    eprintln!("skip undecodable event on {}: {err}", msg.subject);
                    continue;
                }
            };
            let tenant = env.tenant.tenant.to_string();
            let outcomes = manager.route_event(&tenant, &env.topic, env.payload.clone(), false);
            if outcomes.is_empty() {
                eprintln!("[operax serve] event {} matched no deployments", env.topic);
            }
            for outcome in outcomes {
                match &outcome.result {
                    Ok(_) => eprintln!(
                        "[operax serve] routed {} -> {}: ok",
                        env.topic, outcome.deployment_id
                    ),
                    Err(e) => eprintln!(
                        "[operax serve] routed {} -> {}: failed: {e:?}",
                        env.topic, outcome.deployment_id
                    ),
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    })
}
```
(`use futures::StreamExt;` is already in the file for `subscriber.next()`. `EventEnvelope` import already present.)
- [ ] **Step 4: run to verify** — CI compiles it (events build). No unit test.
- [ ] **Step 5: rustfmt + commit** — `feat(operax): run_event_router routes NATS events into the deployment registry`.

---

## Task 6: host the router in `run_serve`

**Files:** Modify `crates/operax-cli/src/lib.rs`.

**Interfaces:** after `let manager = Arc::new(manager);`, under `#[cfg(feature="events")]`, spawn the router thread if `OPERAX_EVENTS_NATS_URL` is set.

- [ ] **Step 1: implement** (no unit test — same rationale as the S2 hosting block). After the existing `let manager = Arc::new(manager);` line in `run_serve`, add:
```rust
    #[cfg(feature = "events")]
    if let Ok(events_nats_url) = std::env::var("OPERAX_EVENTS_NATS_URL") {
        let manager_for_router = manager.clone();
        std::thread::spawn(move || {
            if let Err(e) = crate::business_events::run_event_router(events_nats_url, manager_for_router) {
                eprintln!("[operax serve] event router ended: {e}");
            }
        });
    } else {
        #[cfg(feature = "events")]
        eprintln!("[operax serve] OPERAX_EVENTS_NATS_URL unset; event routing inert");
    }
```
(Place it BEFORE the final `eprintln!("[operax serve] listening ...")` / `start_deployment_server(manager, ...)`. Ensure the `#[cfg(feature="events")]` gating compiles on a non-events build — the whole block vanishes. Avoid a doubled `#[cfg]` if the outer `if` is already gated; structure it as ONE `#[cfg(feature="events")]` block containing the if/else.)
- [ ] **Step 2: rustfmt + commit**
```bash
rustfmt --edition 2024 crates/operax-cli/src/lib.rs
git add crates/operax-cli/src/lib.rs
git commit -m "feat(operax): host business-event router in operax serve"
```

---

## Task 7: routing e2e

**Files:** Create `crates/operax-manager/tests/event_routing_e2e.rs`.

**Interfaces:** drives `DeploymentManager::route_event` directly (a full NATS loop needs a broker; out of scope) against a real deployed pack — mirrors how `discovery_e2e.rs` used a stub.

- [ ] **Step 1: failing test** — build a manager (StubClient builder + unique temp registry, like the S1/S2 e2e/unit patterns — but this can be a plain unit-style integration test on the public API, no HTTP server needed). Deploy the tenancy pack (`examples/tenancy/handoff` via `concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/tenancy/handoff")`) as a static deployment (`sorx_url` set, tenant `demo`). Call `manager.route_event("demo", "sorla.tenancy.payment-recorded", <tenancy daily input read at runtime>, true)` and assert exactly one `RouteOutcome` with `deployment_id == <the id>` and `result.is_ok()`; assert a non-matching topic yields an empty vec.
- [ ] **Step 2: run to verify fail** — `cargo test -p operax-manager --test event_routing_e2e` → FAIL until written.
- [ ] **Step 3: complete the wiring** (inline `StubClient` impl of `operax_sorx_http::SorxClient` (6 methods, `unimplemented!()`), unique temp registry helper, `DeploymentManager::new(store, None, builder)` — no HTTP server needed since `route_event` is a direct method call).
- [ ] **Step 4: run to verify pass** — PASS.
- [ ] **Step 5: rustfmt + commit** — `test(operax): event routing over route_event with a real deployed pack`.

---

## Task 8: docs

**Files:** Modify `crates/operax-cli/README.md` (`operax serve` section).

- [ ] **Step 1:** Document **business-event routing**: when the daemon is built with `events` and `OPERAX_EVENTS_NATS_URL` is set, it subscribes to `greentic.events.>` and routes each event to every deployment whose pack `consumes` the topic (same tenant), running each (real, `dry_run=false`). Note the follow-ups: `caller_role` not yet stamped as `business-event`, environment discrimination and concurrent fan-out deferred.
- [ ] **Step 2: commit** — `docs(operax): document business-event routing for operax serve`.

---

## Self-Review

**Spec coverage:** topic_matches extract (T1) + cli delegate (T2); consumes accessor (T3); route_event fan-out (T4); daemon subscriber (T5) + hosting (T6); e2e (T7); docs (T8). ✓

**Placeholder scan:** T5/T6 have no unit test (a NATS loop needs a broker) — explicitly flagged, covered by T4's route_event test + T7's e2e; the routing logic that matters is fully unit-tested in operax-manager. All code steps carry real code.

**Type consistency:** `topic_matches(&str, &str) -> bool` identical in T1 (def), T2 (cli delegate), T4 (route_event). `EventSubscription` via `operax_core` in T3/T4. `route_event(&str, &str, Value, bool) -> Vec<RouteOutcome>` in T4, called in T5. `RouteOutcome { deployment_id, result }` in T4/T5. `run_event_router(String, Arc<DeploymentManager>)` in T5, spawned in T6.

## Execution Notes
No local cargo → CI validates (`perf` = `--all-features` builds the events router; `ci` may not). rustfmt offline before each commit. Batch CI pushes after T2, T4, T7. operax-manager must stay NATS-free — `route_event` takes `&str topic` + `Value`, never `EventEnvelope`.
