# OperaX Hardening — reconnect · caller_role · concurrent fan-out · environment discrimination

- **Date:** 2026-07-28
- **Repo:** `greentic-operax`
- **Base branch:** `main` (post feature #4 S1+S2+S3, merged at cd1a3e2). `greentic-types 1.1`, no git-deps → **not on the release-train**.
- **Feature branch:** `feat/operax-hardening`
- **Builds on:** S1 daemon, S2 discovery, S3 event-routing. Addresses the four deferred follow-ups from those slices' final reviews.

## Context & Problem

The three shipped `operax serve` slices left four deferred follow-ups, all real robustness/fidelity gaps:

1. **NATS subscribers die silently on disconnect.** `run_presence_subscriber` (S2), `run_event_router` (S3), and `run_subscriber` (#8) each `block_on(connect → subscribe → while-let → Ok)`; when the stream ends (NATS disconnect), the loop returns and the thread exits — routing/discovery stays permanently dead with no reconnect.
2. **Routed runs are indistinguishable from manual runs** in audit/policy — `DeploymentManager::run` builds `OperaxContext` with the default `caller_role="service"`; the #8 path stamped `"business-event"`.
3. **Event fan-out is sequential** — `route_event` runs matched deployments one at a time; a slow SoRX for one delays the rest.
4. **Routing/discovery ignore environment** — a deployment matches events / resolves SoRX by `tenant` (and `sor`) only, ignoring `EventEnvelope`'s `EnvId` and `SorxPresence.environment`, so a tenant present in multiple environments can cross-wire.

## Goals

- **Reconnect:** all three subscribers reconnect with capped exponential backoff instead of exiting on disconnect.
- **caller_role:** event-routed runs stamp `caller_role="business-event"`; HTTP/manual runs keep `"service"`.
- **Concurrent fan-out:** the event router runs matched deployments concurrently.
- **Environment discrimination (opt-in, back-compat):** a deployment may declare an `environment`; discovery + routing then respect it. `environment: None` = wildcard (matches all environments — the current behavior), so existing deployments are unaffected.

## Non-Goals

- Reworking the deploy contract beyond the additive `environment` field.
- Multi-broker / clustered NATS, delivery guarantees, or dedup.
- Per-deployment backoff tuning (one fixed policy).

## Chosen Approach

### 1. NATS reconnect

Add a small shared helper (operax-cli, events-gated), e.g. in a new `nats_reconnect` module:
```rust
/// Run `session` (connect + subscribe + consume loop) repeatedly, sleeping with
/// capped exponential backoff between attempts. Never returns Ok — it loops forever
/// (the daemon thread owns it); returns Err only on an unrecoverable setup error.
pub async fn run_with_reconnect<F, Fut>(mut session: F)
where F: FnMut() -> Fut, Fut: std::future::Future<Output = anyhow::Result<()>>
```
Backoff: start 1s, double to a 30s cap, reset to 1s after a session that ran ≥ some threshold (or simply reset on each new successful connect). Each subscriber's `block_on` body becomes: hoist the invariant setup (e.g. `run_subscriber`'s one-time pack load stays outside), then `run_with_reconnect(|| async { connect; subscribe; while-let; Ok(()) }).await`. On a session returning (stream ended) or erroring, log and sleep-then-retry. Apply to `run_presence_subscriber`, `run_event_router`, `run_subscriber`.

### 2. caller_role (additive, uses the existing `OperaxContext::with_caller_role`)

`OperaxContext::with_caller_role(self, impl Into<String>) -> Result<Self>` already exists. Thread an `Option<&str>` down without changing existing signatures:
- `ManagerRuntime::run_with_client_as(&self, input, dry_run, return_card, caller_role: Option<&str>, client) -> Result<ManagerRunResult>` — builds `OperaxContext::new(...)?` then `let ctx = match caller_role { Some(r) => ctx.with_caller_role(r)?, None => ctx };`. Existing `run_with_client` delegates with `None`; `run_input` unchanged (still delegates to `run_with_client`).
- `DeploymentManager::run_as(&self, id, input, dry_run, caller_role: Option<&str>) -> Result<ManagerRunResult, DeployError>`; existing `run` delegates with `None` (so `serve.rs`'s HTTP `/run` stays `"service"`).
- The routing path (below) calls `run_as(..., Some("business-event"))`.

### 3. Concurrent fan-out

- Extract `DeploymentManager::matching_deployments(&self, event_env: &str, tenant: &str, topic: &str) -> Vec<String>` = the phase-1 match (read-lock, filter by tenant + environment + `Ready` + `consumes`/`topic_matches`, collect ids, drop lock).
- `route_event` stays as a sequential convenience method (keeps the existing unit tests + their ordering assertions valid), now delegating its match to `matching_deployments` and its run to `run_as(..., Some("business-event"))`.
- `run_event_router` (which holds `Arc<DeploymentManager>`) changes from one `spawn_blocking(route_event)` to: `matching_deployments(...)` → for each id, `tokio::task::spawn_blocking({ let mgr = manager.clone(); move || mgr.run_as(&id, input.clone(), false, Some("business-event")) })`, then `futures::future::join_all` the handles and log each outcome. `run` is safe to call concurrently on an `Arc<DeploymentManager>` (read-lock is brief + dropped before the SoRX call; per-runtime state is behind a `Mutex`).

### 4. Environment discrimination (opt-in, back-compat)

- **Deploy contract:** add `environment: Option<String>` (`#[serde(default)]` on `DeploymentRecord`) to `DeploySpec` + `DeploymentRecord`; thread `sor`-style through `deploy`/`upgrade`; update the 3 test fixtures + the `deployment_store` roundtrip test literal.
- **Resolver trait (breaking, intentional):** `SorxResolver::resolve(&self, env: Option<&str>, tenant: &str, sor: &str) -> Option<String>`; `resolve_endpoint(dir, env: Option<&str>, tenant, sor)` adds the filter `env.is_none_or(|e| entry.presence.environment == e)` (so `env: None` = don't filter on environment — wildcard); update `PresenceResolver::resolve` + the `StubResolver`/`presence_resolver_delegates` tests.
- **Discover run:** `run`/`run_as` (discover branch) calls `resolver.resolve(record.environment.as_deref(), &tenant, &sor)`.
- **Routing filter:** `matching_deployments` takes `event_env: &str`; the per-slot predicate gains `record.environment.as_deref().is_none_or(|e| e == event_env)` (a deployment with no environment matches any event env — wildcard; a deployment with an environment only matches its own env). `run_event_router` sources `event_env` from the decoded `EventEnvelope` (`env.tenant.env` → `.as_str()`).

## Semantics summary (environment)

| Deployment `environment` | Event env / SoRX env | Match? |
|---|---|---|
| `None` (wildcard) | anything | ✅ (unchanged behavior) |
| `Some("prod")` | `"prod"` | ✅ |
| `Some("prod")` | `"staging"` | ❌ (routing skips; discovery won't resolve a staging SoRX for it) |

## Testing Strategy

- **Reconnect:** unit-test the `run_with_reconnect` combinator with a `session` closure that returns after N iterations and a fake backoff (assert it retries + the backoff sequence caps); the live-NATS loops themselves remain covered only by compilation (no broker). Keep the backoff pure/injectable so it's testable without sleeping.
- **caller_role:** unit-test that `run_as(..., Some("business-event"))` produces a `RunReport`/context whose caller_role is `"business-event"` (if the report/audit exposes it) or, minimally, that `run_with_client_as` with `Some` vs `None` builds the ctx accordingly (a small ctx-inspection test).
- **Concurrent fan-out:** `route_event`'s existing sequential test stays; add a `matching_deployments` unit test (env + tenant + topic filtering). The concurrency itself lives in the CLI router (no unit test — covered by compilation + the route logic).
- **Environment:** `resolve_endpoint` with `env: Some/None` (filters vs wildcard); `matching_deployments` with a `Some("prod")` deployment vs a `None` deployment against `"prod"`/`"staging"` events; deploy/serde roundtrip of the new field.
- **e2e:** extend or add an e2e that deploys with `environment: Some("prod")` and routes a `"prod"` event (matches) vs a `"staging"` event (skips).
- **CI as build-oracle** (no local network): `perf` = `--all-features` builds the events-gated reconnect/router; validate via PR.

## Risks / Notes

- **Resolver trait break** is the load-bearing change; it ripples to `PresenceResolver`, `StubResolver`, `resolve_endpoint`, and the discover call site — all updated in the same slice.
- **Registry back-compat:** `#[serde(default)]` on `DeploymentRecord.environment` so existing on-disk registries (no `environment` key) load as `None` (wildcard) — no migration.
- **Reconnect must never busy-loop:** the backoff sleep is mandatory on every retry; a session that fails to even connect must still sleep before retrying.
- **`join_all` in the router** collects `JoinHandle`s; a panicked run task surfaces as a `JoinError` — log it, don't propagate (fire-and-forget).
