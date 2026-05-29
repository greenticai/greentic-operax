# Mock SORX

The customer pilot tests use `operax-sorx-http::MockSorxClient` for unit-level
verification and dry-run CLI commands for fixture coverage.

A fuller mock HTTP server should expose:

- `GET /healthz`
- `GET /readyz`
- `GET /v1/sorx/routes`
- `GET /v1/sorx/business-actions`
- `POST /v1/sorx/business-actions/{id}/versions/{version}/dry-run`
- `POST /v1/sorx/business-actions/{id}/versions/{version}/invoke`
- generated `/v1/agent/...` routes declared by `/v1/sorx/routes`
