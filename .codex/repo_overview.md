# Repository Overview

## 1. High-Level Purpose

`greentic-operax` is the OperaX local/pilot runtime for OperaLa operational
handoff artifacts and optional pilot `.gtpack` compatibility artifacts. It runs
runtime inputs such as reconciliation banking events, validates handoff metadata,
dispatches single or batch inputs, emits decision envelopes, applies policy
gates, calls SORX HTTP runtime surfaces when writes are allowed, and writes JSONL
audit records.

The repository now follows the SORX-inspired workspace shape: runtime logic is
split into focused crates, local checks run through `ci/local_check.sh`, i18n
catalogs are synced into the CLI crate and real available catalogs are embedded
in the binary, coverage/release/pilot workflows are present, and `.codex`
includes overview/global guidance plus done-history.

## 2. Main Components and Functionality

- **Path:** `Cargo.toml`
  - **Role:** Rust workspace manifest.
  - **Key functionality:** Defines workspace members for CLI, core types,
    loader, SORX HTTP client, dispatcher, policy, and audit crates.

- **Path:** `crates/operax-core`
  - **Role:** Shared OperaX runtime contracts.
  - **Key functionality:** Defines `OperaxError`, `OperaxContext`, handoff pack
    metadata, SORX targets, proposed actions, decision envelopes, run reports,
    SHA-256 digest helpers, SORX headers, and secret-like value detection.

- **Path:** `crates/operax-pack-loader`
  - **Role:** OperaLa handoff and pilot `.gtpack` loader.
  - **Key functionality:** Loads handoff directories, validates
    `operala.yaml`/`operala-handoff.json`, checks schema/capability/SORX binding,
    collects flow and schema files, computes digests, rejects secret-like values,
    and uses `greentic-pack-lib` for `.gtpack` archive loading and validation.

- **Path:** `crates/operax-dispatch`
  - **Role:** Input detection and reconciliation decision generation.
  - **Key functionality:** Detects single bank transaction and daily batch input
    shapes, splits batches, and emits deterministic reconciliation decisions with
    SORX business-action or generated-route proposed actions.

- **Path:** `crates/operax-policy`
  - **Role:** Generic pilot policy gate.
  - **Key functionality:** Auto-applies high-confidence exact matches and marks
    partial/unmatched/approval-required outcomes as requiring approval.

- **Path:** `crates/operax-sorx-http`
  - **Role:** SORX HTTP client contracts and simple HTTP implementation.
  - **Key functionality:** Models SORX health, route metadata, business actions,
    business-action calls, generated-route calls, a mock client, and an HTTP
    client for SORX discovery/action invocation using SORX context headers.

- **Path:** `crates/operax-audit`
  - **Role:** JSONL audit sink.
  - **Key functionality:** Writes run, input, decision, action, completion, and
    failure audit events without logging request bodies or secret values.

- **Path:** `crates/operax-cli`
  - **Role:** `greentic-operax` binary package.
  - **Key functionality:** Provides `greentic-operax run <artifact> --tenant
    --sorx-url --input`, optional team/locale/dry-run/audit-dir/json flags,
    loads artifacts, dispatches decisions, applies policy, writes audit, and
    conditionally calls SORX. The package includes binstall metadata for
    `greentic-operax-v{version}-{target}.tgz` release assets.

- **Path:** `examples/tenancy`
  - **Role:** Customer pilot tenancy reconciliation demo fixture.
  - **Key functionality:** Contains single and batch banking JSON inputs, mock
    SORX initial state, a local OperaLa handoff fixture, and demo commands.

- **Path:** `ci/`, `scripts/`, `tools/`, `i18n/`, `.github/workflows/`,
  `coverage-policy.json`
  - **Role:** SORX-style repo operations.
  - **Key functionality:** Local check wrapper, release metadata guard, i18n
    validation/sync helpers for a 66-locale target list, coverage policy, CI,
    customer pilot demo, coverage, perf, release, publish, and release-binary
    workflow stubs.

- **Path:** `.codex/`
  - **Role:** Planning/history/orientation material.
  - **Key functionality:** Contains SORX summary, global rules, overview
    maintenance task, current overview, decision schema, runtime examples, and
    completed PR notes in `.codex/done/`.

## 3. Work In Progress, TODOs, and Stubs

- The SORX HTTP client is intentionally minimal and supports plain `http://`
  URLs. It is suitable for local/mock pilot use, not a full production HTTP
  stack.
- The publish and release-binary workflows are present but publishing is disabled
  because workspace crates are marked `publish = false`.
- The mock SORX server is documented but not implemented as a standalone
  process; unit tests use `MockSorxClient` and the demo uses CLI dry-run mode.
- The reconciliation dispatch logic is deterministic pilot logic, not a general
  flow/WASM interpreter yet.
- Non-Dutch non-English i18n catalogs are intentionally absent until
  `tools/i18n.sh translate` creates real translations. Placeholder catalogs that
  are identical to English should be deleted because they confuse translation.

## 4. Broken, Failing, or Conflicting Areas

- No known failing tests after `cargo test --workspace --all-targets
  --all-features`.
- `ci/local_check.sh` may require network/cache availability for dependencies
  and cargo documentation on a clean machine.

## 5. Notes for Future Work

- Replace the simple HTTP implementation with the approved Greentic HTTP/client
  stack when available.
- Keep all `.gtpack` interactions routed through `greentic-pack-lib`.
- Add a real mock SORX HTTP server for mock-write demo CI.
- Generate decision schemas from Rust types once schema generation is wired.
- Move completed planning notes into `.codex/done/` after each implementation
  slice and keep this overview refreshed.
