# OperaX business-event subscriber — design (#3 subscribe/deliver)

_Date: 2026-07-25 · Repo: `greentic-operax` · Branch: `feat/business-event-subscriber` (off `main`)_

## Context

SoRLa/SoRX epic item **#3**: env packs/bundles should dynamically discover, interact with, and
**subscribe to** deployed SoRLa instances. Discover + interact + publish are done; the **receive**
half is missing: SoRX publishes business events to NATS (subject `<prefix>.<tenant>.<topic>`,
default prefix `greentic.events`, topic e.g. `sorla.<pack>.<entity>.<op>`, JSON
`greentic_types::EventEnvelope`) — but nothing consumes them. OperaX bundles declare
`consumes: Vec<EventSubscription>` in `operala.yaml` (`OperaHandoffMetadata.consumes`,
`operax-core/src/lib.rs:134,162-170`), which is parsed + validated (`operax-pack-loader/src/lib.rs:226`)
and then **never acted on** — no NATS subscriber, no delivery.

This builds an in-repo NATS subscriber in OperaX that receives SoRX-published business events and runs
the declared bundle on match. Chosen model (with the user): **wildcard subscribe + filter-on-receipt,
core NATS** (at-most-once, matching the publisher).

## Goal

1. OperaX subscribes to `<prefix>.<tenant>.>` (core NATS), deserializes each `EventEnvelope`, and for
   each declared `EventSubscription` that matches the envelope, runs the deployed pack's artifact via
   the existing `run_artifact_with_client` seam, mapping `envelope.payload` → `RunRequest.input`.
2. Feature-gated (`events`) so the default OperaX build/CI is unchanged and gains no `async-nats` dep.

## Non-goals

- No JetStream / durability / replay (publisher uses core NATS; at-most-once is symmetric). Deferred.
- No change to SoRX (publisher is done on sorx `research`). No new discovery/self-register.
- No cap→exact-subject resolution (we subscribe broad and filter on receipt — avoids the cap-URI ↔
  topic drift, see below).
- No live-broker integration test in the default gate (a NATS round-trip test is gated/ignored).

## Architecture

### Dependencies (feature-gated)

Add an `events` feature (opt-in, default off). The subscriber lives in `operax-cli` (which already
wires `operax-runtime` + `operax-sorx-http`), mirroring the **reverted** `operax-event-bridge` /
`CoreOperaxInvoker` precedent (`git show 4ae7c76:crates/operax-cli/src/event_bridge_invoker.rs`) —
but targeting business-event topics instead of the `operala.request.v1` RPC.

```toml
# operax-cli/Cargo.toml
[features]
events = ["dep:async-nats", "dep:tokio", "dep:futures", "dep:greentic-types"]

[dependencies]
async-nats  = { version = "0.46", optional = true }   # match sorx; core NATS, no JetStream
tokio       = { version = "1", features = ["rt-multi-thread","macros"], optional = true }
futures     = { version = "0.3", optional = true }     # StreamExt for the subscription
greentic-types = { version = "...", optional = true }   # EventEnvelope + parse_business_event_type (REUSE — do not hand-duplicate)
```
Reuse-first: consume `greentic_types::{EventEnvelope, parse_business_event_type}` rather than
re-declaring the envelope (the reverted bridge duplicated structs — do NOT repeat that).

### The subscriber (`operax-cli`, `#[cfg(feature = "events")]`)

