# PR 13 — OperaX runtime scaffold

## Goal

Create `greentic-operax` as a local/pilot runner for OperaLa operational
handoff artifacts and optional pilot `.gtpack` compatibility artifacts.

## Workspace shape

Follow the SORX workspace pattern instead of keeping logic in a root binary:

```text
crates/
  operax-cli/
  operax-core/
  operax-pack-loader/
  operax-sorx-http/
  operax-dispatch/
  operax-policy/
  operax-audit/
```

The root package can remain a thin compatibility wrapper only if needed. Runtime
logic belongs in crates with focused tests.

## CLI

```bash
operax run ./target/gtpacks/tenancy-rent-reconciliation.gtpack --tenant demo-tenant --input ./input.json --sorx-url https://sorx.example
operax run ./target/operala/tenancy-rent-reconciliation --tenant demo-tenant --input ./input.json --sorx-url https://sorx.example
```

Required:

```text
--tenant
--sorx-url
--input
```

Optional:

```text
--team
--locale
--dry-run
--audit-dir
```

## Responsibilities

OperaX must:

- load a handoff directory or pilot `.gtpack`
- validate OperaLa handoff metadata embedded in the artifact
- detect input schema
- execute ingress flow
- call SORX HTTP endpoints for SoRLa-declared actions or agent endpoints
- emit decision envelopes
- apply policy gates
- write audit records

## Acceptance criteria

- CLI compiles.
- Workspace crates are split by responsibility.
- Can load a local OperaLa handoff directory or pilot `.gtpack`.
- Generated artifacts must not contain plaintext secrets, provider credentials,
  or concrete customer SORX URLs.
- Requires tenant.
- Team is optional.
- Dry-run does not call write operations.
