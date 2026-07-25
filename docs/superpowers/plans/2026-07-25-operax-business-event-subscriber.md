# OperaX Business-Event Subscriber — Implementation Plan (#3 receive)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** OperaX receives SoRX-published business events over NATS and runs the deployed pack on match: a feature-gated (`events`) subscriber in `operax-cli` that wildcard-subscribes `greentic.events.<tenant>.>`, filters each `EventEnvelope` against the pack's declared `consumes`, and dispatches matches to `run_artifact_with_client` via `spawn_blocking`.

**Architecture:** core NATS (`async-nats 0.46`, at-most-once, matching the publisher); reuse `greentic_types::EventEnvelope`; mirror the reverted `operax-event-bridge`/`CoreOperaxInvoker` pattern (`git show 4ae7c76:crates/operax-cli/src/event_bridge_invoker.rs`) but targeting business-event topics. Default build gains no NATS dep.

**Tech Stack:** Rust edition 2024, `async-nats 0.46`, `tokio`, `futures` (StreamExt), `greentic-types`.

## Global Constraints

- Edition 2024; no `unwrap()`/`panic!()` in production (tests may unwrap); errors via the crate's `Result`/`OperaxError`.
- English only; Conventional Commits; the global `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>` trailer is allowed (no repo-local restriction).
- **Default build (no `events`) byte-unchanged** — gains no `async-nats`/`greentic-types` dep.
- `bash ci/local_check.sh` runs `cargo clippy --all-features -D warnings` + `cargo test --all-features` — so the `events` deps MUST compile clean under `--all-features`, and the live-NATS test MUST be `#[ignore]`d so no broker is needed.
- Reuse-first: consume `greentic_types::{EventEnvelope, parse_business_event_type}`; do NOT hand-duplicate the envelope (the reverted bridge did — do not repeat).

**Reference signatures (verified, consume as-is):**
```rust
// operax-core
pub struct EventSubscription { pub id: String, pub capability: String, pub mode: Option<String>, pub metadata: serde_json::Map<String, Value> }
// OperaHandoffMetadata { ..., pub consumes: Vec<EventSubscription> }
// operax-runtime
pub struct RunRequest { pub artifact: PathBuf, pub tenant: String, pub team: Option<String>, pub locale: Option<String>, pub caller_role: Option<String>, pub input: Value, pub dry_run: bool, pub audit_dir: Option<PathBuf> }
pub fn run_artifact_with_client<C: SorxClient>(request: RunRequest, client: &C) -> Result<RunReport>;   // loads the pack each call
// operax-sorx-http
pub trait SorxClient { /* ... */ }
pub struct HttpSorxClient; impl HttpSorxClient { pub fn new(base_url: impl Into<String>, token: Option<String>) -> Self }
// operax-pack-loader
pub fn load_operational_pack(path: impl AsRef<Path>) -> Result<OperationalPack>;   // .metadata.consumes
// greentic-types
pub struct EventEnvelope { pub id: EventId, pub topic: String, pub r#type: String, pub source: String, pub tenant: TenantCtx, pub subject: Option<String>, pub time: DateTime<Utc>, pub correlation_id: Option<String>, pub payload: Value, pub metadata: EventMetadata }
pub fn parse_business_event_type(type_str: &str) -> Result<(String, String), String>;   // expects cap://greentic/events/{domain}/{name} — EXACTLY 2 segments
```

**Publish contract (SoRX, verified):** subject `{prefix}.{tenant}.{topic}`, default prefix `greentic.events`; `topic` = `sorla.<pack>.<entity>.<op>` (entity) or `sorla.<pack>.<event>` (command), each segment sanitized keep-`[A-Za-z0-9_-]`-else-`-`; payload = JSON `EventEnvelope`.

---

### Task 1: `events` feature + optional deps

**Files:** Modify `crates/operax-cli/Cargo.toml` (+ workspace root `Cargo.toml` if deps are declared there).

- [ ] **Step 1: Add feature + deps**
```toml
# operax-cli/Cargo.toml
[features]
events = ["dep:async-nats", "dep:tokio", "dep:futures", "dep:greentic-types"]

[dependencies]
async-nats     = { version = "0.46", optional = true }
tokio          = { version = "1", features = ["rt", "macros", "sync"], optional = true }
futures        = { version = "0.3", optional = true }
greentic-types = { version = "<match the workspace's other greentic-types consumers or crates.io>", optional = true }
```
> Find the `greentic-types` version another Greentic repo pins (crates.io) and match it; if operax has no existing pin, use the latest that carries `EventEnvelope` + `parse_business_event_type`. Confirm `serde_json` is already a dep (it is — operax-core uses it).

