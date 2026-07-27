# OperaX Business-Event → Deployment Routing — Feature #4, Slice 3

- **Date:** 2026-07-27
- **Repo:** `greentic-operax`
- **Base branch:** `main` (post-S1+S2; `operax serve` daemon + dynamic discovery merged at 3bb40ed). `greentic-types 1.1`, no git-deps → **not on the release-train**.
- **Feature branch:** `feat/operax-event-routing-s3`
- **Builds on:** S1 (daemon + `DeploymentManager::run`), S2 (discover run path), and the #8 single-pack business-event subscriber (`business_events.rs`).

## Context & Problem

`greentic-operax` already has a **single-pack** business-event subscriber (`crates/operax-cli/src/business_events.rs`, `#[cfg(feature="events")]`): it subscribes to `greentic.events.<tenant>.>`, decodes a `greentic_types::EventEnvelope`, matches the event's `topic` against ONE pack's `consumes` list (`event_matches`), and runs that pack. But the `operax serve` daemon manages **N deployments** and never consults `consumes` — so a business event emitted by a pack/bundle cannot fan out to the matching operala deployments.

**Slice 3 closes audit item #3** ("triggered by packs/bundles knowing a business event"): the daemon hosts a business-event subscriber that, on each incoming event, routes it to **every deployment in the registry whose pack `consumes` the event's topic** (same tenant), running each.

## Goals (Slice 3)

- The `operax serve` daemon (events build) **hosts a business-event subscriber** that subscribes to `greentic.events.>` (all tenants) and routes each event into the deployment registry.
- A pure, NATS-free **`DeploymentManager::route_event(tenant, topic, input, dry_run)`** fans an event across matching deployments (`record.tenant == tenant && Ready && pack.consumes matches topic`), running each via the existing S1/S2 `run` path, and returns a per-deployment outcome list.
- The cap→topic matcher (`event_matches`/`cap_domain_name`/`sanitize_segment`) is **extracted to a pure `operax_core::topic_matches(capability, topic) -> bool`** so the NATS-free manager can match without depending on `greentic-types`/`EventEnvelope`. The #8 subscriber reuses it (no behavior change).
- `operax-manager` stays **NATS-free** — the subscriber (operax-cli, events) holds an `Arc<DeploymentManager>` and calls `route_event` over `&str` topic + `Value` payload.

## Non-Goals (deferred)

- **`caller_role="business-event"` fidelity** (decided): routed runs use the deployment's normal context (`caller_role="service"`), same as a manual `/run`. Audit/guardrail can't yet distinguish event-triggered runs. Follow-up.
- **Environment discrimination.** `EventEnvelope.tenant` carries an `EnvId`; routing matches on tenant only (all environments), mirroring S2.
- **Concurrent fan-out.** Matched deployments run sequentially with per-deployment logging (event volume is low; fire-and-forget). Concurrency is a follow-up.
- **Delivery guarantees / retries.** Business events are fire-and-forget over NATS; a routed-run failure is logged, not retried or propagated (there is no HTTP caller).
- **Replacing the #8 single-pack `events subscribe` CLI command** — it stays for back-compat.

## Chosen Approach

Five additive pieces; the single-pack subscriber and the S1/S2 run path are reused.

### 1. `operax_core::topic_matches` (extract the matcher)

Move `EVENTS_CAP_PREFIX`, `sanitize_segment`, `cap_domain_name`, and the matching logic from `business_events.rs` into `operax-core` (which already owns `EventSubscription`), exposed as:
```rust
pub fn topic_matches(capability: &str, topic: &str) -> bool
```
(The current `event_matches(sub, env)` uses only `sub.capability` + `env.topic`, so this is a mechanical extraction.) `business_events.rs::event_matches(sub, env)` becomes `operax_core::topic_matches(&sub.capability, &env.topic)` — its unit tests move/stay green.

### 2. `ManagerRuntime::consumes` accessor

```rust
pub fn consumes(&self) -> &[operax_core::EventSubscription]  // returns &self.pack.metadata.consumes
```
Exposes the private `pack.metadata.consumes` so the manager can read a deployment's subscriptions live from its active pack (no persistence duplication, always in sync with the active version).

### 3. `DeploymentManager::route_event` (NATS-free fan-out)

