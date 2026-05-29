# greentic-operax

`greentic-operax` is the local and pilot command-line runner for OperaLa
operational handoff artifacts.

```bash
greentic-operax run examples/tenancy/handoff \
  --tenant demo-tenant \
  --team property-ops \
  --sorx-url http://localhost:8088 \
  --input examples/tenancy/banking/daily-transactions.json \
  --dry-run \
  --json
```
