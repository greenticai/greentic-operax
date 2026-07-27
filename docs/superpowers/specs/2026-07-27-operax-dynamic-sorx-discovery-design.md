# OperaX Dynamic SoRX Discovery — Feature #4, Slice 2

- **Date:** 2026-07-27
- **Repo:** `greentic-operax`
- **Base branch:** `main` (post-S1; `operax serve` daemon merged at 5d6f407). `greentic-types 1.1`, no git-deps → **not on the release-train**.
- **Feature branch:** `feat/operax-discovery-s2`
- **Builds on:** S1 (multi-deployment daemon). Reuses the presence subscriber + directory (`presence.rs`, shipped as #9) and the `SorxClientBuilder` seam.

## Context & Problem

After S1, each deployment carries a **static `sorx_url`** supplied at deploy time; the `HttpSorxClient` is built once and frozen on the slot's `ManagerRuntime`. Meanwhile `greentic-operax` already has a **SoRX presence subscriber** (`crates/operax-cli/src/presence.rs`, `#[cfg(feature="events")]`): it subscribes to `greentic.presence.<tenant>.>` and maintains an in-memory `Directory` of `SorxPresence` announcements (`{tenant, sor, base_url, reachable, ...}`) — but it only **logs** the directory. Nothing consumes it; there is no `(tenant, sor) -> base_url` lookup, and the daemon can't use it.

**Slice 2 closes the loop:** let a deployment be **discovered** — resolve its SoRX endpoint dynamically from the presence directory by `(tenant, sor)` instead of hardcoding `sorx_url`. Resolution happens **per run** (truly dynamic: a SoRX instance that moves/restarts is picked up on the next run).

## Goals (Slice 2)

- A deployment may declare **discover mode** (`sor` set) or **static mode** (`sorx_url` set), preserving S1 static behavior unchanged.
- The `operax serve` daemon **hosts the presence subscriber** (when built with the `events` feature) and shares its `Directory` behind an `Arc<RwLock<_>>`.
- A **resolver** `(tenant, sor) -> Option<base_url>` scans the directory for the freshest **reachable** announcement.
- **`run` resolves per-invocation** in discover mode: resolve → build a fresh `HttpSorxClient` → execute against it; static mode keeps using the frozen slot client.
- Clear failure when discovery can't satisfy a run: `503`-style error.
- `operax-manager` stays **NATS-free** — the resolver is injected as a trait object; presence/NATS stays in `operax-cli` behind `events`.

## Non-Goals (deferred)

- **Environment discrimination.** `SorxPresence` carries an `environment` field; S2 resolves on `(tenant, sor)` only and picks the freshest reachable. Multi-environment disambiguation is a follow-up.
- **Health-probing / liveness checks** beyond the `reachable` flag the producer sets.
- **Load-balancing / weighted traffic** across multiple reachable instances (S2 picks one — the freshest).
- **Resolve-at-deploy caching / refresh intervals** (the chosen model is resolve-per-run; no caching layer).
- **Moving presence into `operax-manager`** or adding NATS to the daemon crate.

## Chosen Approach

**Per-run resolution via an injected `SorxResolver`, with client construction reusing the S1 `SorxClientBuilder` seam.**

Resolution timing = **per run** (decided): the semantically-correct option — a deployment tracks presence live. The frozen slot client remains the static-mode path (unchanged from S1); discover mode bypasses it.

### The four new pieces

1. **`SorxResolver` trait** (in `operax-manager`, NATS-free):
   ```rust
   pub trait SorxResolver: Send + Sync {
       fn resolve(&self, tenant: &str, sor: &str) -> Option<String>;
   }
   ```
   `DeploymentManager` gains `resolver: Option<Arc<dyn SorxResolver>>` (None when the daemon is built without `events`).

2. **Shared directory + resolve fn** (`operax-cli/src/presence.rs`): refactor `run_presence_subscriber` to accept an `Arc<RwLock<Directory>>` (today it owns a subscriber-local `HashMap` and only logs it). Add `pub fn resolve_endpoint(dir: &Directory, tenant: &str, sor: &str) -> Option<String>` — scan values, keep entries where `presence.tenant == tenant && presence.sor == sor && presence.reachable`, pick the one with the greatest `last_seen`, return its `base_url`. Make `decode_presence`/the directory type `pub` as needed.

3. **Host the subscriber in the daemon** (`operax-cli` `run_serve`, gated `events`): build `Arc<RwLock<Directory>>`, `std::thread::spawn` the presence subscriber over it (its own current-thread tokio runtime, detached), wrap the shared directory in a `PresenceResolver { dir }` implementing `SorxResolver`, and inject it into `DeploymentManager`. On a `--features events`-less build, the resolver is `None` (a `#[cfg(not(events))]` path).

4. **`ManagerRuntime::run_with_client`** (`operax-manager/src/lib.rs`): a `pub fn run_with_client(&self, input: Value, dry_run: bool, return_card: bool, client: &(dyn SorxClient + Send + Sync)) -> Result<ManagerRunResult>` that builds the `OperaxContext` from `self.{tenant, team, locale, pack}` and calls `run_loaded_pack(&self.pack, &ctx, input, dry_run, self.audit_dir.as_deref(), client)` with the **passed** client. `run_input` is refactored to delegate: `self.run_with_client(input, dry_run, return_card, self.client.as_ref())`. This is the seam that lets `run` use a freshly-resolved client without touching `ManagerRuntime`'s frozen one. (`run_loaded_pack` is already `pub` and `?Sized`-generic; `ManagerRuntime.pack` is private, hence this method rather than a raw accessor.)

### Deploy contract change

`DeploySpec` / `DeploymentRecord` / the HTTP `DeployBody`:
- `sorx_url: Option<String>` (was required)
- `sor: Option<String>` (new)

Validation at deploy/upgrade:
- Require **at least one** of `sorx_url` / `sor` (else `400 OPERAX_BAD_REQUEST`).
- If `sor` is set but `DeploymentManager.resolver` is `None` (daemon built without `events`) → `422 OPERAX_DISCOVERY_UNAVAILABLE` (fail early, don't register a deployment that can never run).
- A discover-mode slot still constructs a frozen `ManagerRuntime` client (S1 shape unchanged) using a resolved-or-placeholder URL — it is never used at run time in discover mode; the placeholder (`http://unresolved.discover`) is harmless.

### Run path

```
run(id, input, dry_run):
  read-lock slots; NotFound if absent; require (Ready, Some(rt)) else DeploymentFailed; clone Arc; drop lock
  if record.sor is Some(sor):                    # discover mode
      url = resolver?.resolve(&tenant, &sor)      # resolver guaranteed Some (checked at deploy)
              .ok_or(DeployError::Unresolved)?     # -> 503, no reachable SoRX
      client = (client_builder)(&url, token)       # fresh HttpSorxClient this run
      rt.run_with_client(input, dry_run, false, client.as_ref())
  else:                                           # static mode (S1, unchanged)
      rt.run_input(input, dry_run, false)          # frozen slot client
```

New error `DeployError::Unresolved(String)` → HTTP **503** `OPERAX_SORX_UNRESOLVED`. `OPERAX_DISCOVERY_UNAVAILABLE` (422) is a deploy-time variant when discover is requested without a resolver.

## Error / Status Additions

| Condition | Error | HTTP |
|---|---|---|
| deploy with neither `sorx_url` nor `sor` | `OPERAX_BAD_REQUEST` | 400 |
| deploy `sor` set, no resolver (no `events`) | `OPERAX_DISCOVERY_UNAVAILABLE` | 422 |
| run discover-mode, no reachable SoRX in directory | `OPERAX_SORX_UNRESOLVED` | 503 |

## Testing Strategy

- **Resolver unit** (`presence.rs`): `resolve_endpoint` picks the freshest reachable entry for `(tenant, sor)`; ignores unreachable, wrong-tenant, wrong-sor; returns `None` when nothing matches.
- **DeploymentManager discover-run** (`deployment.rs`): with a stub `SorxResolver` returning a canned URL and a stub `SorxClient`, deploy in discover mode (`sor` set, no `sorx_url`) → `run` resolves and dispatches; assert the run used the resolved client (e.g. resolver called with the deployment's tenant+sor). With a resolver returning `None` → `run` yields `Unresolved`. Deploy with neither field → validation error; deploy `sor` set + resolver `None` → `DiscoveryUnavailable`.
- **`run_with_client` parity**: a static-mode run through `run_input` and an equivalent `run_with_client` with the same client produce the same `RunReport` (guards the refactor).
- **HTTP layer** (`serve.rs`): deploy body accepting `sor` (no `sorx_url`) → 201; run on an unresolved discover deployment → 503 `OPERAX_SORX_UNRESOLVED`.
- **CI as build-oracle** (no local network): validate via a PR to `greentic-operax`. Note the daemon's presence hosting is `#[cfg(feature="events")]`; ensure the `perf`/`ci` jobs build with the features that exercise it, or add a targeted `--features events` check if the default feature set omits it (verify in the plan).

## Risks / Open Questions

- **Feature-gating friction:** presence + resolver live behind `events` in `operax-cli`; `run_serve` must gate the hosting + injection and provide a `#[cfg(not(events))]` no-op (resolver `None`). The `DeploymentManager` API (`resolver: Option<...>`) is feature-agnostic — good. Confirm CI builds the `events` path (it may need `--features events` or an operax-cli default-features check).
- **Directory freshness vs `reachable`:** S2 trusts the producer's `reachable` flag + `last_seen` recency; a crashed SoRX that never sent `reachable:false` and hasn't aged past the 300s TTL could still be picked. Acceptable for slice 2 (documented); health-probing is a non-goal.
- **`environment` ambiguity:** resolving on `(tenant, sor)` alone can pick an announcement from the wrong environment if one tenant+sor exists in several. Deferred; note in docs.
- **Placeholder frozen client in discover mode** is dead weight but isolated; an alternative (making `ManagerRuntime.client` optional) is a larger S1 change and not worth it for slice 2.
