# greentic-sorx Summary

`../greentic-sorx` is the Greentic System of Record eXecutor, or SORX. Its job is to consume packaged SorLa `.gtpack` artifacts and run them as validated local/system-of-record runtimes. SorLa owns authoring and pack production; SORX owns loading, validation, startup answers, provider binding, HTTP/MCP execution surfaces, policy and approval enforcement, audit events, deployments, and local/e2e execution.

## Repository Shape

- Rust workspace, edition 2024, version `0.1.7`.
- Workspace crates:
  - `crates/greentic-sorx-core`: runtime model, startup planning, router, providers, policy, approvals, audit, deployments, GHCR webhook handling, MCP runtime adapter, manager surfaces, ontology/evidence helpers, and in-memory execution.
  - `crates/greentic-sorx-pack`: `.gtpack` loader, inspector, doctor checks, runtime metadata validation, validation-suite asset loading, ontology/business-action metadata checks, and path/digest safety.
  - `crates/greentic-sorx-cli`: `greentic-sorx` binary, command parsing, localized help, local HTTP adapter, validation runner, deployment commands, webhook fixture commands, and e2e wiring.
- Supporting areas:
  - `docs/`: behavior notes for commands, answers, security, deployments, MCP, provider bindings, validation suites, observability, ontology, business manager views, release, and design/audit history.
  - `scripts/` and `ci/`: local checks and landlord/tenant e2e entrypoints.
  - `i18n/` and `crates/greentic-sorx-cli/i18n/`: CLI localization source and embedded synced translations.
  - `tests/`, crate tests, and Playwright configuration for e2e/webchat-style checks.

## Main Runtime Flow

Typical usage:

```bash
greentic-sorla pack landlord.sorla --out landlord.gtpack
greentic-sorx doctor landlord.gtpack
greentic-sorx start landlord.gtpack --schema --json
greentic-sorx start landlord.gtpack --answers landlord.answers.json
```

At startup, SORX validates the archive and runtime metadata, reads the startup schema, validates and normalizes answers, rejects suspicious inline secrets, resolves provider bindings, builds generated routes from `assets/sorla/agent-gateway.json`, loads MCP tool metadata, applies policy and approval rules, configures audit sinks, and starts a local HTTP runtime.

## Command Surface

Important commands include:

- Pack metadata and validation: `doctor`, `inspect`, `routes`, `mcp-tools`, `graph ...`, `evidence query`.
- Runtime startup: `start --schema`, `start --answers`, `start --dry-run`, `start --emit-answers`, `run`.
- MCP adapter planning: `mcp start`.
- Validation suites: `validate`, `validation report`.
- Deployment registry: `deployments list/inspect/create/validate/activate/promote/rollback/retire-old/public-routes/promotion-status/retire`.
- Aliases: `aliases set/list`.
- GHCR webhook fixtures: `webhook verify-fixture`, `webhook replay`.

Stable exit codes are documented in `README.md` and `docs/commands.md`; notable ones are `3` for pack validation failure, `4` for startup answer validation failure, `5` for provider resolution failure, and `6` for runtime startup failure.

## Providers, Policy, and Boundaries

- The in-memory provider is real, deterministic, and used for local development and CI.
- Provider operations are namespaced by tenant, pack, and version so local runs can isolate records.
- FoundationDB is intentionally only an unavailable adapter boundary for now. It accepts the config shape but returns a clear unavailable error until a SORX-compatible store provider exists.
- Mutating routes and MCP calls go through the same runtime path: route resolution, input validation, policy decision, optional approval, provider invocation, idempotency, and audit emission.
- `mcp start` currently validates answers and emits an adapter runtime plan. Full MCP server transport is future work.
- `validate` runs declarative validation suites embedded in packs and must not execute arbitrary pack code.

## Local HTTP Runtime

The CLI runtime serves health/readiness, route/tool metadata, deployment route/status endpoints, manager endpoints, and generated pack routes. Generated mutating routes share the same core execution path as direct and MCP invocations.

Common built-in endpoints include:

- `GET /healthz`
- `GET /readyz`
- `GET /v1/sorx/routes`
- `GET /v1/sorx/public-routes`
- `GET /v1/sorx/tools`
- `GET /v1/sorx/deployments/local/routes`
- `GET /v1/sorx/deployments/local/promotion-status`
- `/v1/sorx/manager/...` plus `/manager/...` aliases

## Testing and Checks

Primary local check:

```bash
bash ci/local_check.sh
```

This runs formatting, i18n validation/status, release metadata validation, clippy, tests, build, docs, and packaging/publish dry-run checks where possible.

Landlord/tenant e2e:

```bash
bash scripts/e2e/run-landlord-tenant.sh --provider memory
```

The e2e scenario builds a deterministic `.gtpack`, runs `doctor`, starts the local HTTP runtime, exercises generated landlord/property/unit/tenant/tenancy/payment/maintenance routes, checks idempotency, verifies high-risk approval-required behavior, and confirms MCP tool listing.

## Coding-Agent Notes

- Preserve the `.gtpack` runtime contract; do not add loose-folder runtime behavior unless explicitly requested.
- Keep SorLa authoring/packing responsibilities out of SORX.
- Keep memory-provider behavior deterministic.
- Treat FoundationDB support as a boundary, not as implemented storage.
- Keep public deployment promotion gated by a passing validation report for the same deployment ID and pack digest.
- Keep CLI translations synced after changing user-facing CLI text:

```bash
tools/i18n.sh validate
tools/i18n.sh status
bash tools/sync_cli_i18n.sh
```

- Run `bash ci/release_check.sh` after touching Cargo metadata or workflows.

