# PR 14 — OperaX gtpack loader and validation

## Goal

OperaX can load and validate OperaLa operational handoff metadata, including
the optional pilot `.gtpack` compatibility artifact generated for local runs.
All `.gtpack` interactions must go through `greentic-pack-lib`.

## Validate

- For pilot `.gtpack` input, `greentic-pack-lib` can open and validate the
  archive and `manifest.cbor` exists.
- For handoff directory input, `operala-handoff.json` and `operala.yaml` exist.
- For pilot `.gtpack` input, `assets/operala/operala.yaml` or equivalent
  embedded handoff metadata exists.
- handoff schema is `greentic.operala.handoff.v1`.
- capability is known by metadata.
- ingress flows exist.
- input schemas exist.
- SORX HTTP binding template exists and uses `url: runtime-provided`.
- referenced components exist.
- no plaintext secrets, provider credentials, or concrete customer SORX URLs are
  embedded in generated artifacts.
- pilot `.gtpack` archive entries are relative and reject absolute paths,
  backslashes, and `..` traversal components through `greentic-pack-lib`.
- lock/digest metadata is verified when present.

## API

```rust
pub struct OperationalPack {
    pub metadata: OperaHandoffMetadata,
    pub flows: Vec<FlowDefinition>,
    pub schemas: SchemaRegistry,
    pub sorx_binding: SorxBinding,
    pub sorla_source_digest: String,
    pub handoff_digest: String,
}
```

## Acceptance criteria

- Invalid pack gives actionable error.
- Loader supports local file path.
- Loader records pack digest for audit.
- Loader records OperaLa handoff digest and SoRLa source digest for audit.
- Invalid archive paths, digest drift, or secret-like embedded values fail
  before any runtime execution.
- Loader does not implement custom `.gtpack` ZIP parsing.
