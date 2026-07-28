# OperaX Deploy-by-Reference — Design

- **Date:** 2026-07-28
- **Repo:** `greentic-operax`
- **Base branch:** `main` (post feature #4 + hardening, at 16c8268). `greentic-types 1.1`, no git-deps → **not on the release-train**.
- **Feature branch:** `feat/operax-deploy-by-ref`
- **Origin:** the grounded, buildable half of feature #4 S4 "SoRLa catalog discovery" — deploy a pack by a remote reference instead of a pre-placed local path.

## Context & Problem

OperaX `serve` deploys packs by a **local `.gtpack` path** only (`DeployBody.gtpack_path`, `load_operational_pack(&spec.gtpack_path)`). To deploy without pre-staging files on the daemon host, deploy must be able to **fetch a pack from a reference** — an OCI registry (`oci://`/`repo://`/`store://`) or an HTTP(S) URL.

The pack-fetch primitive exists in `greentic-start` (`bundle_ref.rs` → `greentic-distributor-client`), but reusing it is a **trap**: `bundle_ref` is a private module and `greentic-distributor-client` hard-pins `greentic-types >=1.2.0-dev`, which **cannot co-resolve** with operax's `greentic-types 1.1`. Depending on it would drag operax onto the release-train. So OperaX must **reimplement** a minimal fetch using the leaf crate `oci-distribution` (no `greentic-types` dependency), mirroring distributor-client's OCI layer-selection for wire compatibility.

## Goals

- Deploy/upgrade accept an optional `reference` (alongside the existing `gtpack_path`); at least one is required.
- Fetch the referenced pack to a **durable managed cache dir** (keyed by content/OCI digest), then hand the local path to the existing `load_operational_pack`.
- Schemes: `oci://` (arbitrary registry), `repo://`→`{GREENTIC_REPO_REGISTRY_BASE}/…`, `store://`→`{GREENTIC_STORE_REGISTRY_BASE}/…` (OCI), `http(s)://` (download), `file://`/bare path (copy/passthrough).
- **OCI wire-compatible** with greentic packs by mirroring `greentic-distributor-client`'s media-type list + layer-selection.
- Operax stays **OLD-line** — the only new deps are leaf crates (`oci-distribution`, `ureq`, `tokio` rt) with no `greentic-types` coupling.

## Non-Goals (gated on upstream)

- **Catalog LISTING / discovery** (enumerate available packs) — needs an operax HTTP client for greentic-store-server's `/api/v1/packs` (a different, non-OCI channel). Deferred.
- **A `store://`-hosted operala pack payload** — no operala runtime pack is published to any OCI registry today (only the designer extension goes to the store). `store://` resolves like `oci://` here but has nothing operala to fetch until a publish pipeline exists. This slice ships the scheme; the payload is a separate upstream decision.
- **Cache eviction / GC** of the managed dir (unbounded for now; note as follow-up).
- **OCI auth** beyond anonymous / the default `oci-distribution` auth (no per-registry credential config in this slice).

## Chosen Approach

### 1. Fetch helper (`operax-pack-loader`, new `fetch.rs`)

`pub fn fetch_pack_ref(reference: &str, cache_dir: &Path) -> operax_core::Result<PathBuf>` — synchronous public API (spins its own current-thread tokio runtime internally for the OCI path). Scheme dispatch:
- **`oci://<rest>`** → OCI pull of `<rest>`.
- **`repo://<rest>`** → map to `{GREENTIC_REPO_REGISTRY_BASE}/<rest>` (env; error if unset) → OCI pull.
- **`store://<rest>`** → map to `{GREENTIC_STORE_REGISTRY_BASE}/<rest>` (env; error if unset) → OCI pull.
- **`http://` / `https://`** → `ureq` GET → write bytes to `cache_dir/<sha256(bytes)>.gtpack`.
- **`file://<path>` or a bare path** → copy the file to `cache_dir/<sha256(bytes)>.gtpack` (or passthrough if already local — simplest: still copy into the managed dir so the record points at a durable, operax-owned location).

**OCI pull** (mirrors `greentic-distributor-client/src/oci_packs.rs`, reimplemented — do NOT depend on that crate):
- `oci-distribution = { version = "0.11", default-features = false, features = ["rustls-tls"] }`.
- `Client::pull(&parsed_reference, &auth, accepted_layer_media_types)`; `auth = RegistryAuth::Anonymous`.
- `accepted_layer_media_types` = the `default_pack_layer_media_types()` list (copy the media-type constants verbatim from distributor-client: `application/vnd.greentic.pack.layer.*` + zip/tar/octet-stream/json variants).
- Choose the layer by **preferred-rank** (`select_layer`): the first layer whose media-type has the lowest index in `default_preferred_pack_layer_media_types()`; ties/none → first layer. Write `chosen_layer.data` to `cache_dir/<resolved_digest>.gtpack`.
- Idempotent: if the target file already exists (digest known), skip the network pull.

### 2. Deploy contract (`serve.rs` + `deployment.rs`)

- `DeployBody` / `UpgradeBody`: `gtpack_path: Option<PathBuf>` (was required) + new `reference: Option<String>` (both `#[serde(default)]`).
- `DeploySpec`: `gtpack_path: Option<PathBuf>` + `reference: Option<String>`.
- `DeploymentVersion`: add `source_ref: Option<String>` (`#[serde(default)]`) — provenance (the ref the pack came from; `None` for direct-path deploys).
- **Validation** (deploy): require at least one of `gtpack_path` / `reference` (else `400 OPERAX_BAD_REQUEST`).
- **deploy / upgrade**: resolve the effective local path:
  ```
  let local = match (reference, gtpack_path) {
      (Some(r), _)     => fetch_pack_ref(&r, &self.pack_cache_dir)?,   // ref wins
      (None, Some(p))  => p,                                           // direct path (S1 behaviour)
      (None, None)     => Err(BadRequest),
  };
  load_operational_pack(&local) ...
  ```
  Store `local` as `DeploymentVersion.gtpack_path` (durable managed path → boot-reload re-reads it unchanged) and `reference` as `source_ref`.
- `DeploymentManager` gains a `pack_cache_dir: PathBuf` (constructed by `new`/`load`, default `<registry-dir-parent>/packs` or `~/.greentic/operax/packs`). Fetch errors map to a new `DeployError::Fetch(String)` → `502 OPERAX_PACK_FETCH_FAILED`.

### 3. Boot-reload

Unchanged: `build_runtime` reloads `record.active.gtpack_path`, which for a ref-deploy is the durable managed-cache path — the fetched file persists across restarts, so no re-fetch is needed on boot. (A future enhancement could re-fetch from `source_ref` if the cache file is missing; out of scope.)

## Error / Status Additions

| Condition | Error | HTTP |
|---|---|---|
| deploy with neither `gtpack_path` nor `reference` | `OPERAX_BAD_REQUEST` | 400 |
| fetch fails (registry unreachable, bad ref, env base unset, no matching layer) | `OPERAX_PACK_FETCH_FAILED` | 502 |
| fetched pack fails to load | `OPERAX_PACK_LOAD_FAILED` (existing) | 422 |

## Testing Strategy

- **Scheme mapping (pure unit):** `repo://x`→`{base}/x`, `store://x`→`{base}/x` (with env set), error when the base env is unset; `oci://x` passthrough; `http(s)`/`file`/bare classification.
- **`file://` + bare-path fetch (unit):** `fetch_pack_ref` on a local `.gtpack` copies it into the cache dir keyed by sha256 and returns the managed path; a second call is idempotent (same path, no re-copy needed).
- **`http(s)` fetch (unit):** spin a tiny local HTTP server (std `TcpListener` in the test, like the S1 e2e) serving a fixture `.gtpack`; assert `fetch_pack_ref("http://127.0.0.1:PORT/x.gtpack", dir)` writes + returns the file.
- **deploy-by-ref plumbing (integration):** deploy with `reference = "file://…/tenancy/handoff/…"`? — the tenancy fixture is a **directory**, not a `.gtpack`; use a bare-path/`file://` ref to the handoff dir IF `load_operational_pack` accepts a dir (it does — S1), so `fetch_pack_ref` for a local **directory** must passthrough the dir path (don't sha/copy a dir). Handle: local dir → passthrough as-is (can't hash a dir); local file → copy. Then deploy resolves + loads. Assert the deployment is `Ready` and its `source_ref` is recorded.
- **OCI pull:** NOT unit-testable (no registry in CI, no operala OCI payload exists). Validated by **code review against `greentic-distributor-client/src/oci_packs.rs`** (mirror fidelity: same accepted/preferred media-type lists, same `select_layer` rank logic) + the leaf crate. Flag this as a known coverage gap; the media-type constants + layer-selection are copied verbatim.
- **CI as build-oracle** (no local network): compile + clippy + the tested paths via a PR. Confirm the new deps (`oci-distribution`, `ureq`, `tokio`) resolve on operax's line (CI regenerates the lock).

## Risks / Notes

- **OCI wire-compat is trust-by-mirror.** No CI test + no real operala OCI artifact today. The mitigation is verbatim mirroring of distributor-client's media-type list + `select_layer`. First real validation happens whenever an operala pack is actually pushed to a registry.
- **New deps on operax-pack-loader** (`oci-distribution 0.11` rustls-tls, `ureq`, `tokio` rt) — all leaf crates with no `greentic-types` dependency, so operax stays OLD-line. Confirm at plan time that none transitively pulls `greentic-types` (distributor-client's problem was its OWN direct greentic-types dep, not oci-distribution's).
- **Local directory refs:** `load_operational_pack` accepts a handoff DIR or a `.gtpack`. `fetch_pack_ref` must passthrough a local directory (can't hash/copy a dir into a single file); only remote/http/file-that-is-a-file gets cached. Keep the dir-passthrough branch explicit.
- **Cache dir unbounded** — eviction/GC deferred; note in docs.
- **store:// empty for operala** — the scheme works but has no operala payload until an upstream publish pipeline exists; documented as a non-goal boundary.
