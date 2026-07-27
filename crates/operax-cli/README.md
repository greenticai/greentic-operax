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
`operax run`. Each deployment is a `.gtpack` loaded for one tenant against
either a fixed SoRX URL or a discovered one (see "Static vs. discover mode"
below); the daemon keeps a disk-persisted registry of deployments so it can
restore them across restarts.

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

`team`, `locale`, `sorx_url`, and `sor` are optional; `id`, `gtpack_path`, and
`tenant` are required. **At least one of `sorx_url` / `sor` must be set** —
deploying with neither is rejected. The deployment's `id` is chosen by the
caller and is stable across upgrades.

### Static vs. discover mode

A deployment resolves its SoRX endpoint one of two ways, per deployment:

- **Static** (`sorx_url` set, no `sor`): every run talks to that fixed URL.
  This is the slice-1 behavior and is unchanged.
- **Discover** (`sor` set): every `run` request resolves the endpoint fresh
  from the SoRX presence directory, keyed by `(tenant, sor)`, picking the
  freshest reachable announcement. Discover mode is evaluated per run, not
  cached at deploy time, so a newly-announced SoRX instance is picked up
  without redeploying. If `sorx_url` is also supplied alongside `sor`, it is
  ignored for `run` — `sor` takes priority.

Discover mode requires the daemon to be built with the `events` feature and
started with `OPERAX_PRESENCE_NATS_URL` set: the presence subscriber that
maintains the directory is hosted **inside** `operax serve` itself, not a
separate command.

```bash
cargo build -p greentic-operax --features events

OPERAX_PRESENCE_NATS_URL=nats://localhost:4222 \
  greentic-operax serve --bind 127.0.0.1:8099
```

Without `OPERAX_PRESENCE_NATS_URL` set, or when the daemon was built without
`events`, there is no resolver: deployments and runs that rely on `sor` fail
with the errors below instead of falling back to anything.

### Deploy / run errors (discover mode)

| Status | Code | When |
| --- | --- | --- |
| `400` | `OPERAX_BAD_REQUEST` | Deploy request sets neither `sorx_url` nor `sor`. |
| `422` | `OPERAX_DISCOVERY_UNAVAILABLE` | Deploy request sets `sor` but the daemon has no resolver (built without `events`, or `OPERAX_PRESENCE_NATS_URL` unset). Also returned by `run` for an existing discover-mode deployment if the daemon no longer has an active resolver (e.g. restarted without `events`). |
| `503` | `OPERAX_SORX_UNRESOLVED` | Run request against a discover-mode deployment, but the presence directory has no reachable SoRX announced for `(tenant, sor)` yet. |

This slice resolves purely on `(tenant, sor)` and trusts the producer's
`reachable` flag as announced. Environment discrimination (e.g. staging vs.
production SoR instances) and presence health-probing (verifying reachability
instead of trusting the announcement) are planned follow-ups, not implemented
here.

### Business-event routing

When the daemon is built with the `events` feature **and** started with
`OPERAX_EVENTS_NATS_URL` set, `operax serve` also runs an in-process business-event
router: it subscribes to `greentic.events.>` (all tenants, not subject-scoped) and,
for each decoded event, routes it to **every** deployment whose pack declares a
matching `consumes` subscription for the matching tenant, running each one for real
(`dry_run=false`).

```bash
cargo build -p greentic-operax --features events

OPERAX_EVENTS_NATS_URL=nats://localhost:4222 \
  greentic-operax serve --bind 127.0.0.1:8099
```

A deployment's pack declares what it subscribes to via `consumes` entries in its
`operala.yaml`, each with a `capability` cap-URI in the form
`cap://greentic/events/<domain>/[vN/]<name>` (the version segment is optional). The
router matches an incoming event's `topic` against every `Ready` deployment's
declared subscriptions for the event's tenant; a match triggers a run of that
deployment with the event payload as input.

Without `OPERAX_EVENTS_NATS_URL` set, or when the daemon was built without `events`,
event routing is inert: the daemon logs that it's disabled and starts normally
otherwise.

This slice fans events out synchronously and does not yet stamp routed runs
distinctly from manual ones. Concretely, out of scope here:

- **`caller_role` is not stamped as a business event** — a run triggered by the
  router looks identical to a manually-triggered run to audit logging and any
  caller-role-based policy, unlike the separate `operax events subscribe` CLI
  command (which does stamp `caller_role: "business-event"` on its runs).
- **No environment discrimination** — matching is scoped by tenant only, so an
  event is routed to every matching deployment for that tenant regardless of
  environment (e.g. staging vs. production).
- **No concurrent fan-out** — when an event matches multiple deployments, the
  daemon runs them sequentially, one after another, not in parallel.
