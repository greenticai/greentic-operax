# PR 23 — OperaX manager runtime and `test <pack>` foundation

## Goal

Add the first executable foundation for `greentic-operax test <pack>`: a local
OperaX manager HTTP runtime that can load the same OperaLa handoff artifacts as
`run`, expose Adaptive Cards, accept single-item or batch JSON, process it
through the shared decision path, and return flow-embeddable run results.

The SORX WebChat bundle flow remains the target integration shape, but the
bundle creation and WebChat hook patching are split into PR 24 so this PR can
land the reusable manager/runtime surface without mixing two large concerns.

## SORX Pattern Applied

Follow the current Rust implementation behind `greentic-sorx test`, especially
`crates/greentic-sorx-cli/src/test_runtime.rs`, not the older
`scripts/test_sorx.sh` helper.

The relevant SORX ideas for this foundation are:

- expose provider-neutral manager Adaptive Cards over HTTP;
- keep card payloads as intent only and derive trusted tenant/team/runtime
  context server-side;
- make flow/API callers able to post prepared JSON directly to the running
  manager; and
- share the normal runtime path rather than creating a parallel decision engine.

## Implemented Shape

Add reusable runtime code, shared by `run` and `test`:

```rust
RunRequest
run_artifact_with_client(...)
run_pack_with_client(...)
run_loaded_pack(...)
```

Add an `operax-manager` crate with:

- in-memory recent run storage;
- Adaptive Card renderers for dashboard, input, decision summary, and errors;
- secret-like JSON rejection before dispatch;
- local HTTP serving for manager endpoints; and
- flow/API submission support.

Add `greentic-operax test <artifact>`:

```text
greentic-operax test <artifact>
  --tenant <TENANT>
  --team <TEAM>
  --sorx-url <URL>
  --operax-url <URL>
  --audit-dir <DIR>
  --locale <LOCALE>
```

The command starts the local OperaX manager and prints the dashboard, flow, and
SORX runtime URLs.

## Manager HTTP Surface

Implemented canonical and short aliases:

```text
GET  /healthz
GET  /readyz
GET  /v1/operax/manager
GET  /v1/operax/manager/view
GET  /v1/operax/manager/cards/dashboard
GET  /v1/operax/manager/cards/input
GET  /v1/operax/manager/cards/decisions/{run_id}
POST /v1/operax/manager/submit
POST /v1/operax/runs
GET  /v1/operax/runs/{run_id}
```

Flow endpoint request shape:

```json
{
  "input": { "...": "single item or batch JSON" },
  "mode": "dry_run",
  "idempotency_key": "optional-flow-step-key",
  "return_card": true
}
```

Response shape:

```json
{
  "schema": "greentic.operax.run-result.v1",
  "run_id": "run_...",
  "report": { "input_count": 3, "decisions": [] },
  "card": { "type": "AdaptiveCard" }
}
```

## Security and Safety

- Default manager submissions are dry-run.
- Applying actions uses the shared runtime and existing policy gate.
- Card submissions do not trust tenant/team/SORX URL from the card body.
- Secret-like values in uploaded/pasted JSON are rejected.
- Uploaded JSON is not stored in the run history; recent runs store report/card
  state only.

## Tests

Covered by package tests:

- Adaptive Card renderer emits `metadata.schema =
  greentic.operax.manager-card.v1`.
- Manager submit rejects invalid JSON and returns an error card.
- Manager submit accepts batch JSON and returns a card.
- Flow endpoint returns `greentic.operax.run-result.v1`.
- Secret-like uploaded payloads are rejected.
- Shared runtime applies actions through the SORX client abstraction.

## Acceptance Criteria

- `greentic-operax test <pack>` starts a local manager runtime.
- `/v1/operax/manager/cards/dashboard` returns an OperaX manager Adaptive Card.
- `/v1/operax/manager/submit` supports pasted JSON and returns decision cards.
- `/v1/operax/runs` supports flow-prepared JSON and returns a run result.
- Real execution shares the existing `run` path.
- Focused runtime, manager, and CLI tests pass.

## Follow-Up

PR 24 covers the remaining SORX-style WebChat bundle preparation:

- temporary WebChat bundle creation;
- `--webchat-url`, `--bundle-dir`, `--answers`, `--force`, and `--no-start`;
- WebChat hook patching for OperaX manager card actions; and
- Playwright/end-to-end WebChat validation.
