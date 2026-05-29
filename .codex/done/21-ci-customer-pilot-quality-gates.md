# PR 21 — CI and pilot quality gates

## Goal

Add SORX-style CI checks needed before first customer pilot.

OperaX should have one local check command that developers and CI both run:

```bash
bash ci/local_check.sh
```

`scripts/local_check.sh` should be a compatibility wrapper that delegates to
`ci/local_check.sh`, matching the SORX repo.

## Required checks

- `cargo fmt`
- i18n validation/status when `tools/i18n.sh` exists
- release metadata validation via `bash ci/release_check.sh`
- `cargo clippy --all-targets --all-features`
- `cargo test --workspace --all-targets --all-features`
- `cargo build --all-features`
- `cargo doc --no-deps --all-features`
- crates.io packaging / publish dry-run checks for publishable crates, in
  dependency order, with local workspace dependency deferral like SORX
- schema validation tests
- golden fixture tests
- end-to-end demo dry-run
- end-to-end demo against mock SORX HTTP server
- i18n key coverage
- handoff validation
- optional pilot pack validation
- coverage policy enforcement with `greentic-dev coverage`
- lightweight performance guard tests and a benchmark smoke run once hot paths
  exist
- CI rejects generated artifacts containing plaintext secrets, concrete SORX
  customer URLs, or legacy SoRLa `entities`/`functions` examples.

## Required workflows

```text
.github/workflows/ci.yml
.github/workflows/customer-pilot-demo.yml
.github/workflows/nightly-coverage.yml
.github/workflows/perf.yml
.github/workflows/release.yml
.github/workflows/publish.yml
.github/workflows/release-binaries.yml
```

`ci.yml` may call the Greentic shared host-crate CI workflow as SORX does, but
the repo must still keep `ci/local_check.sh` as the canonical local entrypoint.

## Acceptance criteria

- PR cannot merge if demo breaks.
- Golden files must be intentionally updated.
- CI uploads demo artifacts on failure.
- `bash ci/local_check.sh` is the documented single command before pushing.
- `coverage-policy.json` exists and excludes only thin binary entrypoints.
- Release workflows verify binary names, version metadata, i18n sync, binstall
  metadata, release archive naming, and publish order.
