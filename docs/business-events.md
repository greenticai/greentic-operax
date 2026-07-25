# Business Events: OperaX Subscriber

OperaX can subscribe to business events published by SoRX (SoRLa runtime) over NATS and automatically run your deployed pack when matching events arrive. This guide explains how to declare subscriptions, run the subscriber, and understand the delivery model.

## Declaring Event Subscriptions

In your bundle's `operala.yaml`, list the events your pack consumes:

```yaml
consumes:
  - id: "entity.created"
    capability: "cap://greentic/events/mypack/Entity.created"
  - id: "account.updated"
    capability: "cap://greentic/events/mypack/v1/Account.updated"
  - id: "custom.command"
    capability: "cap://greentic/events/mypack/custom.command"
```

Each subscription has:
- **`id`**: a friendly identifier for your pack's logic (e.g., a step name that handles this event).
- **`capability`**: the event type URI, following the format `cap://greentic/events/<pack>/<event>` with an optional `v1` version segment placed *before* the event name (`cap://greentic/events/<pack>/v1/<event>`; currently unused but reserved).

The `<event>` segment can represent either:
- **Entity lifecycle events**: `<Entity>.<operation>` (e.g., `Entity.created`, `Account.updated`).
- **Command events**: `<domain>.<command>` (e.g., `custom.command`).

Capabilities are parsed and validated when your pack is loaded; the OperaX subscriber will only deliver events whose topics match your declared subscriptions.

## Running the Subscriber

The subscriber is a feature-gated subcommand available when OperaX is built with `--features events`:

```bash
operax events subscribe \
  --artifact <path-to-pack.gtpack> \
  --tenant <tenant-id> \
  --sorx-url <sorx-http-base-url> \
  --nats-url <nats-server-url>
```

### Arguments

- **`--artifact`** (required): path to the deployed `.gtpack` file.
- **`--tenant`** (required): the tenant ID for which to subscribe. The subscriber will only process events tagged with this tenant.
- **`--sorx-url`** (required): the HTTP base URL of the SoRX instance (e.g., `http://localhost:8080`).
- **`--nats-url`** (optional): the NATS server URL. There is no default — either `--nats-url` or the `OPERAX_EVENTS_NATS_URL` environment variable must be set; if neither is provided, the command fails with `missing_nats_url` (exit code 2).

### Environment Variables

- **`OPERAX_EVENTS_NATS_URL`**: alternative to `--nats-url`; the subscriber will use this if the flag is not provided.
- **`SORX_TOKEN`**: credentials for SoRX HTTP requests (the subscriber reads this by default; override with `--sorx-token-env <VARNAME>` to use a different variable).

### Example

```bash
export SORX_TOKEN="your-sorx-token"
export OPERAX_EVENTS_NATS_URL="nats://events.example.com:4222"

operax events subscribe \
  --artifact /path/to/my-pack.gtpack \
  --tenant acme-corp \
  --sorx-url https://sorx.example.com
```

The subscriber runs until interrupted (Ctrl+C).

### Feature Gate

The `events` subcommand is only available when OperaX is compiled with the `events` feature:

```bash
cargo build --features events
```

If you attempt to run `operax events` on a default build (without `--features events`), the CLI will print an error message directing you to rebuild with the feature flag.

## Delivery Model

The subscriber implements a **wildcard subscribe + filter-on-receipt** pattern:

1. **Subscription**: The subscriber opens a single wildcard subscription to `greentic.events.<tenant>.>` on the NATS server, receiving all events for the tenant.

2. **Deserialization**: For each message received, the payload is deserialized as a `greentic_types::EventEnvelope` (JSON). Malformed payloads are logged and skipped without crashing the loop.

3. **Matching**: The envelope's topic is compared against each of your pack's declared `consumes` subscriptions. The subscriber normalizes both the declared capability and the envelope's topic (alphanumeric and underscore/hyphen preserved; other characters replaced with hyphens) and checks for a match.

4. **Dispatch**: For each matching subscription, the subscriber calls `run_artifact_with_client` on a blocking thread (so it never runs on the async NATS reactor). The envelope's `payload` field is passed as the `RunRequest.input`, and the caller role is tagged as `"business-event"`. Results are logged.

### Guarantees

- **At-most-once delivery**: messages can be lost if the subscriber crashes, but no duplicate runs occur (matching core NATS semantics used by SoRX).
- **No persistence**: there is no message replay or durability. Messages published while the subscriber is offline are not delivered later.
- **Blocking execution**: each pack run is synchronous (on a blocking thread), so slow-running packs will temporarily block new event processing.

## Known Limitations and Drift

### Topic vs. Domain Validation

The canonical `greentic_types::validate_business_event` function expects event envelopes to satisfy `envelope.topic == domain`, where `domain` is derived from the capability URI.

However, SoRX publishes hierarchical topics (e.g., `sorla.mypack.Entity.created`, `sorla.mypack.custom.command`). The OperaX subscriber deliberately matches against SoRX's **actual hierarchical topic** rather than the stricter canonical rule, ensuring compatibility with the SoRX event format.

**Reconciling** `validate_business_event` with SoRX's topic scheme is a planned enhancement in the `greentic-types` crate; it is not in scope for this release.

## Troubleshooting

### No events are being delivered

1. Verify the subscriber is running and connected to NATS:
   ```bash
   operax events subscribe --artifact ... --tenant ...
   ```
   Look for a `subscribed to <subject>` log message (e.g., `subscribed to greentic.events.acme-corp.>`).

2. Confirm your pack's `operala.yaml` includes the correct `consumes` entries and that the capabilities match the events SoRX is publishing.

3. Check that the tenant ID in your subscription command matches the tenant ID in the event envelope.

### Malformed event errors

Events that fail to deserialize as `EventEnvelope` (corrupt JSON or missing fields) are logged but do not stop the subscriber. Check your logs and ensure SoRX is publishing well-formed events.

### Subscriber is built-in or unavailable

Rebuild OperaX with `--features events` if you see a "feature not enabled" message.

## Further Reading

- [SoRX business events publishing](../superpowers/specs/2026-07-25-operax-business-event-subscriber-design.md) — design spec (SoRX side).
- OperaX CLI reference: `operax events subscribe --help`.
