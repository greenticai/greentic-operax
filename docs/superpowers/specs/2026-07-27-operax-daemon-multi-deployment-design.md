# OperaX Daemon — Multi-Deployment Registry + Lifecycle (Feature #4, Slice 1)

- **Date:** 2026-07-27
- **Repo:** `greentic-operax`
- **Base branch:** `main` (operax advanced line; `greentic-types = 1.1` registry pin, sibling `greentic-pack` path dep, **no git-deps → not on the release-train**)
- **Feature branch:** `feat/operax-daemon-multi-deployment`
- **Epic:** OperaLa/OperaX time-based events (audit feature #4 — "when operax deployed: multiple operala deployments deployed/upgraded/removed + discover SoRX/SoRLa + triggered by pack/bundle business events")

## Context & Problem

Today `greentic-operax` runs **exactly one pack per invocation** everywhere:

- `operax run` — one-shot reconciliation over a single `.gtpack`/handoff dir.
- `operax test` — spawns a persistent single-pack HTTP "manager server" (`start_manager_server`, `operax-manager`).
- `operax events subscribe` — a single-pack NATS business-event loop (CLI-private `business_events.rs`, `consumes`-matching → `run_artifact_with_client`).
- `operax presence subscribe` — subscribes to `greentic.presence.>`, builds an **in-memory directory keyed by `instance_id`**, and **only logs it** (CLI-private `presence.rs`, no `lookup`/resolve, not wired to `HttpSorxClient`).

There is **no multi-deployment concept**: no registry, no way to run/upgrade/remove several operala deployments in one long-running process. `ManagerRuntime` holds a single `pack` field; there is no supervisor hosting the two NATS subscribers together, and nothing consumes the presence directory.

Feature #4 needs an OperaX **daemon** that manages multiple operala deployments over their lifecycle. This spec covers **Slice 1: the daemon spine** — a multi-deployment registry with deploy / list / get / remove / run / upgrade over HTTP. Dynamic SoRX discovery (consume the presence directory) and daemon-wide business-event routing are **later slices (S2, S3)** that will be hosted *inside* this daemon.

## Goals (Slice 1)

- A new `operax serve` subcommand: a long-running daemon holding **N deployments**, each = a loaded pack + tenant/team binding + a SoRX client.
- HTTP control-plane: **deploy / list / get / remove / upgrade** deployments, identified by a **client-provided stable id**.
- HTTP data-plane: **run** — submit input to a named deployment; it executes that deployment's pack (reusing the existing one-shot run path).
- **Upgrade** = versioned replace-by-id: load the new pack, bump `version`, retain previous versions' metadata (capped) for a *future* rollback slice. Atomic and zero-downtime (old version stays active if the new pack fails to load).
- **Persistence**: registry survives daemon restart (disk JSON, mirroring greentic-sorx's `LocalDeploymentRegistryStore`). Packs are re-loaded from their stored paths on boot; a pack that fails to load marks that deployment `Failed` **without crashing the daemon**.

## Non-Goals (explicit — deferred to later slices)

- **Dynamic SoRX discovery** (consuming the presence directory to resolve `sorx_url`). Slice 1 keeps a **static `sorx_url` per deployment**. → **S2**.
- **Daemon-wide business-event routing** (fan an incoming NATS event to matching deployments by `consumes`). The existing single-pack `events subscribe` stays as-is. → **S3**.
- **Absorbing the CLI-private `presence.rs` / `business_events.rs` modules** into the daemon. Slice 1 is their *future* host but does not move or generalize them yet.
- **Rollback endpoint / traffic-splitting / blue-green.** Slice 1 only *retains* version history; flipping to a prior version is a later slice.
- **Store/URL/`oci://` fetch** of packs, and **byte-upload** deploy. Slice 1 deploy references a **local `.gtpack` path** on the daemon's own disk (mirrors `run --artifact`).
- **Per-tenant auth identity.** Slice 1 uses a single optional shared secret (mirrors sorx).

## Chosen Approach

**Approach A — additive multi-deployment layer that reuses `ManagerRuntime` wholesale.**

`ManagerRuntime` already encapsulates exactly what one deployment needs: `(pack, tenant, team, locale, client: Arc<dyn SorxClient>, runs ring-buffer)` plus `run_input` and `handle_json`. Slice 1 wraps **N of them** behind a new `DeploymentManager`, adds a new `operax serve` daemon server, and **leaves `run` / `test` / `events` / `presence` untouched**.

Rejected alternatives:
- **B — refactor `ManagerRuntime` to be inherently multi-pack.** Larger blast radius on the working `test` path and its tests; violates "don't refactor unrelated code."
- **C — standalone `operax-daemon` crate independent of `operax-manager`.** Duplicates the pack-load/run/HTTP wiring that `ManagerRuntime` + `start_manager_server` already provide.

### Module layout (all additive, in `greentic-operax`)

```
operax-manager/src/
  deployment.rs        (NEW) DeploymentManager, DeploymentSlot, DeploymentRecord,
                             DeploymentVersion, DeploymentStatus
  deployment_store.rs  (NEW) OperaxDeploymentStore — load/save registry JSON file
  serve.rs             (NEW) start_deployment_server — TCP accept loop + route dispatch
  http_util.rs         (opt) request-parse/response helpers factored from the existing
                             manager server, ONLY if duplication is real
operax-cli/src/lib.rs  (EDIT) add subcommand `Serve(ServeArgs)` + handler
```

**`ServeArgs`** (the `operax serve` flags): `--bind <addr>` (default `127.0.0.1:8099`),
`--registry <path>` (default `~/.greentic/operax/deployments.json`), `--secret <value>`
(optional shared secret; if omitted the API is open), `--sorx-token-env <name>` (default
`SORX_TOKEN`, sourced once at boot). The handler builds the `DeploymentManager` (loading
the persisted registry), then runs `start_deployment_server(manager, bind)`.

**Reuse seams (confirmed against code):**
- `operax_pack_loader::load_operational_pack(path) -> Result<OperationalPack>` — hold N loaded packs.
- `ManagerRuntime::new(pack, tenant, team, locale, audit_dir, client)` / `ManagerRuntime::load(options, client)` — one per deployment.
- `ManagerRuntime::run_input(...)` (builds a fresh `OperaxContext::new(tenant, team, locale, pack_digest)` per call → unique `request_id`, then `run_loaded_pack`) — the run endpoint delegates here.
- `HttpSorxClient::new(base_url, token)` — built at deploy time from the deployment's `sorx_url` + token. **Note:** `sorx_url`/`token` live on the *client*, not on `OperaxContext`.

## Data Model

Persisted **record** vs in-memory **slot** (the `OperationalPack` / `ManagerRuntime` are rebuilt from the path, never serialized — mirrors sorx):

```rust
// ---- persisted to disk (serde) ----
struct DeploymentRegistry {          // the whole file
    deployments: Vec<DeploymentRecord>,
}

struct DeploymentRecord {
    id: String,                      // client-provided, stable → upgrade/run/remove target
    tenant: String,
    team: Option<String>,
    locale: Option<String>,
    sorx_url: String,                // static in S1; S2 makes it dynamic
    active: DeploymentVersion,
    history: Vec<DeploymentVersion>, // previous versions, newest-first, capped (keep 5)
}

struct DeploymentVersion {
    version: u64,                    // per-deployment monotonic; ++ on upgrade
    gtpack_path: PathBuf,
    pack_digest: String,             // sha256 of the .gtpack — integrity + audit
    pack_version: Option<String>,    // from pack manifest metadata, if present
    deployed_at: String,             // RFC3339 (operax is normal Rust; real clock OK)
}

// ---- in-memory (record + live runtime) ----
struct DeploymentSlot {
    record: DeploymentRecord,
    runtime: Arc<ManagerRuntime>,    // the active version, ready to run
    status: DeploymentStatus,        // Ready | Failed { error: String }
}

struct DeploymentManager {
    slots: RwLock<HashMap<String, DeploymentSlot>>,
    store: OperaxDeploymentStore,    // disk persistence
    token: Option<String>,           // SoRX bearer token (from --sorx-token-env), shared
}
```

The `token` is process-wide (sourced once from `--sorx-token-env`, default `SORX_TOKEN`), consistent with how `run`/`test`/`events` already source it; per-deployment tokens are out of scope for S1.

## Persistence & Restart

- `OperaxDeploymentStore { path }` → `load() -> Result<DeploymentRegistry>` (empty registry if the file is absent), `save(&DeploymentRegistry) -> Result<()>` (pretty JSON; write-tmp-then-rename for atomicity). Path from a `--registry` flag; default `~/.greentic/operax/deployments.json`. Mirrors `LocalDeploymentRegistryStore` in greentic-sorx.
- **Boot (`serve`)**: `store.load()` → for each `DeploymentRecord`, `load_operational_pack(record.active.gtpack_path)` then build `ManagerRuntime` → `Ready`; on load error, keep the slot with `status = Failed { error }` (the record stays; the deployment is visible but not runnable). The daemon **never aborts boot** because one pack failed.
- **Mutations** (deploy / upgrade / remove) take the write lock, mutate the map, then `store.save()`; the HTTP response is only returned **after** a successful persist (persist-then-ack). **Run** takes the read lock only → runs of different deployments proceed concurrently.
- **History cap**: retain the newest 5 previous `DeploymentVersion`s (metadata only). Slice 1 does **not** delete old `.gtpack` files — those paths belong to the caller.

## HTTP API (hand-rolled TCP, mirrors the manager server)

All non-health routes require auth when a shared secret is configured (see below). All error bodies use a consistent envelope `{ "error": { "code": "...", "message": "..." } }`.

| Method | Path | Body / params | Success | Errors |
|---|---|---|---|---|
| `GET` | `/healthz`, `/readyz` | — | `200` | (pre-auth) |
| `POST` | `/v1/operax/deployments` | `{ id, gtpack_path, tenant, team?, locale?, sorx_url }` | `201 { id, version: 1, status }` | `409` id exists; `422` pack load failed; `400` bad body |
| `GET` | `/v1/operax/deployments` | — | `200 [{ id, tenant, pack_version, active_version, status }]` | — |
| `GET` | `/v1/operax/deployments/{id}` | — | `200 { record, status, history }` | `404` |
| `PUT` | `/v1/operax/deployments/{id}` | `{ gtpack_path }` | `200 { id, version: n+1 }` | `404`; `422` new pack load failed (old stays active) |
| `DELETE` | `/v1/operax/deployments/{id}` | — | `204` | `404` |
| `POST` | `/v1/operax/deployments/{id}/run` | `{ input, dry_run?, locale? }` | `200 RunReport` | `404`; `409` deployment `Failed`; `400` bad body |

Error codes: `OPERAX_DEPLOYMENT_EXISTS`, `OPERAX_DEPLOYMENT_NOT_FOUND`, `OPERAX_PACK_LOAD_FAILED`, `OPERAX_DEPLOYMENT_FAILED`, `OPERAX_BAD_REQUEST`, `OPERAX_UNAUTHORIZED`, `OPERAX_INTERNAL`.

## Operation Semantics

- **Deploy** — reject if `id` exists (`409`). `load_operational_pack(gtpack_path)`; on failure → `422 OPERAX_PACK_LOAD_FAILED`, **nothing registered**. Compute the `.gtpack` sha256 digest, read `pack_version` from the manifest metadata, build `DeploymentVersion { version: 1, ... }`, construct the `ManagerRuntime`, insert a `Ready` slot, `store.save()`, return `201`.
- **Upgrade (PUT)** — require existing `id` (`404`). Load the **new** pack; on failure → `422`, **the old version stays active and `Ready` (no downtime)**. On success: `version = active.version + 1`; push the old `active` onto the front of `history` (cap 5); swap `slot.runtime` to the new `Arc<ManagerRuntime>`; `store.save()`; return `200`. The whole swap is under the write lock (atomic).
- **Run** — look up `id` (`404`); if `Failed` → `409` (a pack that failed to load cannot run). Delegate to the slot's `runtime.run_input(input, dry_run, locale_override)` — which builds a fresh `OperaxContext` and calls `run_loaded_pack` against the held pack + client. Return the `RunReport`. Read-lock only.
- **Remove** — drop the slot from the map (`404` if absent), `store.save()`, `204`. The `.gtpack` file is left untouched.
- **List / Get** — projections over the registry; `Get` includes `history` for audit.

## Error Handling, Auth, Concurrency

- **No `unwrap`/`panic` on daemon paths.** Every fallible step returns `Result` mapped to an HTTP status. A poisoned `RwLock` maps to `500 OPERAX_INTERNAL` (recover the guard or return the error — never propagate a panic across the accept loop). `#![forbid(unsafe_code)]` stays.
- **Auth** — mirror greentic-sorx: an optional shared secret via `--secret` / env. When set, every non-health route requires `Authorization: Bearer <secret>` **or** `x-greentic-sorx-secret: <secret>`; mismatch/absent → `401 OPERAX_UNAUTHORIZED`. When unset, the API is open (local-dev), same stance as the existing manager server.
- **Tenant** is a property of the *deployment* (supplied at deploy time in the body), **not** a per-request header — differs from sorx where tenant is per-request.
- **Concurrency** — one `RwLock` guards the slot map; mutations serialize + persist, runs share the read lock. Each `ManagerRuntime` already serializes its own `runs` ring-buffer internally.

## Testing Strategy

- **Unit (`deployment.rs`)** — deploy/upgrade/remove/get against the existing `examples/tenancy` fixture pack: id-exists `409`, upgrade version bump + history cap at 5, digest computed, remove then get `404`.
- **Upgrade atomicity** — upgrade with a bad path → `422`, previous version still `Ready` and active.
- **Persistence roundtrip** — `save` → fresh `OperaxDeploymentStore::load` → equal records; boot with one record pointing at a missing/corrupt pack → that slot `Failed`, the others `Ready`, no panic.
- **Server integration** (mirrors `crates/operax-cli/tests/customer_pilot_demo.rs`) — spawn `serve` on an ephemeral port → deploy → `run` (dry-run) asserts the tenancy `RunReport` (3 decisions) → upgrade → `run` again → remove → `get` `404`. Reuse the existing mock-sorx.
- **Auth** — with a secret set: no-header request → `401`; correct header → `200`.
- **CI as build-oracle** — the sandbox has no network; validation is via a PR to `greentic-operax` (main line, `greentic-types 1.1`, **no release-train**), whose CI builds + tests. This is the same technique used to ship the operala business-events work.

## Interfaces to Later Slices

- **S2 (dynamic SoRX discovery)** — the daemon becomes the host for the presence subscriber; `DeploymentSlot`'s static `sorx_url` gains a resolve path: presence directory `lookup(tenant, sor) -> base_url` rebuilds the `HttpSorxClient`. Slice 1's `sorx_url` field and per-deployment client construction are the seam.
- **S3 (business-event → deployment routing)** — the daemon hosts the business-event subscriber; instead of one pack's `consumes`, it fans an incoming event across `slots` whose pack `consumes` matches, dispatching via each slot's `runtime.run_input`. Slice 1's `DeploymentManager` map is the routing table.
- **Rollback** — Slice 1 retains `history`; a later `POST /v1/operax/deployments/{id}/rollback` flips `active` to a prior `DeploymentVersion` and rebuilds the runtime.

## Risks / Open Questions

- **CLI-private modules** (`presence.rs`, `business_events.rs`) will eventually need lifting into `operax-manager` (or a shared crate) so the daemon can host them — a refactor deferred to S2/S3, flagged here so it is not a surprise.
- **Hand-rolled HTTP duplication** — the daemon server reuses the manager's request-parse/response style; if the duplication is more than trivial, factor `http_util.rs`. Kept minimal to avoid touching the working `test` path.
- **Token model** — a single process-wide SoRX token is an S1 simplification; per-deployment credentials may be needed once real multi-tenant deployments land.
