# PR 15 — SORX HTTP client integration

## Goal

OperaX should use SORX HTTP as the system-of-record runtime interface. Runtime
actions must resolve to SORX runtime surfaces derived from the OperaLa handoff
metadata: locked SORX business actions when available, or generated
`/v1/agent/...` routes from SORX route metadata as a fallback. OperaX must not
invent ad-hoc record mutation endpoints.

## HTTP contract

Base URL:

```text
--sorx-url https://sorx.customer.example
```

Headers:

```text
X-Greentic-Tenant-Id: demo-tenant
X-Greentic-Caller-Id: operax
X-Greentic-Caller-Role: service
X-Greentic-Team: property-ops
X-Greentic-Pack: tenancy-rent-reconciliation
X-Greentic-Request-Id: <uuid>
Idempotency-Key: <stable-action-key>
Authorization: Bearer <token>
```

Discovery endpoints:

```http
GET  /healthz
GET  /readyz
GET  /v1/sorx/routes
GET  /v1/sorx/business-actions
```

Runtime action endpoints:

```http
POST /v1/sorx/business-actions/{id}/versions/{version}/dry-run
POST /v1/sorx/business-actions/{id}/versions/{version}/invoke
<method> <generated /v1/agent/... route from /v1/sorx/routes>
```

When SORX shared-secret auth is enabled, OperaX sends either
`Authorization: Bearer <secret>` or `X-Greentic-Sorx-Secret: <secret>`. Outside
local SORX mode, `X-Greentic-Tenant-Id` and `X-Greentic-Caller-Id` are required.

## Client trait

```rust
#[async_trait]
pub trait SorxClient {
    async fn health(&self) -> Result<SorxHealth>;
    async fn routes(&self) -> Result<Vec<SorxRoute>>;
    async fn business_actions(&self) -> Result<Vec<SorxBusinessAction>>;
    async fn dry_run_business_action(&self, action: BusinessActionCall) -> Result<serde_json::Value>;
    async fn invoke_business_action(&self, action: BusinessActionCall) -> Result<serde_json::Value>;
    async fn invoke_generated_route(&self, route: GeneratedRouteCall) -> Result<serde_json::Value>;
}
```

## Pilot auth

Support:

```text
--sorx-token-env SORX_TOKEN
```

Default token env var:

```text
SORX_TOKEN
```

## Acceptance criteria

- OperaX can call mocked SORX HTTP endpoints.
- Tenant/team headers are always included.
- Caller headers are included outside local SORX mode.
- Business action calls include URL id/version plus request contract hash.
- Dry-run uses SORX business-action dry-run when available and avoids generated
  mutating route calls.
- Mutating calls use stable idempotency keys.
- HTTP errors are converted to structured operational errors.
