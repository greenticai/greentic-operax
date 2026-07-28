# OperaX Deploy-by-Reference — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** OperaX `serve` deploy/upgrade can fetch a pack from a `reference` (oci/repo/store/http/file) into a durable managed cache, instead of requiring a local `.gtpack` path.

**Architecture:** A sync `fetch_pack_ref` helper in `operax-pack-loader` (file/http tested; OCI reimplemented by mirroring `greentic-distributor-client` via the `oci-distribution` leaf crate — no release-train). Deploy threads an optional `reference` and resolves it to a durable managed path before the existing `load_operational_pack`.

**Tech Stack:** Rust edition 2024; new leaf deps `ureq` (http), `oci-distribution 0.11` (rustls-tls), `tokio` (rt) — none pulls `greentic-types`.

## Global Constraints

- Base `main` (16c8268). `greentic-types 1.1`, no git-deps — NOT on the release-train. New deps must be leaf crates with NO `greentic-types` dependency (that was distributor-client's coupling — `oci-distribution` itself is clean; confirm at T2).
- No `unwrap()`/`panic!()`/`expect()` outside `#[cfg(test)]`. `operax_core::{Result, OperaxError}` in operax-core/manager/pack-loader (NO anyhow). Clippy-clean `-D warnings`. English only; Conventional Commits; no AI-authorship trailers.
- **No local cargo** (no network): validate via CI. **rustfmt offline** — run before each commit. `perf` = `--all-features`. New-dep resolution happens in CI (regenerates lock).
- **Verified facts:**
  - `operax_pack_loader::load_operational_pack(path: impl AsRef<Path>) -> operax_core::Result<OperationalPack>` accepts a handoff DIR or a `.gtpack`.
  - `operax-pack-loader` deps today: operax-core, greentic-pack, serde, serde_json, serde_yaml, sha2 (no tokio/http/oci).
  - `DeploySpec`/`DeploymentRecord` (post-hardening): `{ id, tenant, team, locale, sorx_url: Option, sor: Option, environment: Option, active, history }`; `DeploymentVersion { version, gtpack_path: PathBuf, pack_digest, deployed_at_unix }`. `DeployBody`/`UpgradeBody` in serve.rs map to these; `deploy` calls `load_operational_pack(&spec.gtpack_path)`; boot-reload re-reads `record.active.gtpack_path`.
  - OCI mirror source (READ, do not depend): `greentic-distributor-client/src/oci_packs.rs` — `default_pack_layer_media_types()` (accepted), `default_preferred_pack_layer_media_types()` (preferred), `select_layer(layers, preferred, ref)` (lowest preferred-rank wins; else first), `Client::pull(&ref, &RegistryAuth::Anonymous, &accepted)`. `oci-distribution = { version="0.11", default-features=false, features=["rustls-tls"] }`.

---

## File Structure
- `crates/operax-pack-loader/src/fetch.rs` (CREATE) — `fetch_pack_ref` + `pull_oci_blob`.
- `crates/operax-pack-loader/src/lib.rs` (MODIFY) — `pub mod fetch;` + re-export.
- `crates/operax-pack-loader/Cargo.toml` (MODIFY) — add `ureq`, then `oci-distribution` + `tokio`.
- `crates/operax-manager/src/deployment.rs` (MODIFY) — `reference`/optional `gtpack_path`/`source_ref`/`DeployError::Fetch`/`pack_cache_dir`/resolve-effective-path.
- `crates/operax-manager/src/serve.rs` (MODIFY) — `DeployBody`/`UpgradeBody` `reference`+optional path; 400/502 mapping.
- `crates/operax-manager/tests/deploy_by_ref_e2e.rs` (CREATE).
- README.

---

## Task 1: `fetch_pack_ref` — file/dir/http + scheme mapping (OCI stubbed)

**Files:** CREATE `crates/operax-pack-loader/src/fetch.rs`; MODIFY `lib.rs` (`pub mod fetch;`), `Cargo.toml` (add `ureq`).

**Interfaces:**
- `pub fn fetch_pack_ref(reference: &str, cache_dir: &Path) -> operax_core::Result<PathBuf>`.
- Private `fn pull_oci_blob(image_ref: &str, cache_dir: &Path) -> operax_core::Result<PathBuf>` — placeholder returning `Err(OperaxError::new("oci_fetch_unavailable", "OCI fetch is implemented in the next task"))` (Task 2 replaces the body).

- [ ] **Step 1: failing tests** (in fetch.rs `#[cfg(test)]`)
```rust
#[test]
fn maps_repo_and_store_schemes_from_env() {
    // repo:// and store:// map via env base; unset base -> error. Use a guard that
    // sets/removes the env var within the test (serialize via a Mutex or unique keys).
    // (If env-mutation-in-tests is fragile, factor a pure `fn map_scheme(reference, repo_base: Option<&str>, store_base: Option<&str>) -> Result<Resolved>` and unit-test THAT instead — preferred.)
}

#[test]
fn fetches_local_file_into_cache() {
    let tmp = std::env::temp_dir().join(format!("obr-src-{}.gtpack", std::process::id()));
    std::fs::write(&tmp, b"PK\x03\x04 fake pack bytes").unwrap();
    let cache = std::env::temp_dir().join(format!("obr-cache-{}", std::process::id()));
    let got = fetch_pack_ref(&format!("file://{}", tmp.display()), &cache).expect("fetch file");
    assert!(got.starts_with(&cache));
    assert_eq!(std::fs::read(&got).unwrap(), b"PK\x03\x04 fake pack bytes");
    // idempotent: second call returns the same cached path
    let again = fetch_pack_ref(&format!("file://{}", tmp.display()), &cache).expect("fetch again");
    assert_eq!(got, again);
}

#[test]
fn passes_through_a_local_directory() {
    // a bare/file:// ref pointing at a DIRECTORY returns the dir path as-is (can't hash a dir).
    let dir = std::env::temp_dir().join(format!("obr-dir-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cache = std::env::temp_dir().join(format!("obr-dirc-{}", std::process::id()));
    let got = fetch_pack_ref(dir.to_str().unwrap(), &cache).expect("dir passthrough");
    assert_eq!(got, dir);
}

#[test]
fn fetches_over_http() {
    // spin a tiny TcpListener serving fixed bytes; assert fetch writes them.
    // (mirror the S1 e2e's raw-TCP server helper; respond with a minimal HTTP/1.1 200 + body.)
}
```
Prefer factoring a pure `map_scheme(reference, repo_base: Option<&str>, store_base: Option<&str>) -> Result<Resolved>` where `enum Resolved { LocalFile(PathBuf), LocalDir(PathBuf), Http(String), Oci(String) }`, and unit-test it directly (no env mutation).
- [ ] **Step 2: run to verify fail** — `cargo test -p operax-pack-loader fetch` → FAIL.
- [ ] **Step 3: implement**
  - `Cargo.toml`: add `ureq = { version = "2", default-features = false, features = ["tls"] }` (or the workspace's existing ureq if present — check; greentic-start uses ureq).
  - `map_scheme(reference, repo_base, store_base)`: `oci://<rest>`→`Oci(rest)`; `repo://<rest>`→`Oci(format!("{repo_base}/{rest}"))` (err if `repo_base` None); `store://<rest>`→`Oci(format!("{store_base}/{rest}"))` (err if None); `http://`/`https://`→`Http(reference)`; `file://<p>` or bare → classify by `Path::is_dir()` → `LocalDir` else `LocalFile`.
  - `fetch_pack_ref`: read env bases (`GREENTIC_REPO_REGISTRY_BASE`/`GREENTIC_STORE_REGISTRY_BASE`), call `map_scheme`, then: `LocalDir(p)` → `Ok(p)`; `LocalFile(p)` → read bytes, `sha256`, write to `cache_dir/<sha>.gtpack` (skip if exists), return that path; `Http(url)` → `ureq` GET bytes → same cache-write; `Oci(image)` → `pull_oci_blob(&image, cache_dir)`. `std::fs::create_dir_all(cache_dir)` first. Map all IO/http errors to `OperaxError::new("pack_fetch_failed", ...)`.
  - `pull_oci_blob`: placeholder `Err(OperaxError::new("oci_fetch_unavailable", "OCI fetch lands in the next task"))`.
- [ ] **Step 4: run to verify pass** — file/dir/http + map_scheme tests PASS.
- [ ] **Step 5: rustfmt + commit**
```bash
rustfmt --edition 2024 crates/operax-pack-loader/src/fetch.rs crates/operax-pack-loader/src/lib.rs
git add crates/operax-pack-loader/src/fetch.rs crates/operax-pack-loader/src/lib.rs crates/operax-pack-loader/Cargo.toml
git commit -m "feat(operax): fetch_pack_ref for file/http refs + scheme mapping"
```

---

## Task 2: OCI pull (mirror distributor-client)

**Files:** MODIFY `crates/operax-pack-loader/src/fetch.rs` (`pull_oci_blob` body), `Cargo.toml` (add `oci-distribution`, `tokio`).

**Interfaces:** `pull_oci_blob(image_ref, cache_dir)` now performs a real OCI pull.

- [ ] **Step 1: implement** (no unit test — no registry in CI; validated by mirror-fidelity + code review). FIRST **READ `greentic-distributor-client/src/oci_packs.rs`** and copy verbatim: the `PACK_LAYER_MEDIA_TYPE*` const strings, `default_pack_layer_media_types()`, `default_preferred_pack_layer_media_types()`, and the `select_layer` rank logic.
  - `Cargo.toml`: `oci-distribution = { version = "0.11", default-features = false, features = ["rustls-tls"] }` and `tokio = { version = "1", features = ["rt"] }`. **Verify neither transitively depends on `greentic-types`** (they are external crates — they don't; if the lock ends up pulling greentic-types via them, STOP and report — that would mean the wrong crate).
  - `pull_oci_blob(image_ref, cache_dir)`: build a `tokio::runtime::Builder::new_current_thread().enable_all().build()?` and `block_on`:
    - `let reference: oci_distribution::Reference = image_ref.parse().map_err(...)?;`
    - `let client = oci_distribution::Client::new(oci_distribution::client::ClientConfig { protocol: ..default.., ..Default::default() });` (rustls default).
    - `let accepted: Vec<&str> = <default_pack_layer_media_types list>;`
    - `let image = client.pull(&reference, &oci_distribution::secrets::RegistryAuth::Anonymous, accepted).await.map_err(...)?;`
    - `let chosen = select_layer(&image.layers, &preferred, image_ref)?;` (adapt `select_layer` to `oci_distribution`'s layer type — it exposes `.media_type` + `.data`).
    - compute `sha256(&chosen.data)`; write to `cache_dir/<sha>.gtpack` (skip if exists); return path.
  - Map all errors to `OperaxError::new("pack_fetch_failed", ...)`. No `unwrap`/`expect` outside tests.
  (Adapt exact `oci-distribution` 0.11 API names by reading the crate + distributor-client's usage; the pull signature is `Client::pull(&Reference, &RegistryAuth, Vec<&str>) -> Result<ImageData>` with `ImageData.layers: Vec<ImageLayer>` where `ImageLayer { data: Vec<u8>, media_type: String, .. }`.)
- [ ] **Step 2: run to verify** — CI compiles + clippy; no unit test (state this in the report; the file/http tests from Task 1 still pass).
- [ ] **Step 3: rustfmt + commit**
```bash
rustfmt --edition 2024 crates/operax-pack-loader/src/fetch.rs
git add crates/operax-pack-loader/src/fetch.rs crates/operax-pack-loader/Cargo.toml
git commit -m "feat(operax): OCI pack pull mirroring greentic-distributor-client layer selection"
```

---

## Task 3: deploy contract — optional path + reference + source_ref + DeployError::Fetch + validation + pack_cache_dir

**Files:** MODIFY `crates/operax-manager/src/deployment.rs`, `crates/operax-manager/src/serve.rs`.

**Interfaces:** `DeploySpec.gtpack_path: Option<PathBuf>` + `reference: Option<String>`; `DeploymentRecord`… (record keeps `gtpack_path` on its `DeploymentVersion` as the RESOLVED local path); `DeploymentVersion.source_ref: Option<String>` (`#[serde(default)]`); `DeployError::Fetch(String)`; `DeploymentManager.pack_cache_dir: PathBuf`; `DeployBody`/`UpgradeBody`: `gtpack_path: Option<PathBuf>` + `reference: Option<String>` (`#[serde(default)]`).

- [ ] **Step 1: failing test**
```rust
#[test]
fn deploy_requires_path_or_reference() {
    let mgr = test_manager();
    let mut spec = deploy_spec("noneither");
    spec.gtpack_path = None; spec.reference = None;
    assert!(matches!(mgr.deploy(spec).unwrap_err(), DeployError::BadRequest(_)));
}

#[test]
fn deploy_with_file_reference_records_source_ref() {
    let mgr = test_manager();
    let mut spec = deploy_spec("byref");
    let dir = repo_examples().join("tenancy/handoff"); // a directory ref → passthrough
    spec.gtpack_path = None;
    spec.reference = Some(format!("file://{}", dir.display()));
    let summary = mgr.deploy(spec).expect("deploy by ref");
    assert!(matches!(summary.status, DeploymentStatus::Ready));
    let detail = mgr.get("byref").expect("exists");
    assert_eq!(detail.record.active.source_ref.as_deref(), Some(&*format!("file://{}", dir.display())));
}
```
(`deploy_spec` sets `gtpack_path: Some(...)`, `reference: None`. `DeployError::BadRequest` may already exist from an earlier slice — check; add if missing.)
- [ ] **Step 2: run to verify fail** — FAIL/compile error (shape change).
- [ ] **Step 3: implement**
  - `DeploySpec`: `gtpack_path: Option<PathBuf>`, add `reference: Option<String>`.
  - `DeploymentVersion`: add `source_ref: Option<String>` (`#[serde(default)]`).
  - `DeployError`: add `Fetch(String)` (+ `BadRequest(String)` if not present).
  - `DeploymentManager`: add `pack_cache_dir: PathBuf`; set in `new` (derive from the store path's parent + `packs/`, or `~/.greentic/operax/packs`). `with_*` chaining not needed.
  - `deploy` (and the runtime rebuild in `build_runtime`/`upgrade` where it loads): resolve the effective local path:
    ```
    let local = match (spec.reference.as_deref(), spec.gtpack_path.clone()) {
        (Some(r), _) => operax_pack_loader::fetch_pack_ref(r, &self.pack_cache_dir).map_err(|e| DeployError::Fetch(e.to_string()))?,
        (None, Some(p)) => p,
        (None, None) => return Err(DeployError::BadRequest("deploy requires gtpack_path or reference".into())),
    };
    let pack = operax_pack_loader::load_operational_pack(&local).map_err(|e| DeployError::PackLoad(e.to_string()))?;
    ```
    Store `local` as `DeploymentVersion.gtpack_path` and `spec.reference` as `source_ref`. Update all test fixtures/`deploy_spec` for the new `Option`/fields. `upgrade` takes a `gtpack_path` today — extend it to also accept a reference OR keep upgrade path-only for this slice and note it (SIMPLEST: give `upgrade` the same `(gtpack_path: Option, reference: Option)` resolution; if that ripples too far, keep upgrade path-only and document — deploy-by-ref is the headline).
  - `serve.rs`: `DeployBody`/`UpgradeBody` gain `reference: Option<String>` + `gtpack_path: Option<PathBuf>` (both `#[serde(default)]`); pass into `DeploySpec`.
- [ ] **Step 4: run to verify pass** — PASS.
- [ ] **Step 5: rustfmt + commit** — `feat(operax): deploy/upgrade accept a pack reference; record source_ref`.

---

## Task 4: HTTP error mapping (400/502)

**Files:** MODIFY `crates/operax-manager/src/serve.rs`.

- [ ] **Step 1: failing test** (serve.rs tests)
```rust
#[test]
fn deploy_with_neither_path_nor_ref_is_400() {
    let m = mgr();
    let body = serde_json::to_vec(&serde_json::json!({"id":"x","tenant":"demo"})).unwrap();
    let r = handle_deployment_request("POST", "/v1/operax/deployments", &body, &m);
    assert_eq!(r.status, 400);
    assert_eq!(r.body["error"]["code"], "OPERAX_BAD_REQUEST");
}
```
- [ ] **Step 2: run to verify fail** — FAIL.
- [ ] **Step 3: implement** — extend `deploy_error_reply`: `DeployError::Fetch(m) => HttpReply::error(502, "OPERAX_PACK_FETCH_FAILED", m)` (and `BadRequest(m) => 400 "OPERAX_BAD_REQUEST"` if not already mapped).
- [ ] **Step 4: run to verify pass** — PASS.
- [ ] **Step 5: rustfmt + commit** — `feat(operax): map pack-fetch failure to 502, missing source to 400`.

---

## Task 5: deploy-by-ref e2e

**Files:** CREATE `crates/operax-manager/tests/deploy_by_ref_e2e.rs`.

- [ ] **Step 1: failing test** — mirror `event_routing_e2e.rs` scaffolding (StubClient, unique registry, `examples` path). Deploy via HTTP `POST /v1/operax/deployments` with a body carrying `reference: "file://<abs handoff dir>"` and NO `gtpack_path` → 201; then `GET /v1/operax/deployments/{id}` → 200 with `source_ref` present; `POST .../{id}/run` dry-run → 200. (Reuse the S1 `deployment_server_e2e.rs` HTTP-server pattern OR call `DeploymentManager` directly — direct is simpler; a file:// dir ref needs no network.)
- [ ] **Step 2: run to verify fail** — FAIL.
- [ ] **Step 3: complete wiring.**
- [ ] **Step 4: run to verify pass** — PASS.
- [ ] **Step 5: rustfmt + commit** — `test(operax): deploy-by-reference e2e (file:// dir ref)`.

---

## Task 6: docs

**Files:** MODIFY `crates/operax-cli/README.md`.

- [ ] **Step 1:** Document the deploy body's optional `reference` (schemes `oci://`/`repo://`/`store://`/`http(s)://`/`file://`; at least one of `reference`/`gtpack_path` required); env vars `GREENTIC_REPO_REGISTRY_BASE`/`GREENTIC_STORE_REGISTRY_BASE`; fetched packs cached under the managed dir; errors `400`/`502`. Note the boundaries: catalog LISTING and a `store://`-hosted operala payload are not part of this (upstream publish pipeline); cache eviction is a follow-up.
- [ ] **Step 2: commit** — `docs(operax): document deploy-by-reference`.

---

## Self-Review

**Spec coverage:** fetch helper file/http (T1) + OCI (T2); deploy contract reference/optional-path/source_ref/validation/cache-dir (T3); 400/502 (T4); e2e (T5); docs (T6). ✓

**Placeholder scan:** T1's `pull_oci_blob` is an honest error-returning placeholder (not fake success), replaced with the real body in T2 — a deliberate two-step, not an unresolved stub. T2 has no unit test (no CI registry) — flagged, mirror-fidelity + code review substitute. T1's env-based scheme test prefers a pure `map_scheme` helper to avoid env-mutation flakiness. All other steps carry real code.

**Type consistency:** `fetch_pack_ref(&str, &Path) -> Result<PathBuf>` in T1, called in T3. `map_scheme`→`Resolved` enum in T1. `DeployError::Fetch` added T3, mapped T4. `reference: Option<String>` + `gtpack_path: Option<PathBuf>` threaded DeploySpec/DeployBody/UpgradeBody (T3). `source_ref` on DeploymentVersion T3. `pack_cache_dir` on DeploymentManager T3.

## Execution Notes
No local cargo → CI validates (new-dep resolution + compile + clippy + tested paths). rustfmt offline per commit. Batch CI after T2 (new deps resolve) and T4. **T2 dep check is load-bearing:** confirm `oci-distribution`/`tokio` don't pull `greentic-types` (else operax leaves its OLD line). OCI pull is code-review-validated, not CI-tested.
