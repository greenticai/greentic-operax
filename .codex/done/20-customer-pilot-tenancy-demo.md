# PR 20 — Customer pilot tenancy rent reconciliation demo

## Goal

Create a full demo showing:

1. OperaX runs a reconciliation handoff artifact or pilot `.gtpack` produced by
   OperaLa.
2. OperaX runs the artifact against a SORX HTTP endpoint.
3. Single and daily batch banking JSON are both supported.
4. SORX is updated through locked business-action calls or generated
   `/v1/agent/...` routes.

## Demo structure

```text
examples/tenancy/
  banking/
    one-transaction.json
    daily-transactions.json
  sorx-fixtures/
    initial-state.json
    expected-after-single.json
    expected-after-batch.json
  mock-sorx/
    README.md
  expected/
    readiness.report.yaml
    decisions.batch.json
```

## Demo command sequence

```bash
operax run ./target/gtpacks/tenancy-rent-reconciliation.gtpack \
  --tenant demo-tenant \
  --team property-ops \
  --sorx-url http://localhost:8088 \
  --input ./examples/tenancy/banking/one-transaction.json

operax run ./target/gtpacks/tenancy-rent-reconciliation.gtpack \
  --tenant demo-tenant \
  --team property-ops \
  --sorx-url http://localhost:8088 \
  --input ./examples/tenancy/banking/daily-transactions.json
```

## Expected results

```text
bank_tx_001 → matched → Payment created → RentObligation marked paid
bank_tx_002 → partial_payment → Payment created → ReconciliationCase created
bank_tx_003 → unmatched → ReconciliationCase created reason=unallocated_cash
```

## Acceptance criteria

- Demo can be run locally.
- Mock SORX HTTP server verifies expected calls.
- Mock SORX exposes `/healthz`, `/readyz`, `/v1/sorx/routes`, and business
  action dry-run/invoke endpoints used by the generated handoff.
- README includes exact commands.
- CI runs demo in dry-run and mock-write modes.
- Demo validates embedded OperaLa handoff metadata and build lock when present.
