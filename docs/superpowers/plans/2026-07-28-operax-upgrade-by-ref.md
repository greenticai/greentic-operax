# OperaX upgrade-by-reference Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let `operax` upgrade a deployment from a pack **reference** (oci/http/repo/store/file/bare-path), at parity with deploy-by-reference, reusing `fetch_pack_ref`.

**Architecture:** Extract the effective-path resolver shared by `deploy` and `upgrade` into one private `DeploymentManager::resolve_effective_path` method; widen `upgrade`'s signature and the HTTP `UpgradeBody` to carry an optional `reference`; record `source_ref` on the upgraded version. No new deps; no fetch-semantics change.

**Tech Stack:** Rust (edition 2024, toolchain 1.95.0), `operax_core::{Result, OperaxError}` error type, hand-rolled TCP HTTP server (`serve.rs`), `operax_pack_loader::fetch::fetch_pack_ref`.

## Global Constraints

- `operax` MUST stay on **greentic-types 1.1** (old line): add NO new dependency; use only crates already in the workspace. Do not touch the release-train.
- No `unwrap()` / `panic!()` / `expect()` outside `#[cfg(test)]`.
- English-only in source, tests, and commit messages. Conventional Commits.
- NO AI-authorship / co-author trailer on operax commits.
- Do NOT run `cargo build`/`cargo test` locally (no cargo toolchain for this repo on this machine) — validate via CI (draft PR, `pull_request` trigger runs `ci`/`perf`/`demo`). `rustfmt --edition 2024` on changed files IS available offline; run it before each commit.
- Error-code mapping is fixed and already implemented in `deploy_error_reply`: `Fetch` → 502 `OPERAX_PACK_FETCH_FAILED`, `BadRequest` → 400 `OPERAX_BAD_REQUEST`, `NotFound` → 404. Reuse it; add no new codes.

---

### Task 1: Extract shared `resolve_effective_path` helper