```rust
pub struct RouteOutcome {
    pub deployment_id: String,
    pub result: Result<crate::ManagerRunResult, DeployError>,
}

pub fn route_event(&self, tenant: &str, topic: &str, input: serde_json::Value, dry_run: bool) -> Vec<RouteOutcome>
```
- Read-lock `slots`; collect the ids of slots where `record.tenant == tenant`, `status == Ready`, `runtime` is `Some`, and any `runtime.consumes()` entry satisfies `operax_core::topic_matches(&sub.capability, topic)`.
- **Drop the lock**, then for each matched id call `self.run(id, input.clone(), dry_run)` (the S1/S2 static+discover path) and collect `RouteOutcome { deployment_id, result }`.
- Never panics; a poisoned slots lock yields an empty `Vec` (logged by the caller). An empty result (no matches) is normal.

### 4. `run_event_router` (daemon subscriber)

New function in `business_events.rs` (events-gated), mirroring `run_subscriber`'s loop but registry-wide:
```rust
pub fn run_event_router(nats_url: String, manager: std::sync::Arc<operax_manager::deployment::DeploymentManager>) -> anyhow::Result<()>
```
- Own current-thread tokio runtime; `async_nats::connect`; subscribe **`greentic.events.>`** (all tenants).
- Per message: decode `EventEnvelope` (skip-and-log on error); call `manager.route_event(&env.tenant.tenant.to_string(), &env.topic, env.payload, false)`; log each `RouteOutcome` ("routed event `<topic>` → deployment `<id>`: ok/failed").
- The #8 single-pack `run_subscriber` is untouched (still used by `operax events subscribe`).

### 5. Host in `run_serve`

After `let manager = Arc::new(manager);` in `run_serve` (events-gated), if `OPERAX_EVENTS_NATS_URL` is set, `std::thread::spawn` a thread running `run_event_router(nats_url, manager.clone())`; else log "event routing inert". Mirrors the S2 presence-hosting block. `DeploymentManager` is already `Arc`-shared + `Send+Sync`.

## Error / Logging

- `route_event` returns per-deployment `Result`s; the subscriber logs each (ok/failed) — no HTTP status (fire-and-forget). A run that fails (e.g. discover `Unresolved`, or pack error) is logged and does not stop routing to other matched deployments.
- Decode failures skip-and-log (as #8). Lock poison → empty route + log.

## Testing Strategy

- **`operax_core::topic_matches` unit** (moved from `business_events.rs`): the existing entity/command-form + cross-pack-non-match cases, now over `(capability, topic)` strings.
- **`DeploymentManager::route_event` unit** (`deployment.rs`): deploy two deployments with fixture packs whose `consumes` differ (one matches `topic`, one doesn't) + a third for a different tenant; `route_event(tenant, topic, payload, dry_run=true)` returns exactly the one matching id with an `Ok` result; a topic matching nothing → empty `Vec`; a wrong-tenant match → excluded. (Use the tenancy fixture pack, which declares `consumes`; if it doesn't, add a small handoff fixture that does — verify what `examples/tenancy/handoff` declares.)
- **`consumes` accessor**: a deployed runtime returns its pack's `consumes`.
- **e2e** (`tests/event_routing_e2e.rs`): spawn the daemon; deploy a `consumes`-declaring pack; call `route_event` through a thin path (or directly on the manager, since the NATS subscriber needs a broker) asserting the deployment runs. (A full NATS e2e needs a broker — out of scope; the e2e drives `route_event` directly, mirroring how the discovery e2e used a stub resolver.)
- **CI as build-oracle** (no local network): `perf` = `--all-features` builds the events-gated router; validate via PR.

## Risks / Open Questions

- **Fixture `consumes`:** RESOLVED — `examples/tenancy/handoff/operala.yaml` (the loader reads metadata from `operala.yaml`) declares `consumes[0].capability = cap://greentic/events/tenancy/v1/payment-recorded`, which `topic_matches` maps to topic `sorla.tenancy.payment-recorded`. The `route_event` tests reuse this fixture: a matching topic is `"sorla.tenancy.payment-recorded"`, a non-matching one is anything else. No new fixture needed.
- **`topic_matches` extraction parity:** the move to operax-core must preserve behavior exactly (entity + command tails, version-segment tolerance, cross-pack non-match). The moved unit tests guard this.
- **Sequential fan-out latency:** a slow SoRX for one matched deployment delays routing to the next. Acceptable for slice 3 (low event volume); concurrency is a follow-up.
- **`caller_role` deferral:** routed runs are indistinguishable from manual runs in audit/policy until the follow-up threads `caller_role="business-event"`.
- **operax-manager NATS-freeness preserved:** `route_event`'s signature is `&str` topic + `Value` payload — no `EventEnvelope`/greentic-types leak into operax-manager. The subscriber does all `EventEnvelope` decoding in operax-cli.
