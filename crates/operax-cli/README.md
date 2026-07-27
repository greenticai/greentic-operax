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

## `operax serve`

`operax serve` starts a long-running daemon that manages **multiple** OperaLa
deployments over HTTP, instead of the single ad-hoc run performed by
`operax run`. Each deployment is a `.gtpack` loaded for one tenant against a
fixed SoRX URL; the daemon keeps a disk-persisted registry of deployments so
it can restore them across restarts.

```bash
greentic-operax serve \
  --bind 127.0.0.1:8099 \
  --registry ~/.greentic/operax/deployments.json \
  --secret "$OPERAX_SERVE_SECRET" \
  --sorx-token-env SORX_TOKEN
```

### Flags

| Flag | Default | Description |
| --- | --- | --- |
| `--bind` | `127.0.0.1:8099` | Address the HTTP server listens on. |
| `--registry` | `~/.greentic/operax/deployments.json` | Path to the persisted deployment registry JSON. |
| `--secret` | _(unset)_ | Optional shared secret. When set, every route except the health checks requires `Authorization: Bearer <secret>` or `x-greentic-sorx-secret: <secret>`. When unset, the daemon is open (local-dev convenience). |
| `--sorx-token-env` | `SORX_TOKEN` | Environment variable holding the SoRX bearer token used for all deployments. |

### Routes

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/v1/operax/deployments` | Deploy a new `.gtpack` under a client-provided id. |
| `GET` | `/v1/operax/deployments` | List all deployments and their status. |
| `GET` | `/v1/operax/deployments/{id}` | Get details for one deployment. |
| `PUT` | `/v1/operax/deployments/{id}` | Upgrade a deployment to a new `.gtpack` (versioned replace, zero-downtime unless the new pack fails to load). |
| `DELETE` | `/v1/operax/deployments/{id}` | Remove a deployment. |
| `POST` | `/v1/operax/deployments/{id}/run` | Run an input against a deployed artifact. |
| `GET` | `/healthz`, `/readyz` | Health checks (never require the shared secret). |

### Deploy request body

```json
{
  "id": "acme-daily-transactions",
  "gtpack_path": "/path/to/handoff.gtpack",
  "tenant": "acme",
  "team": "property-ops",
  "locale": "en",
  "sorx_url": "http://localhost:8088"
}
```

`team` and `locale` are optional; every other field is required. The
deployment's `id` is chosen by the caller and is stable across upgrades.

This slice keeps a single, static `sorx_url` per deployment. Dynamic SoRX
discovery (locating the right SoRX instance automatically) and routing
inbound business events to the matching deployment are planned as follow-on
slices and are not implemented yet.
