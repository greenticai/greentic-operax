# PR 17 — OperaX input dispatcher for single and batch banking data

## Goal

OperaX should detect whether input JSON is one transaction or a daily batch and route accordingly.

## Supported input forms

Single:

```json
{
  "transaction_id": "bank_tx_001",
  "booked_at": "2026-06-02",
  "amount": 1250.0,
  "currency": "GBP",
  "reference": "TEN-001 June Rent"
}
```

Batch:

```json
{
  "source": "bank_daily_export",
  "date": "2026-06-02",
  "transactions": []
}
```

## Behaviour

```text
single transaction → ingest-transaction.flow.yaml
daily batch → ingest-daily-transactions.flow.yaml → split → reconcile-one
```

## Acceptance criteria

- Both input shapes are accepted.
- Unknown input shape gives localized error.
- Batch result aggregates decisions.
