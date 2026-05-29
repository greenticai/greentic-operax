# PR 19 — OperaX audit trail

## Goal

Write auditable JSONL events for every run.

## Events

```text
operax.run.started
operax.input.accepted
operax.decision.created
operax.action.applied
operax.action.skipped
operax.exception.created
operax.run.completed
operax.run.failed
```

## Required fields

```json
{
  "event": "operax.decision.created",
  "timestamp": "2026-06-02T10:00:00Z",
  "tenant": "demo-tenant",
  "team": "property-ops",
  "pack_digest": "sha256:...",
  "sorla_source_digest": "sha256:...",
  "operala_handoff_digest": "sha256:...",
  "sorx_target": {
    "kind": "business_action",
    "id": "record_rent_payment",
    "version": "0.1.0"
  },
  "input_id": "bank_tx_002",
  "outcome": "partial_payment"
}
```

## Acceptance criteria

- Default audit path: `target/operax/audit.jsonl`.
- `--audit-dir` overrides.
- Tenant/team included.
- SoRLa source digest and OperaLa handoff digest included when available.
- Request bodies and secret values are not written to audit events by default.
- SORX error codes such as `policy_denied`, `approval_required`,
  `provider_unavailable`, and contract hash errors are preserved.
- Errors are audited.