`BusinessEventSubscriber::run(config)` — a dedicated OS thread + single-thread tokio runtime (mirroring
the reverted precedent + sorx's `NatsEventSink`), so the sync `run_artifact_with_client` never runs on
the async reactor:

1. `async_nats::connect(nats_url)`.
2. Resolve `subscriptions: Vec<EventSubscription>` + `artifact: PathBuf` + `tenant: String` from the
   deployed pack (load its `operala.yaml` via `operax-pack-loader`, read `metadata.consumes`).
3. `client.subscribe(format!("{prefix}.{tenant}.>"))` — one wildcard subscription for the tenant.
4. For each message: `serde_json::from_slice::<EventEnvelope>(&msg.payload)` (on error: log + skip,
   never crash the loop); for each `subscription` where `event_matches(subscription, &envelope)`:
   `tokio::task::spawn_blocking(move || run_artifact_with_client(request, &HttpSorxClient))`, where
   `request = RunRequest { artifact, tenant: envelope.tenant.tenant, team: <default/None>, input:
   envelope.payload.clone(), caller_role: Some("business-event".into()), dry_run: false, .. }`. Log the
   `RunReport` outcome (mirror the reverted `outcome_from_report`).

### The matcher (pure, unit-tested)

`event_matches(sub: &EventSubscription, env: &EventEnvelope) -> bool`:
- Parse `sub.capability` (`cap://greentic/events/<domain>/<name>`) via
  `greentic_types::parse_business_event_type` → `(domain, name)`. Non-business-event caps → no match.
- Normalize `domain`/`name` with the SAME sanitization SoRX applies to topic segments (keep
  `[A-Za-z0-9_-]`, map every other char — including `.` — to `-`); this is ~8 lines, vendored locally
  (do not depend on `greentic-sorx-core`'s private `topic_segment`).
- Match iff `env.topic == format!("sorla.{sanitized_domain}.{sanitized_name}")` OR `env.topic` ends
  with `.{sanitized_domain}.{sanitized_name}` (tolerates a differing `sorla.` prefix). This is the
  deterministic contract between SoRX's `entity_event_topic`/`command_event_topic` and the declared
  cap subscriptions.

> **Known drift (documented, not resolved here):** `greentic_types::validate_business_event` asserts
> `env.topic == domain`, but SoRX emits a hierarchical topic (`sorla.<pack>.<entity>.<op>`), so SoRX
> envelopes fail that canonical validator. The matcher above deliberately matches SoRX's ACTUAL topic,
> not the stricter canonical rule. Reconciling `validate_business_event` with SoRX's topic scheme is a
> separate `greentic-types` follow-up; note it in the PR.

### Lifecycle / start

An `operax-cli` subcommand (feature-gated), e.g. `operax events subscribe --artifact <pack.gtpack>
--tenant <t>` with the NATS URL from `--nats-url` or `OPERAX_EVENTS_NATS_URL`. Runs the subscriber loop
until interrupted. (Integrating it into `operax serve`/manager auto-start on deploy is a follow-up; the
subcommand is the phase-1 surface.)

## Testing

- **Matcher unit tests** (pure, no NATS, default-buildable-under-`events`): matching cap↔topic
  (entity + command topics), non-match (wrong domain/name), non-business-event cap → no match, the
  dot→dash normalization case (`tenant.code_generated` ↔ `tenant-code_generated`).
- **Envelope-routing unit test**: feed a decoded `EventEnvelope` + a `consumes` list to the routing
  function and assert it builds the expected `RunRequest` (tenant/input) for matches and nothing for
  non-matches — using a fake `SorxClient` so no real HTTP/NATS runs.
- **Live NATS round-trip**: a `#[ignore]`d / feature-gated integration test (needs a `nats-server`),
  mirroring sorx's gated NATS test — publish an envelope, assert the subscriber routes it. Not in the
  default gate.

## Files touched

- `greentic-operax` — `operax-cli/Cargo.toml` (`events` feature + optional deps); new
  `operax-cli/src/business_events.rs` (`#[cfg(feature="events")]`: subscriber + matcher + routing);
  `operax-cli` command wiring for `events subscribe`; docs. `operax-core`/`runtime`/`sorx-http`
  unchanged (consumed as-is).

## Global constraints (from this repo)

- Rust 1.95 target, edition 2024; no `unwrap()`/`panic!()` in production (tests may unwrap); errors via
  the crate's `Result`/error types.
- English only; Conventional Commits. No repo-local Claude-attribution restriction found → the global
  co-author trailer is allowed.
- `bash ci/local_check.sh` before done (fmt + i18n + clippy `--all-features -D warnings` + test
  `--all-features` + build + doc + package dry-run). Because `--all-features` activates `events`, the
  new deps MUST compile cleanly under it; the live-NATS test must be `#[ignore]`d so
  `cargo test --all-features` doesn't require a broker.
- Default build (no `events`) gains no `async-nats`/`greentic-types` dep and is byte-unchanged.
