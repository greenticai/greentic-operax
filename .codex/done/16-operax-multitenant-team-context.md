# PR 16 — OperaX tenant and optional team context

## Goal

Make OperaX pilot-safe for multi-tenant usage.

## Context model

```json
{
  "tenant": "demo-tenant",
  "team": "property-ops",
  "locale": "en-GB",
  "caller_id": "operax",
  "caller_role": "service",
  "request_id": "uuid",
  "pack_digest": "sha256:..."
}
```

## Rules

- Tenant is required.
- Team is optional.
- Tenant/team must be included in:
  - SORX HTTP headers
  - audit events
  - decision envelopes
  - metric labels
- SORX HTTP uses `X-Greentic-Tenant-Id`, `X-Greentic-Caller-Id`,
  `X-Greentic-Caller-Role`, optional `X-Greentic-Team`, and `Accept-Language`.
- Pack may declare `team_required: true`; if so, missing team is an error.

## Acceptance criteria

- `operax run` fails without tenant.
- Team is passed when present.
- Audit and decisions include tenant/team.
- Caller context is always available for SORX requests.
