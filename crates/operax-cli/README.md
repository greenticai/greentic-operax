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
  "sorx_url": "http://localhost:8088",
  "environment": "prod"
}
```

`team`, `locale`, `sorx_url`, `sor`, and `environment` are optional; `id` and
`tenant` are required. **At least one of `sorx_url` / `sor` must be set** —
deploying with neither is rejected. The deployment's `id` is chosen by the
caller and is stable across upgrades.

`gtpack_path` and `reference` are each optional, but **at least one of the two
must be set** — deploying with neither is rejected. `gtpack_path` is a local
path to a `.gtpack` file or an already-unpacked handoff directory, loaded
as-is. `reference` is a pack reference resolved and fetched before load (see
"Deploy by reference" below). If both are set, `reference` takes precedence —
the fetched pack is what gets loaded, and `gtpack_path` is ignored.

`environment` is a free-form label (e.g. `"prod"`, `"staging"`) scoping this
deployment to a single environment. Omitted (the default) means a wildcard:
the deployment resolves discovery and receives routed events regardless of
environment — the unchanged slice-1/2/3 behavior. Set, it restricts both
discover-mode resolution and business-event routing to that environment (see
below); it has no effect on static (`sorx_url`-only) deployments' `run`
behavior.

### Deploy by reference

Instead of (or alongside) a local `gtpack_path`, a deploy request can supply
`reference`: a pack reference the daemon resolves and fetches on your behalf.

```json
{
  "id": "acme-daily-transactions",
  "reference": "oci://ghcr.io/acme/handoff@sha256:abc123...",
  "tenant": "acme",
  "sorx_url": "http://localhost:8088"
}
```

Supported `reference` schemes:

| Scheme | Resolves to |
| --- | --- |
| `oci://<registry>/<name>@<digest\|tag>` | Pulled directly from the given OCI registry. |
| `repo://<name>` | Pulled as an OCI reference `{GREENTIC_REPO_REGISTRY_BASE}/<name>`. |
| `store://<name>` | Pulled as an OCI reference `{GREENTIC_STORE_REGISTRY_BASE}/<name>`. |
| `http://<url>` / `https://<url>` | Downloaded directly. |
| `file://<path>` (or a bare local path) | Read from local disk. A path that is a directory is used as-is (unpacked handoff dir); a path that is a file is cached like any other fetched artifact. |

`repo://` and `store://` require `GREENTIC_REPO_REGISTRY_BASE` /
`GREENTIC_STORE_REGISTRY_BASE` respectively to be set in the daemon's
environment — deploying with the scheme but no matching env var fails (see
errors below).

Fetched (non-directory) content is hashed with SHA-256 and written into a
durable, managed cache directory — `packs/` next to the deployment registry
file (e.g. `~/.greentic/operax/packs` alongside the default
`~/.greentic/operax/deployments.json`) — as `<hex-digest>.gtpack`. A repeat
fetch of identical bytes reuses the existing cached file instead of writing
again. The deployment record persists both the resolved local cache path
(`gtpack_path`, actually loaded) and the original `reference` string
(`source_ref`, kept as provenance only). On daemon restart, a deployment
reloads directly from its persisted `gtpack_path` — the cached file is reused
and `reference` is not re-fetched.

### Upgrade by reference

`PUT /v1/operax/deployments/{id}` accepts the same optional `gtpack_path` /
`reference` pair as deploy, with the same mutual-optionality contract: at
least one must be set, and if both are set `reference` takes precedence. The
same schemes (`oci://`, `http(s)://`, `repo://`, `store://`, `file://`, or a
bare local path) and the same env-var bases
(`GREENTIC_REPO_REGISTRY_BASE`, `GREENTIC_STORE_REGISTRY_BASE`) apply.

```json
{
  "reference": "oci://ghcr.io/acme/handoff@sha256:def456..."
}
```

When upgraded by reference, the new active version's `source_ref` records
the reference string as provenance — the same way `deploy` does.

#### Deploy / upgrade errors (pack source)

