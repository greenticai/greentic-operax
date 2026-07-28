# OperaX upgrade-by-reference — Design

## Goal

Bring `operax` **upgrade** to parity with **deploy**: an upgrade may name a
pack **reference** (`oci://`, `http(s)://`, `repo://`, `store://`, `file://`,
bare path) instead of only a local `.gtpack` path. This closes the
`upgrade`-is-path-only follow-up flagged by the deploy-by-reference review.

The slice reuses the `operax_pack_loader::fetch::fetch_pack_ref` primitive
shipped in deploy-by-reference (PR #14). It adds **no new dependencies** and
keeps `operax` on the greentic-types 1.1 (old) line — off the release-train.

## Non-goals

- No catalog listing / discovery of available packs (that is S4-proper,
  deferred).
- No cache eviction/GC changes (the managed cache is unchanged).
- No change to fetch semantics, scheme handling, or the digest-verification
  guard — all reused verbatim from deploy-by-reference.

## Current behavior

- `DeploymentManager::upgrade(id, gtpack_path: PathBuf)` loads a pack from a
  bare local path, swaps it in as the new active version, and records
  `source_ref: None` unconditionally.
- `UpgradeBody { gtpack_path: PathBuf }` (required) is the HTTP body.
- `deploy` already resolves an effective local path from
  `(reference, gtpack_path)`: `reference` wins → `fetch_pack_ref`; else the
  bare path; else `BadRequest`. It records `source_ref: reference`.

## Design

### 1. Shared resolver helper

Extract the effective-path resolution currently inline in `deploy`
(`deployment.rs`) into a private method so `deploy` and `upgrade` share one
source of truth:

```rust
/// Resolve the effective local pack path from an optional reference and an
/// optional bare path. A `reference` is fetched into the managed pack cache
/// and wins when both are set; a bare `gtpack_path` is used as-is; neither
/// set is a bad request.
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

`deploy` calls this in place of its inline `match`. Behavior is unchanged
for deploy (same precedence, same error variants).

### 2. `upgrade` signature

```rust
pub fn upgrade(
    &self,
    id: &str,
    gtpack_path: Option<PathBuf>,
    reference: Option<String>,
) -> Result<DeploymentSummary, DeployError>
```

- Resolve the effective path via `resolve_effective_path` **before** any
  mutation, so a bad path/reference leaves the old version active with no
  downtime (the existing "load first" invariant is preserved — fetch also
  happens before the swap).
- The new `DeploymentVersion` records `source_ref: reference` (no longer a
  hardcoded `None`).
- `NotFound` when the id is unknown is checked first, as today.

### 3. HTTP body

```rust
#[derive(Deserialize)]
struct UpgradeBody {
    #[serde(default)]
    gtpack_path: Option<PathBuf>,
    #[serde(default)]
    reference: Option<String>,
}
```

Backward-compatible: an existing body sending only `gtpack_path` still
deserializes into `Some(path)`. The serve handler calls
`mgr.upgrade(id, b.gtpack_path, b.reference)` and maps errors through the
existing `deploy_error_reply` (Fetch → 502 `OPERAX_PACK_FETCH_FAILED`,
BadRequest → 400 `OPERAX_BAD_REQUEST`).

## Error handling

Identical to deploy-by-reference — no new error codes:

| Condition                         | Result                          |
|-----------------------------------|---------------------------------|
| Unknown deployment id             | `NotFound` → 404                |
| Neither path nor reference        | `BadRequest` → 400              |
| Reference fetch fails             | `Fetch` → 502                   |
| Pack load fails (bad content)     | `PackLoad` → old version stays  |

## Testing

- **Unit** (`deployment.rs` tests):
  - upgrade by `reference` (a `file://` dir ref) bumps the version and
    records `source_ref = Some(ref)`.
  - upgrade with neither path nor reference → `BadRequest`.
  - existing path-based upgrade tests updated to the new signature and keep
    passing (`source_ref` stays `None` for the bare-path arm).
- **e2e** (`deploy_by_ref_e2e` or a sibling): deploy by path, then upgrade
  the same deployment by `file://` dir reference over HTTP, asserting the
  active version bumps and `source_ref` is recorded.

## Docs

Update `crates/operax-cli/README.md`: remove the "upgrade is path-only"
limitation note and document that upgrade accepts the same optional
`gtpack_path` / `reference` pair as deploy, with the same schemes and error
codes.

## Boundaries preserved

- No new dependency; `operax` stays greentic-types 1.1 (old line).
- No `unwrap()`/`panic!()`/`expect()` outside tests.
- English-only in source/tests/commits; Conventional Commits; no
  AI-authorship trailer on operax commits.
