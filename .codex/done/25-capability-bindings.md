# PR: Add capability-oriented bindings on top of the existing SORX client

## Summary

Extend OPERAX so handoff metadata can declare required business-function capabilities and consumed business events, while preserving the current runtime shape:

- `operax-sorx-http` already exposes a synchronous `SorxClient` trait.
- `HttpSorxClient` is already the compatibility transport for SORX HTTP.
- `operax-runtime::apply_action` already routes `ProposedAction` through `SorxClient` instead of open-coding HTTP endpoints.
- The CLI still requires a runtime SORX URL (`--sorx-url` / `SORX_URL`) because capability resolution is not available yet.

This PR should not replace the existing client with an unrelated async/`anyhow` abstraction. It should add a capability-oriented layer that can be backed by the current `SorxClient` first, and later by direct operator/SORX invocation.

## Motivation

OPERAX workflows should be portable across local/demo/prod and should not encode generated HTTP route details as the long-term binding model. Today, HTTP remains the only concrete transport, but the code already has a useful boundary in `SorxClient`. The next step is to make capability requirements explicit in pack metadata and translate those requirements into the existing action invocation model.

## Current codebase facts

- Handoff metadata is parsed into `OperaHandoffMetadata` in `operax-core`.
- `operax-pack-loader` validates `schema == "greentic.operala.handoff.v1"`.
- `operax-pack-loader` currently requires:

```yaml
sorx:
  transport: http
  url: runtime-provided
```

- `OperationalPack` carries `metadata`, arbitrary handoff JSON, flow/schema info, and `sorx_binding`.
- `ProposedAction` already carries:
  - `SorxTarget::BusinessAction`
  - `SorxTarget::GeneratedRoute`
  - `ActionOperation`
  - `idempotency_key`
- `OperaxContext` already carries tenant, optional team, locale, caller id, caller role, request id, and pack digest.
- There is no `greentic-config`, `greentic-secrets`, `anyhow`, `async-trait`, or async runtime dependency in this repo today.

## Proposed additions

### Capability requirement declarations

Add optional fields to `OperaHandoffMetadata` and keep them backward-compatible with the existing handoff schema:

```yaml
requires:
  - id: operax.needs.reconciliation.record-rent-payment
    capability: cap://greentic/sorx/tenancy/v1/functions/record-rent-payment
    metadata:
      kind: business_function
      action_id: record_rent_payment
      version: "0.1.0"
```

The first implementation should map business-function declarations to existing `SorxTarget::BusinessAction` calls. It should not require removing `sorx: runtime-provided` yet.

### Event subscription declarations

Add parsing and validation for event subscriptions, but do not assume there is an operator delivery path in this repo yet:

```yaml
consumes:
  - id: operax.subscribes.work-order-assigned
    capability: cap://greentic/events/boiler-maintenance/v1/work-order-assigned
    mode: shared
    metadata:
      kind: business_event_subscription
      handler:
        component_ref: operax:maintenance-workflow:v1
        operation: on_work_order_assigned
      filter:
        payload.priority: urgent
```

For this PR, event subscriptions can be metadata-only and discoverable from loaded packs. Actual event delivery can follow once an operator event transport exists.

### Capability client layer

Do not add this as an async `anyhow::Result` trait unless the workspace first adopts those dependencies. Prefer the current repo style:

```rust
pub trait CapabilityClient {
    fn invoke(
        &self,
        capability: &str,
        operation: &str,
        input: serde_json::Value,
        ctx: &operax_core::OperaxContext,
        idempotency_key: Option<&str>,
    ) -> operax_core::Result<serde_json::Value>;
}
```

Initial implementation:

- `SorxCapabilityClient<C: SorxClient>` or an adapter method that translates capability calls into existing `BusinessActionCall` / `GeneratedRouteCall`.
- `HttpSorxClient` remains the concrete HTTP compatibility client through the existing `SorxClient` trait.

Future implementation:

- A direct operator capability client once that transport exists.

### Actor context

Use the existing `OperaxContext` first. It already maps into SORX headers through `sorx_headers`.

Potential additive fields should be introduced deliberately:

- actor type, if it differs from today’s `caller_role`
- correlation id, if distinct from `request_id`
- causation id

Do not duplicate the existing `idempotency_key`; it is already part of `ProposedAction` and is forwarded by `HttpSorxClient`.

## Config and secrets

Do not assume `greentic-config` or `greentic-secrets` are already dependencies. If this PR introduces them, add them intentionally at the workspace level with tests. Otherwise, keep non-secret runtime configuration in the current CLI/runtime inputs and keep secret material behind the existing `SORX_TOKEN` environment-variable lookup.

OPERAX should continue rejecting concrete customer SORX URLs in handoff artifacts. Runtime URLs are still allowed through CLI/test configuration until capability resolution exists.

## Migration plan

1. Add typed `requires` and `consumes` metadata to `operax-core`.
2. Update `operax-pack-loader` validation and tests so existing handoffs still load unchanged.
3. Add capability-to-SORX translation backed by the existing `SorxClient`.
4. Move one existing call path to the capability adapter without removing `SorxClient` or `HttpSorxClient`.
5. Keep generated-route fallback working for actions that do not have a declared business capability yet.
6. Expose consumed event metadata for operator discovery, without implementing event delivery in this PR.

## Acceptance criteria

- Existing `examples/tenancy/handoff` still loads and runs.
- OPERAX can parse optional `requires` declarations from `operala.yaml`.
- OPERAX can parse optional `consumes` declarations from `operala.yaml`.
- Existing HTTP integration still works through `HttpSorxClient`.
- At least one action path invokes through a capability adapter backed by `SorxClient`.
- Generated-route fallback still works for reconciliation-case creation.
- Event subscription metadata is discoverable from `OperationalPack`.

## Test plan

- Unit tests for `requires` / `consumes` parsing in `operax-core` or `operax-pack-loader`.
- Regression test that the current tenancy handoff without `requires` / `consumes` remains valid.
- Unit test for capability-to-`BusinessActionCall` translation.
- Regression test for `HttpSorxClient` compatibility behavior through the existing mock client.
- Metadata discovery test for event subscriptions without requiring live event delivery.
