# Tenancy Reconciliation Demo

Run a dry-run against the checked-in OperaLa handoff fixture:

```bash
cargo run -p greentic-operax --bin greentic-operax -- run examples/tenancy/handoff \
  --tenant demo-tenant \
  --team property-ops \
  --sorx-url http://localhost:8088 \
  --input examples/tenancy/banking/daily-transactions.json \
  --dry-run \
  --json
```

Expected outcomes:

- `bank_tx_001` becomes `matched`
- `bank_tx_002` becomes `partial_payment`
- `bank_tx_003` becomes `unmatched`