| Status | Code | When |
| --- | --- | --- |
| `400` | `OPERAX_BAD_REQUEST` | Deploy/upgrade request sets neither `gtpack_path` nor `reference`. |
| `502` | `OPERAX_PACK_FETCH_FAILED` | Resolving/fetching `reference` failed: registry unreachable, a malformed or unpullable OCI/HTTP reference, no layer matching a known pack media type in the OCI manifest, a local file that doesn't exist, or a `repo://`/`store://` reference whose env base isn't set. |

#### Boundaries

- No catalog listing/discovery endpoint: deploy-by-reference only fetches a
  reference you already know; there is no route to browse or search what a
  registry has published.
- `store://` resolves against `GREENTIC_STORE_REGISTRY_BASE`, but the store
  side has no operala runtime packs published to it yet (an upstream publish
  pipeline is still needed). `oci://` and `http(s)://` work against arbitrary
  registries/URLs today and are the practical way to exercise this end to
  end in the meantime.
- The managed pack cache has no eviction or garbage collection yet — it
  grows unboundedly as new references are fetched. Planned follow-up, not
  implemented here.

### Static vs. discover mode

A deployment resolves its SoRX endpoint one of two ways, per deployment:

- **Static** (`sorx_url` set, no `sor`): every run talks to that fixed URL.
  This is the slice-1 behavior and is unchanged.
- **Discover** (`sor` set): every `run` request resolves the endpoint fresh
  from the SoRX presence directory, keyed by `(tenant, sor)`, picking the
  freshest reachable announcement. Discover mode is evaluated per run, not
  cached at deploy time, so a newly-announced SoRX instance is picked up
  without redeploying. If `sorx_url` is also supplied alongside `sor`, it is
  ignored for `run` — `sor` takes priority. If the deployment also sets
  `environment`, resolution is further scoped: only a presence announcement
  whose own `environment` matches is eligible, so a `prod`-scoped deployment
  will never resolve a `staging` instance sharing the same `(tenant, sor)`
  even if it is the freshest reachable one. A deployment with no
  `environment` set (the default) matches any announced environment,
  unchanged from before.

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

Resolution filters by `(tenant, sor)` plus the deployment's `environment` (see
above) and trusts the producer's `reachable` flag as announced.
Presence health-probing (verifying reachability instead of trusting the
announcement) remains a planned follow-up, not implemented here.

### Business-event routing

When the daemon is built with the `events` feature **and** started with
`OPERAX_EVENTS_NATS_URL` set, `operax serve` also runs an in-process business-event
router: it subscribes to `greentic.events.>` (all tenants, not subject-scoped) and,
for each decoded event, routes it to **every** `Ready` deployment whose pack declares a
matching `consumes` subscription for the event's tenant and whose `environment` is
either unset (wildcard) or equal to the event's own environment, running each one for
real (`dry_run=false`).

```bash
cargo build -p greentic-operax --features events

OPERAX_EVENTS_NATS_URL=nats://localhost:4222 \
  greentic-operax serve --bind 127.0.0.1:8099
```

A deployment's pack declares what it subscribes to via `consumes` entries in its
`operala.yaml`, each with a `capability` cap-URI in the form
`cap://greentic/events/<domain>/[vN/]<name>` (the version segment is optional). The
router matches an incoming event's `topic` against each eligible deployment's declared
subscriptions (tenant- and environment-scoped as above); a match triggers a run of that
deployment with the event payload as input.

Without `OPERAX_EVENTS_NATS_URL` set, or when the daemon was built without `events`,
event routing is inert: the daemon logs that it's disabled and starts normally
otherwise.

A run triggered by the router is stamped `caller_role: "business-event"` — the same
value the separate `operax events subscribe` CLI command stamps on its own runs —
distinguishing it from a manually-triggered `POST .../run`, which defaults to
`caller_role: "service"`. When an event matches multiple deployments, the router runs
them concurrently rather than one after another, and each is logged (`ok`/`failed`)
independently as it completes.

### NATS reconnect

The daemon's NATS subscribers — the presence subscriber backing discover mode, the
business-event router above, and the separate `operax events subscribe` CLI command —
no longer exit when the underlying NATS connection drops or its subscription stream
ends. Each reconnects automatically with capped exponential backoff, starting at 1s and
doubling up to a 30s cap, retrying forever rather than requiring an operator restart.
