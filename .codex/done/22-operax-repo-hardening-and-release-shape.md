# PR 22 — OperaX repo hardening and release shape

## Goal

Mirror the proven SORX repository shape for checks, scripts, localization,
coverage, release readiness, and `.codex` task history before OperaX reaches a
customer pilot.

Use `greentic-pack-lib` for every `.gtpack` read/validation path. OperaX should
not maintain its own pack archive parser.

## Required files

```text
ci/
  local_check.sh
  release_check.sh
scripts/
  local_check.sh
  test_operax.sh
tools/
  i18n.sh
  sync_cli_i18n.sh
i18n/
  en.json
  locales.json
crates/operax-cli/i18n/
coverage-policy.json
.github/workflows/
  ci.yml
  customer-pilot-demo.yml
  nightly-coverage.yml
  perf.yml
  release.yml
  publish.yml
  release-binaries.yml
.codex/
  done/
  repo_overview.md
  repo_overview_task.md
  global_rules.md
```

## Local check behaviour

`ci/local_check.sh` should follow SORX:

- `cargo fmt --all -- --check`
- `tools/i18n.sh validate` and `tools/i18n.sh status` when present
- `bash ci/release_check.sh`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-targets --all-features`
- `cargo build --all-features`
- `cargo doc --no-deps --all-features`
- `cargo package`, `cargo package --no-verify`, and `cargo publish --dry-run`
  for publishable crates whose local workspace dependencies are already
  published or absent

`scripts/local_check.sh` must just delegate to `ci/local_check.sh`.

## Release metadata guard

`ci/release_check.sh` should verify:

- workspace package version matches the publishable CLI crate version
- the binary target is exactly `operax` or `greentic-operax`, whichever the
  final CLI package declares
- CLI package includes `i18n/**`
- root `i18n/*.json` and embedded CLI i18n catalogs are byte-for-byte in sync
- `package.metadata.binstall` matches the release archive naming used by
  `release-binaries.yml`
- publish workflow only releases from main/master, creates or verifies the
  matching `vX.Y.Z` tag, dispatches `release-binaries.yml` on that tag, waits
  for the binary workflow, then publishes crates in dependency order

## i18n

Use the SORX `tools/i18n.sh` pattern:

- `translate`, `validate`, `status`, and `all` modes
- local fallback validation when `greentic-i18n-translator` is unavailable
- `tools/sync_cli_i18n.sh` copies root catalogs into
  `crates/operax-cli/i18n/`
- all CLI/help/error text goes through the i18n layer

## Coverage and performance

- `coverage-policy.json` starts with SORX-style 60% global and per-file line
  minimums.
- Thin binary entrypoints may be excluded.
- `nightly-coverage.yml` installs `cargo-nextest`, `cargo-llvm-cov`, and
  `greentic-dev`, then runs `greentic-dev coverage`.
- `perf.yml` runs normal tests plus lightweight perf guard tests and a small
  benchmark smoke run once an `operax-cli` benchmark exists.

## Scripts

`scripts/test_operax.sh` should be the manual demo helper analogous to SORX
`scripts/test_sorx.sh`. It should:

- accept a handoff directory or pilot `.gtpack`
- accept `--sorx-url`, `--input`, `--tenant`, `--team`, `--dry-run`, and
  `--audit-dir`
- optionally start or point at a mock SORX server for local demo validation
- never embed concrete customer SORX URLs or secrets

## .codex history

As PRs are implemented, move completed planning notes to `.codex/done/` and
keep a living `repo_overview.md`. This mirrors SORX and gives future coding
agents a compact repo map plus historical implementation context.

## Acceptance criteria

- `bash ci/local_check.sh` is the single documented pre-push command.
- CI uses the same local check command where practical.
- i18n catalogs validate and stay synced into the CLI crate.
- Coverage, perf, release readiness, publish, and release-binary workflows are
  present.
- `.codex` has current overview/global guidance plus done-history for completed
  PRs.