- [ ] **Step 2: Verify both feature states resolve**
Run: `cargo check -p operax-cli --features events 2>&1 | tail -15` (fetches + compiles the deps) and `cargo check -p operax-cli 2>&1 | tail -5` (default: no new deps). Both green. Slow first time — background + WAIT.

- [ ] **Step 3: Commit**
```bash
git add crates/operax-cli/Cargo.toml Cargo.toml Cargo.lock
git commit -m "build(operax-cli): events feature + async-nats/greentic-types deps

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: The matcher (pure, unit-tested)

**Files:** Create `crates/operax-cli/src/business_events.rs` (`#[cfg(feature = "events")]`); declare `#[cfg(feature = "events")] mod business_events;` in the cli crate root (read `operax-cli/src/main.rs` / `lib.rs` to see where modules are declared).

**Interfaces:**
- Produces: `pub fn event_matches(sub: &EventSubscription, env: &EventEnvelope) -> bool` and a private `fn sanitize_segment(s: &str) -> String` + `fn expected_topics(cap: &str) -> Option<(String, String)>`.

- [ ] **Step 1: Write the failing tests**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use greentic_types::EventEnvelope; // build via a small helper or Default-ish constructor
    use operax_core::EventSubscription;

    fn sub(cap: &str) -> EventSubscription {
        EventSubscription { id: "s".into(), capability: cap.into(), mode: None, metadata: Default::default() }
    }
    fn env_with_topic(topic: &str) -> EventEnvelope { /* construct an EventEnvelope with .topic = topic; other fields dummy */ }

    #[test]
    fn matches_entity_topic() {
        // cap domain=landlord-tenant-sor name=tenant.code_generated  ↔  topic sorla.landlord-tenant-sor.tenant-code_generated
        assert!(event_matches(&sub("cap://greentic/events/landlord-tenant-sor/tenant.code_generated"),
                              &env_with_topic("sorla.landlord-tenant-sor.tenant-code_generated")));
    }
    #[test]
    fn does_not_match_wrong_name() {
        assert!(!event_matches(&sub("cap://greentic/events/landlord/tenant.created"),
                               &env_with_topic("sorla.landlord.other-event")));
    }
    #[test]
    fn ignores_non_business_event_cap() {
        assert!(!event_matches(&sub("cap://greentic/business-functions/x/y"),
                               &env_with_topic("sorla.x.y")));
    }
}
```
> Determine how to construct an `EventEnvelope` in tests (Default, a `new`, or struct literal) by reading `greentic-types/src/events.rs`; build a small `env_with_topic` helper. Determine the exact cap segment shape that OperaX `operala.yaml` `consumes[].capability` actually uses (the operax-core test fixture uses `cap://greentic/events/boiler-maintenance/v1/work-order-assigned` — a `/v1/` version segment; the SoRX-side cap is `cap://greentic/events/{pack}/{event}` with no version). Your `expected_topics`/parse MUST handle the real operala shape — do NOT blindly rely on `greentic_types::parse_business_event_type` (it requires exactly 2 post-prefix segments and would reject a `/v1/` cap). Parse defensively: strip the `cap://greentic/events/` prefix, treat the LAST segment as `name` and the segment(s) before it (minus an optional `vN` version) as `domain`. Add a test for the `/v1/` form once you confirm it.

- [ ] **Step 2: Run to verify fail**
`cargo test -p operax-cli --features events business_events 2>&1 | tail -20` → FAIL (compile: `event_matches` missing).

- [ ] **Step 3: Implement**
```rust
use greentic_types::EventEnvelope;
use operax_core::EventSubscription;

const EVENTS_CAP_PREFIX: &str = "cap://greentic/events/";

/// Keep `[A-Za-z0-9_-]`, map every other char (incl. `.`) to `-` — mirrors SoRX's topic-segment
/// sanitization so a declared cap resolves to the topic SoRX actually publishes.
fn sanitize_segment(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' }).collect()
}

/// Parse an events cap into `(domain, name)`, tolerating an optional `vN` version segment
/// (`cap://greentic/events/<domain>/[v1/]<name>`). Returns None for non-events caps.
fn cap_domain_name(cap: &str) -> Option<(String, String)> {
    let rest = cap.strip_prefix(EVENTS_CAP_PREFIX)?;
    let mut segs: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() < 2 { return None; }
    let name = segs.pop().unwrap();
    // drop a trailing version segment if present (e.g. ".../v1/name")
    if segs.last().is_some_and(|s| s.len() >= 2 && s.starts_with('v') && s[1..].chars().all(|c| c.is_ascii_digit())) {
        segs.pop();
    }
    let domain = segs.join("-");
    Some((domain, name.to_string()))
}