**Files:**
- Modify: `crates/operax-manager/src/deployment.rs` (add private method; rewire `deploy`'s inline `match` at ~lines 300-309)

**Interfaces:**
- Produces: `fn resolve_effective_path(&self, reference: Option<&str>, gtpack_path: Option<PathBuf>) -> Result<PathBuf, DeployError>` on `impl DeploymentManager`.
- Consumes: existing `self.pack_cache_dir`, `operax_pack_loader::fetch::fetch_pack_ref`, `DeployError::{Fetch, BadRequest}`.

- [ ] **Step 1: Add the private helper method**

Add to the `impl DeploymentManager` block (near `deploy`):

```rust
/// Resolve the effective local pack path from an optional reference and an
/// optional bare path. A `reference` is fetched into the managed pack cache
/// and wins when both are set; a bare `gtpack_path` is used as-is; neither
/// set is a bad request. Shared by `deploy` and `upgrade`.
fn resolve_effective_path(
    &self,
    reference: Option<&str>,
    gtpack_path: Option<PathBuf>,
) -> Result<PathBuf, DeployError> {
    match (reference, gtpack_path) {
        (Some(r), _) => operax_pack_loader::fetch::fetch_pack_ref(r, &self.pack_cache_dir)
            .map_err(|e| DeployError::Fetch(e.to_string())),
        (None, Some(p)) => Ok(p),
        (None, None) => Err(DeployError::BadRequest(
            "requires gtpack_path or reference".into(),
        )),
    }
}
```

- [ ] **Step 2: Rewire `deploy` to call the helper**

Replace the inline `let local = match (spec.reference.as_deref(), spec.gtpack_path.clone()) { ... };` block in `deploy` with:

```rust
let local = self.resolve_effective_path(spec.reference.as_deref(), spec.gtpack_path.clone())?;
```

Leave the rest of `deploy` unchanged (it still loads the pack from `local`, sets `source_ref: spec.reference`, etc.).

- [ ] **Step 3: Confirm the existing deploy unit tests still describe the same behavior**

No test changes in this task. The existing `deploy_requires_path_or_reference` / `deploy_with_neither_path_nor_ref_is_400` tests exercise the `(None, None)` arm through `deploy`; the `file://`-ref deploy path test exercises `(Some, _)`. They assert the same outcomes the helper produces.

Note: the helper's `BadRequest` message changed from `"deploy requires gtpack_path or reference"` to `"requires gtpack_path or reference"`. If any test asserts the exact deploy message substring, keep it passing by checking the assertion — tests should assert the `DeployError::BadRequest` variant / 400 status, not the exact string. If a test does string-match the old message, update it to match the new message.

- [ ] **Step 4: Format and commit**

Run: `rustfmt --edition 2024 crates/operax-manager/src/deployment.rs`

```bash
git add crates/operax-manager/src/deployment.rs
git commit -m "refactor(operax): extract shared resolve_effective_path for deploy"
```

---

### Task 2: `upgrade` accepts a reference and records `source_ref`

**Files:**
- Modify: `crates/operax-manager/src/deployment.rs` (`upgrade` signature + body; 4 test call-sites)
- Modify: `crates/operax-manager/src/serve.rs` (`UpgradeBody` struct + handler call)

**Interfaces:**
- Consumes: `resolve_effective_path` from Task 1.
- Produces: `pub fn upgrade(&self, id: &str, gtpack_path: Option<PathBuf>, reference: Option<String>) -> Result<DeploymentSummary, DeployError>`.

- [ ] **Step 1: Write/adjust the failing unit tests**

In `crates/operax-manager/src/deployment.rs` `#[cfg(test)] mod tests`, update the four existing `mgr.upgrade("x", fixture_gtpack())` call-sites to the new signature and add two new tests. The existing four become:

```rust
// upgrade_bumps_version_and_pushes_history
let summary = mgr.upgrade("up", Some(fixture_gtpack()), None).expect("upgrade");
```
```rust
// upgrade_bad_path_keeps_old_active
    .upgrade("keep", Some(std::path::PathBuf::from("/nonexistent/x.gtpack")), None)
```
```rust
// upgrade_unknown_id_is_not_found
let err = mgr.upgrade("ghost", Some(fixture_gtpack()), None).unwrap_err();
```
```rust
// (the history-cap test)
mgr.upgrade("cap", Some(fixture_gtpack()), None).expect("upgrade");
```

Add two new tests (place near the other upgrade tests). `fixture_gtpack()` returns a real `.gtpack` path; for the reference test, wrap its parent dir as a `file://` dir-ref (mirrors `deploy_by_ref_e2e`'s dir-ref approach — `fetch_pack_ref` passes a directory through as-is):

```rust
#[test]
fn upgrade_by_reference_records_source_ref() {
    let mgr = ready_manager_with_one("byref"); // helper that deploys id "byref" v1 by path
    let dir = fixture_gtpack_dir(); // directory holding the fixture pack
    let reference = format!("file://{}", dir.display());
    let summary = mgr
        .upgrade("byref", None, Some(reference.clone()))
        .expect("upgrade by reference");
    assert_eq!(summary.active_version, 2);
    let detail = mgr.get("byref").expect("deployment present");
    assert_eq!(detail.record.active.source_ref, Some(reference));
}

#[test]
fn upgrade_with_neither_path_nor_ref_is_bad_request() {
    let mgr = ready_manager_with_one("nada");
    let err = mgr.upgrade("nada", None, None).unwrap_err();
    assert!(matches!(err, DeployError::BadRequest(_)));
}
```

If a `ready_manager_with_one` / `fixture_gtpack_dir` helper does not already exist, add minimal ones next to the existing test helpers: `ready_manager_with_one(id)` builds a `DeploymentManager` and calls `deploy` with a `DeploySpec` whose `gtpack_path = Some(fixture_gtpack())` and `reference = None`; `fixture_gtpack_dir()` returns `fixture_gtpack().parent()` as a `PathBuf`. Reuse whatever the existing tests use to construct a manager + `DeploySpec` (copy that construction verbatim rather than inventing new fields).

- [ ] **Step 2: Run the new tests to verify they fail**

Cannot run locally (no cargo). The failing state is guaranteed: the new signature `upgrade(id, Option, Option)` does not yet exist and the call-sites won't compile. Proceed to implement; CI will confirm.

- [ ] **Step 3: Change the `upgrade` signature and body**

Replace the `upgrade` method's signature and its path-resolution + `source_ref` handling:

```rust
pub fn upgrade(
    &self,
    id: &str,
    gtpack_path: Option<PathBuf>,
    reference: Option<String>,
) -> Result<DeploymentSummary, DeployError> {
    let mut slots = self
        .slots
        .write()
        .map_err(|_| DeployError::Internal("lock poisoned".into()))?;
    if !slots.contains_key(id) {
        return Err(DeployError::NotFound);
    }
    // Resolve the effective path (fetch by reference, or use the bare path)
    // and load the NEW pack first; on failure the old version stays active.
    let local = self.resolve_effective_path(reference.as_deref(), gtpack_path)?;
    let pack = operax_pack_loader::load_operational_pack(&local)
        .map_err(|e| DeployError::PackLoad(e.to_string()))?;
    let digest = pack.pack_digest.clone();

    let slot = slots.get_mut(id).ok_or(DeployError::NotFound)?;
    let frozen_url = slot
        .record
        .sorx_url
        .as_deref()
        .unwrap_or("http://unresolved.discover");
    let client = (self.client_builder)(frozen_url, self.token.as_deref());
    // ManagerRuntime::new is infallible (returns Self).
    let runtime = ManagerRuntime::new(
        pack,
        slot.record.tenant.clone(),
        slot.record.team.clone(),
        slot.record.locale.clone(),
        None,
        client,
    );

    let next_version = slot.record.active.version + 1;
    let new_active = DeploymentVersion {
        version: next_version,
        gtpack_path: local,
        source_ref: reference,
        pack_digest: digest,
        deployed_at_unix: now_unix(),
    };
    let old_active = std::mem::replace(&mut slot.record.active, new_active);
    // ... (keep the remaining history-push / runtime-swap / persist / return
    //      logic exactly as it is today, unchanged below this line)
```

Keep everything after `std::mem::replace` (history push with cap, `slot.runtime` swap, `persist_locked`, building and returning the `DeploymentSummary`) exactly as the current implementation. Only the signature, the `local` resolution (now via helper, done before the `NotFound`-cleared borrow), and `source_ref` change.

Important ordering: keep the `if !slots.contains_key(id)` `NotFound` check BEFORE resolving/fetching, so an unknown id fails fast without a network fetch (matches today's early `NotFound`).

- [ ] **Step 4: Update the serve `UpgradeBody` and handler**

In `crates/operax-manager/src/serve.rs`, change:

```rust
#[derive(Deserialize)]
struct UpgradeBody {
    #[serde(default)]
    gtpack_path: Option<PathBuf>,
    #[serde(default)]
    reference: Option<String>,
}
```

and the handler call (was `mgr.upgrade(id, b.gtpack_path)`):

```rust
return match mgr.upgrade(id, b.gtpack_path, b.reference) {
```

Leave the surrounding match arms (success reply + `deploy_error_reply(err)` on error) unchanged — `deploy_error_reply` already maps `Fetch`/`BadRequest`/`NotFound`.

- [ ] **Step 5: Format and commit**

Run: `rustfmt --edition 2024 crates/operax-manager/src/deployment.rs crates/operax-manager/src/serve.rs`

```bash
git add crates/operax-manager/src/deployment.rs crates/operax-manager/src/serve.rs
git commit -m "feat(operax): upgrade accepts a pack reference; record source_ref"
```

---

### Task 3: e2e upgrade-by-reference + docs

**Files:**
- Modify: `crates/operax-manager/tests/deploy_by_ref_e2e.rs` (add an upgrade-by-ref case, or add a sibling test)
- Modify: `crates/operax-cli/README.md`

**Interfaces:**
- Consumes: the HTTP `deploy` then `upgrade` endpoints; `UpgradeBody`'s new `reference` field.

- [ ] **Step 1: Add the e2e test**

In `crates/operax-manager/tests/deploy_by_ref_e2e.rs`, add a test that (reusing the existing harness in that file for spinning up the server and posting JSON): deploys a deployment by path (or by the existing file-ref flow), then POSTs an upgrade with a JSON body `{ "reference": "file://<dir>" }` (no `gtpack_path`), and asserts a 200 with the active version bumped to 2 and `source_ref` recorded on `get`. Copy the server-spin-up + request helpers verbatim from the existing test in the same file; do not invent a new harness.

Body for the upgrade request:

```rust
let body = format!(r#"{{"reference":"file://{}"}}"#, dir.display());
```

Assert: response status line contains `200`, and a follow-up `GET /deployments/<id>` shows `"active":{...,"version":2}` and the `source_ref` string. Match the existing test's assertion style (it already parses the JSON body or greps the response text).

- [ ] **Step 2: Run the e2e test to verify**

Cannot run locally (no cargo). CI `perf` job (`cargo test --workspace --all-features`) runs it. Proceed; CI confirms.

- [ ] **Step 3: Update the README**

In `crates/operax-cli/README.md`, find the deploy-by-reference section and the "upgrade is path-only" limitation note. Remove the limitation note and add, near the upgrade docs:

- `upgrade` accepts the same optional `gtpack_path` / `reference` pair as `deploy` (mutually optional; `reference` wins; neither → 400).
- Same schemes (`oci://`, `http(s)://`, `repo://`, `store://`, `file://`, bare path) and same env-var bases (`GREENTIC_REPO_REGISTRY_BASE`, `GREENTIC_STORE_REGISTRY_BASE`).
- Same error codes: fetch failure → 502 `OPERAX_PACK_FETCH_FAILED`; neither field → 400 `OPERAX_BAD_REQUEST`.
- The upgraded version records `source_ref` when upgraded by reference.

- [ ] **Step 4: Format and commit**

Run: `rustfmt --edition 2024 crates/operax-manager/tests/deploy_by_ref_e2e.rs`

```bash
git add crates/operax-manager/tests/deploy_by_ref_e2e.rs crates/operax-cli/README.md
git commit -m "test(operax): upgrade-by-reference e2e; document upgrade parity"
```

---

## Self-Review

**Spec coverage:** shared resolver (§Design.1) → Task 1; `upgrade` signature + `source_ref` (§Design.2) → Task 2 steps 3; HTTP body (§Design.3) → Task 2 step 4; testing (§Testing) → Task 2 step 1 + Task 3 step 1; docs (§Docs) → Task 3 step 3. Error-handling table → reused `deploy_error_reply`, no new code (Task 2 step 4). All covered.

**Placeholder scan:** the two test-helper names (`ready_manager_with_one`, `fixture_gtpack_dir`) are specified with a fallback ("add minimal ones … copy that construction verbatim") because the exact existing test-helper shape isn't quoted here — the implementer reads the file and reuses what's there. This is a deliberate "reuse existing pattern" instruction, not a blank TODO.

**Type consistency:** `upgrade(id, Option<PathBuf>, Option<String>)` used consistently across Task 2 (definition), the 4 updated call-sites, and the serve handler `mgr.upgrade(id, b.gtpack_path, b.reference)` where `b.gtpack_path: Option<PathBuf>`, `b.reference: Option<String>`. `resolve_effective_path(Option<&str>, Option<PathBuf>) -> Result<PathBuf, DeployError>` matches its callers in `deploy` (`spec.reference.as_deref()`, `spec.gtpack_path.clone()`) and `upgrade` (`reference.as_deref()`, `gtpack_path`). Consistent.
