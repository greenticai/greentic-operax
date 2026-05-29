# PR 18 — Decision envelope and policy gate

## Goal

All OperaX capabilities emit a standard decision envelope before applying actions.

## Decision envelope

```json
{
  "schema": "greentic.operax.decision.v1",
  "tenant": "demo-tenant",
  "team": "property-ops",
  "capability": "reconciliation",
  "input_id": "bank_tx_002",
  "outcome": "partial_payment",
  "confidence": 82,
  "matched_records": [
    {
      "record": "RentObligation",
      "id": "rent_002",
      "score": 82
    }
  ],
  "proposed_actions": [
    {
      "sorx_target": {
        "kind": "business_action",
        "id": "record_rent_payment",
        "version": "0.1.0",
        "contract_hash": "sha256:0000000000000000000000000000000000000000000000000000000000000000"
      },
      "operation": "invoke",
      "values": {},
      "idempotency_key": "bank_tx_002-record-rent-payment"
    },
    {
      "sorx_target": {
        "kind": "generated_route",
        "endpoint_id": "reconciliation_case.create",
        "method": "POST",
        "path": "/v1/agent/reconciliation-cases/create"
      },
      "operation": "invoke",
      "values": {},
      "idempotency_key": "bank_tx_002-create-case"
    }
  ],
  "requires_approval": true,
  "reasons": [
    "reference_matched",
    "date_within_window",
    "amount_less_than_expected"
  ],
  "audit": {
    "sorla_source_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
    "operala_handoff_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000"
  }
}
```

## Policy

Pilot policy:

- exact match above auto threshold: auto-apply
- partial payment: create settlement + case
- unmatched: create unallocated case
- ambiguous: create manual allocation case
- duplicate possible: manual review

## Acceptance criteria

- Decision created before SORX mutation.
- Dry-run prints decision, uses SORX business-action dry-run when available, and
  does not call mutating generated routes.
- Policy gate is generic and reads handoff metadata.
- Proposed actions resolve to locked SORX business actions or generated SORX
  routes.
- OperaX performs its policy gate first, then treats SORX `policy_denied` and
  `approval_required` responses as structured outcomes rather than bypassing
  SORX policy.