pub fn event_matches(sub: &EventSubscription, env: &EventEnvelope) -> bool {
    let Some((domain, name)) = cap_domain_name(&sub.capability) else { return false };
    let suffix = format!("{}.{}", sanitize_segment(&domain), sanitize_segment(&name));
    env.topic == format!("sorla.{suffix}") || env.topic.ends_with(&format!(".{suffix}"))
}
```

- [ ] **Step 4: Run to verify pass**
`cargo test -p operax-cli --features events business_events 2>&1 | tail -20` → PASS. Add the `/v1/` cap test now that the parser is in.

- [ ] **Step 5: Commit**
```bash
git add crates/operax-cli/src/business_events.rs crates/operax-cli/src/main.rs
git commit -m "feat(operax-cli): business-event cap↔topic matcher

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Subscriber loop + routing dispatch

**Files:** Modify `crates/operax-cli/src/business_events.rs` (add the subscriber + a pure routing helper + a fake-client routing test).

**Interfaces:**
- Consumes: `event_matches` (Task 2); `operax_runtime::{RunRequest, run_artifact_with_client, RunReport}`; `operax_sorx_http::{SorxClient, HttpSorxClient}`; `operax_pack_loader::load_operational_pack`.
- Produces: `pub struct SubscriberConfig { pub nats_url: String, pub tenant: String, pub artifact: PathBuf, pub sorx_base_url: String, pub sorx_token: Option<String> }`; `pub fn run_subscriber(config: SubscriberConfig) -> anyhow::Result<()>`; and a pure `fn request_for(env: &EventEnvelope, sub: &EventSubscription, artifact: &Path) -> RunRequest`.

- [ ] **Step 1: Write the failing routing test**
```rust
#[test]
fn routes_matching_event_to_run_request() {
    let env = env_with_topic("sorla.landlord.tenant-created"); // give it a payload too
    let s = sub("cap://greentic/events/landlord/tenant.created");
    let req = request_for(&env, &s, std::path::Path::new("/x.gtpack"));
    assert_eq!(req.tenant, /* env.tenant’s tenant string */);
    assert_eq!(req.input, env.payload);
    assert_eq!(req.caller_role.as_deref(), Some("business-event"));
    assert!(!req.dry_run);
}
```
> Confirm the `TenantCtx` field that holds the tenant string (`env.tenant.tenant` or `.tenant_id`) by reading `greentic-types/src/events.rs`; use it in `request_for` and the assert.

- [ ] **Step 2: Verify fail** → `request_for` missing.

- [ ] **Step 3: Implement**
```rust
use std::path::{Path, PathBuf};
use futures::StreamExt;
use operax_runtime::{RunRequest, run_artifact_with_client};
use operax_sorx_http::HttpSorxClient;
use operax_pack_loader::load_operational_pack;

pub struct SubscriberConfig {
    pub nats_url: String,
    pub tenant: String,
    pub artifact: PathBuf,
    pub sorx_base_url: String,
    pub sorx_token: Option<String>,
}

fn request_for(env: &EventEnvelope, _sub: &EventSubscription, artifact: &Path) -> RunRequest {
    RunRequest {
        artifact: artifact.to_path_buf(),
        tenant: env.tenant.tenant.clone(), // confirm field name
        team: None,
        locale: None,
        caller_role: Some("business-event".to_string()),
        input: env.payload.clone(),
        dry_run: false,
        audit_dir: None,
    }
}

/// Runs the NATS subscription loop until the connection ends. Blocking: spins its own
/// current-thread tokio runtime (the caller runs this on a dedicated thread).
pub fn run_subscriber(config: SubscriberConfig) -> anyhow::Result<()> {
    let pack = load_operational_pack(&config.artifact)?;
    let subscriptions = pack.metadata.consumes.clone();
    if subscriptions.is_empty() {
        eprintln!("no `consumes` subscriptions declared in {}; nothing to subscribe", config.artifact.display());
        return Ok(());
    }
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async move {
        let client = async_nats::connect(&config.nats_url).await?;
        let subject = format!("greentic.events.{}.>", config.tenant);
        let mut sub = client.subscribe(subject.clone()).await?;
        eprintln!("subscribed to {subject}");
        while let Some(msg) = sub.next().await {
            let env: EventEnvelope = match serde_json::from_slice(&msg.payload) {
                Ok(env) => env,
                Err(err) => { eprintln!("skip undecodable event on {}: {err}", msg.subject); continue; }
            };
            for subscription in &subscriptions {
                if !event_matches(subscription, &env) { continue; }
                let request = request_for(&env, subscription, &config.artifact);
                let sorx = HttpSorxClient::new(config.sorx_base_url.clone(), config.sorx_token.clone());
                match tokio::task::spawn_blocking(move || run_artifact_with_client(request, &sorx)).await {
                    Ok(Ok(report)) => eprintln!("ran {} for {}: {:?}", config.artifact.display(), env.topic, report_outcome(&report)),
                    Ok(Err(err)) => eprintln!("run failed for {}: {err}", env.topic),
                    Err(join) => eprintln!("run task panicked for {}: {join}", env.topic),
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    })
}
```
Add a small `fn report_outcome(report: &RunReport) -> &'static str` mirroring the reverted `outcome_from_report` (ok/failed based on `report.failed_operation.is_none()` — confirm the `RunReport` field).

- [ ] **Step 4: Verify pass** → `cargo test -p operax-cli --features events business_events 2>&1 | tail` PASS (routing unit test; the subscriber loop itself isn't unit-tested here — a live-NATS test is Task 5-adjacent / ignored).

- [ ] **Step 5: Commit**
```bash
git add crates/operax-cli/src/business_events.rs
git commit -m "feat(operax-cli): NATS business-event subscriber + dispatch to run_artifact

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: CLI subcommand `events subscribe`

**Files:** Modify `crates/operax-cli/src/main.rs` (manual arg dispatch — read it first to match the existing pattern).

- [ ] **Step 1: Wire the subcommand**
Read `operax-cli/src/main.rs` to see how subcommands are parsed/dispatched (it is `fn main() -> ExitCode` with manual arg handling, not clap-derive). Add an `events subscribe` path (feature-gated) that reads `--artifact`, `--tenant`, `--sorx-url`, optional `--nats-url` (default env `OPERAX_EVENTS_NATS_URL`), optional `--sorx-token`, builds `SubscriberConfig`, and calls `business_events::run_subscriber(config)` on a dedicated `std::thread` (or directly — it blocks). Under `#[cfg(not(feature = "events"))]`, the subcommand prints a "built without the `events` feature" error and exits non-zero. Keep the default (no-`events`) `main` behavior byte-identical.

- [ ] **Step 2: Verify**
`cargo build -p operax-cli` (default — unchanged) and `cargo build -p operax-cli --features events`. Both green. Manually confirm `operax events subscribe --help`-ish path exists (or the arg dispatch handles it).

- [ ] **Step 3: Commit**
```bash
git add crates/operax-cli/src/main.rs
git commit -m "feat(operax-cli): `events subscribe` subcommand

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Docs + gate + PR

- [ ] **Step 1: Docs**
Create/append `docs/business-events.md` (or the repo's docs convention): how a bundle declares `consumes:`, how to run `operax events subscribe --artifact <pack> --tenant <t> --sorx-url <url>` (feature `events`), the wildcard-subscribe + filter-on-receipt model + at-most-once semantics, and the known `greentic_types::validate_business_event` topic-vs-domain drift (the subscriber matches SoRX's actual hierarchical topic; reconciling the canonical validator is a `greentic-types` follow-up).

- [ ] **Step 2: Gate**
```bash
cargo fmt --all -- --check
cargo clippy -p operax-cli --all-targets -- -D warnings
cargo clippy -p operax-cli --all-targets --features events -- -D warnings
cargo test -p operax-cli                      # default, unchanged
cargo test -p operax-cli --features events     # matcher + routing tests green; live-NATS test #[ignore]d
```
If time permits, run `bash ci/local_check.sh` (it uses `--all-features`; ensure the live-NATS test is `#[ignore]`d so no broker is needed).

- [ ] **Step 3: Push + PR into main**
```bash
git push -u origin feat/business-event-subscriber
gh pr create --base main --head feat/business-event-subscriber \
  --title "feat(operax): business-event subscriber (SoRX #3 receive half)" \
  --body "..."   # summarize: feature-gated NATS subscriber wildcard-subscribes greentic.events.<tenant>.>, filters EventEnvelope against pack.consumes, dispatches matches to run_artifact_with_client via spawn_blocking; reuses greentic-types EventEnvelope; core NATS at-most-once; matcher unit-tested; live-NATS test ignored; note the validate_business_event topic drift as a greentic-types follow-up.
```
