use operax_core::{
    FlowDefinition, HandoffSorx, OperaHandoffMetadata, OperationalPack, OperaxError, Result,
    SchemaRegistry, SorxBinding, looks_secret_like, sha256_digest,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub fn load_operational_pack(path: impl AsRef<Path>) -> Result<OperationalPack> {
    let path = path.as_ref();
    if path.is_dir() {
        load_handoff_dir(path)
    } else if path.extension().and_then(|ext| ext.to_str()) == Some("gtpack") {
        load_pilot_gtpack(path)
    } else {
        Err(OperaxError::new(
            "unsupported_artifact",
            format!("expected handoff directory or .gtpack: {}", path.display()),
        ))
    }
}

fn load_handoff_dir(path: &Path) -> Result<OperationalPack> {
    let metadata_path = path.join("operala.yaml");
    let handoff_path = path.join("operala-handoff.json");
    require_file(&metadata_path, "missing_operala_yaml")?;
    require_file(&handoff_path, "missing_operala_handoff")?;

    let metadata_bytes = fs::read(&metadata_path)?;
    let handoff_bytes = fs::read(&handoff_path)?;
    let metadata = parse_metadata(&metadata_bytes)?;
    validate_metadata(&metadata)?;

    let handoff_json: Value = serde_json::from_slice(&handoff_bytes)?;
    if looks_secret_like(&handoff_json) {
        return Err(OperaxError::new(
            "secret_like_value",
            "handoff metadata contains secret-like values or concrete customer SORX URLs",
        ));
    }

    let flows = collect_files(path.join("flows"), "flow.yaml")?
        .into_iter()
        .map(|path| FlowDefinition {
            name: path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("flow")
                .to_string(),
            path: path.to_string_lossy().into_owned(),
        })
        .collect::<Vec<_>>();
    if flows.is_empty() {
        return Err(OperaxError::new(
            "missing_flows",
            "handoff has no flow files",
        ));
    }

    let schema_files = collect_files(path.join("schemas"), "json")?;
    let schema_names = schema_files
        .iter()
        .map(|path| file_name(path))
        .collect::<Vec<_>>();
    if schema_names.is_empty() {
        return Err(OperaxError::new(
            "missing_input_schemas",
            "handoff has no JSON schemas",
        ));
    }
    let schema_documents = load_schema_documents(schema_files)?;

    let binding = binding_from_metadata(&metadata)?;
    Ok(OperationalPack {
        metadata,
        handoff: handoff_json,
        flows,
        schemas: SchemaRegistry {
            names: schema_names,
        },
        schema_documents,
        sorx_binding: binding,
        pack_digest: sha256_digest(&handoff_bytes),
        sorla_source_digest: sorla_digest_from_metadata(path, &metadata_bytes)?,
        handoff_digest: sha256_digest(&handoff_bytes),
    })
}

fn load_pilot_gtpack(path: &Path) -> Result<OperationalPack> {
    let bytes = fs::read(path)?;
    let pack_load =
        greentic_pack::open_pack(path, greentic_pack::SigningPolicy::DevOk).map_err(|err| {
            OperaxError::new(
                "invalid_gtpack",
                format!(
                    "greentic-pack-lib failed to open pilot .gtpack: {}",
                    err.message
                ),
            )
        })?;

    let metadata_bytes = pack_load
        .files
        .get("assets/operala/operala.yaml")
        .or_else(|| pack_load.files.get("operala.yaml"))
        .cloned();
    let handoff_bytes = pack_load
        .files
        .get("assets/operala/operala-handoff.json")
        .or_else(|| pack_load.files.get("operala-handoff.json"))
        .cloned();
    let flows = pack_load
        .files
        .keys()
        .filter(|name| name.starts_with("assets/operala/flows/") && name.ends_with(".flow.yaml"))
        .map(|name| FlowDefinition {
            name: Path::new(name)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("flow")
                .to_string(),
            path: name.clone(),
        })
        .collect::<Vec<_>>();
    let schema_entries = pack_load
        .files
        .iter()
        .filter(|(name, _)| name.starts_with("assets/operala/schemas/") && name.ends_with(".json"))
        .map(|(name, bytes)| (name.clone(), bytes.clone()))
        .collect::<Vec<_>>();
    let schema_names = schema_entries
        .iter()
        .map(|(name, _)| file_name(Path::new(name)))
        .collect::<Vec<_>>();

    let metadata_bytes = metadata_bytes.ok_or_else(|| {
        OperaxError::new(
            "missing_operala_yaml",
            "pilot .gtpack is missing assets/operala/operala.yaml",
        )
    })?;
    let handoff_bytes = handoff_bytes.ok_or_else(|| {
        OperaxError::new(
            "missing_operala_handoff",
            "pilot .gtpack is missing assets/operala/operala-handoff.json",
        )
    })?;
    let metadata = parse_metadata(&metadata_bytes)?;
    validate_metadata(&metadata)?;
    let handoff_json: Value = serde_json::from_slice(&handoff_bytes)?;
    if looks_secret_like(&handoff_json) {
        return Err(OperaxError::new(
            "secret_like_value",
            "pilot .gtpack contains secret-like values or concrete customer SORX URLs",
        ));
    }
    if flows.is_empty() {
        return Err(OperaxError::new(
            "missing_flows",
            "pilot .gtpack has no flows",
        ));
    }
    if schema_names.is_empty() {
        return Err(OperaxError::new(
            "missing_input_schemas",
            "pilot .gtpack has no JSON schemas",
        ));
    }

    Ok(OperationalPack {
        sorx_binding: binding_from_metadata(&metadata)?,
        sorla_source_digest: metadata
            .sorla
            .as_ref()
            .and_then(|sorla| sorla.source_digest.clone())
            .unwrap_or_else(|| sha256_digest(&metadata_bytes)),
        metadata,
        handoff: handoff_json,
        flows,
        schemas: SchemaRegistry {
            names: schema_names,
        },
        schema_documents: load_schema_documents_from_bytes(schema_entries)?,
        pack_digest: sha256_digest(&bytes),
        handoff_digest: sha256_digest(&handoff_bytes),
    })
}

fn parse_metadata(bytes: &[u8]) -> Result<OperaHandoffMetadata> {
    serde_yaml::from_slice(bytes)
        .map_err(|err| OperaxError::new("invalid_operala_yaml", err.to_string()))
}

fn validate_metadata(metadata: &OperaHandoffMetadata) -> Result<()> {
    if metadata.schema != "greentic.operala.handoff.v1" {
        return Err(OperaxError::new(
            "invalid_handoff_schema",
            format!("unsupported handoff schema: {}", metadata.schema),
        ));
    }
    if metadata.capability.trim().is_empty() {
        return Err(OperaxError::new(
            "missing_capability",
            "capability is required",
        ));
    }
    let sorx = metadata
        .sorx
        .as_ref()
        .ok_or_else(|| OperaxError::new("missing_sorx_binding", "sorx binding is required"))?;
    if sorx.transport != "http" || sorx.url != "runtime-provided" {
        return Err(OperaxError::new(
            "invalid_sorx_binding",
            "SORX binding must use transport=http and url=runtime-provided",
        ));
    }
    Ok(())
}

fn binding_from_metadata(metadata: &OperaHandoffMetadata) -> Result<SorxBinding> {
    let HandoffSorx { transport, url } = metadata
        .sorx
        .clone()
        .ok_or_else(|| OperaxError::new("missing_sorx_binding", "sorx binding is required"))?;
    Ok(SorxBinding { transport, url })
}

fn sorla_digest_from_metadata(path: &Path, metadata_bytes: &[u8]) -> Result<String> {
    let metadata = parse_metadata(metadata_bytes)?;
    Ok(metadata
        .sorla
        .and_then(|sorla| sorla.source_digest)
        .unwrap_or_else(|| sha256_digest(path.to_string_lossy().as_bytes())))
}

fn require_file(path: &Path, code: &str) -> Result<()> {
    if !path.is_file() {
        return Err(OperaxError::new(
            code,
            format!("missing {}", path.display()),
        ));
    }
    Ok(())
}

fn collect_files(dir: impl AsRef<Path>, suffix: &str) -> Result<Vec<std::path::PathBuf>> {
    let dir = dir.as_ref();
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.ends_with(suffix))
                .unwrap_or(false)
        {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string()
}

fn load_schema_documents(paths: Vec<std::path::PathBuf>) -> Result<BTreeMap<String, Value>> {
    let mut out = BTreeMap::new();
    for path in paths {
        let name = file_name(&path);
        let bytes = fs::read(&path)?;
        let value = serde_json::from_slice(&bytes)?;
        out.insert(name, value);
    }
    Ok(out)
}

fn load_schema_documents_from_bytes(
    entries: Vec<(String, Vec<u8>)>,
) -> Result<BTreeMap<String, Value>> {
    let mut out = BTreeMap::new();
    for (path, bytes) in entries {
        let value = serde_json::from_slice(&bytes)?;
        out.insert(file_name(Path::new(&path)), value);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_handoff(root: &Path) {
        fs::write(
            root.join("operala.yaml"),
            r#"
schema: greentic.operala.handoff.v1
capability: reconciliation
extension: greentic.operala.reconciliation.v1
tenant_required: true
team_optional: true
sorla:
  source_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
sorx:
  transport: http
  url: runtime-provided
"#,
        )
        .unwrap();
        fs::write(root.join("operala-handoff.json"), r#"{"ok":true}"#).unwrap();
        fs::create_dir(root.join("flows")).unwrap();
        fs::write(
            root.join("flows/ingest-transaction.flow.yaml"),
            "name: ingest",
        )
        .unwrap();
        fs::create_dir(root.join("schemas")).unwrap();
        fs::write(root.join("schemas/bank-transaction.schema.json"), "{}").unwrap();
    }

    #[test]
    fn loads_handoff_directory() {
        let temp = tempfile::tempdir().unwrap();
        write_handoff(temp.path());
        let pack = load_operational_pack(temp.path()).unwrap();
        assert_eq!(pack.metadata.capability, "reconciliation");
        assert_eq!(pack.flows.len(), 1);
        assert_eq!(pack.schemas.names.len(), 1);
        assert!(
            pack.schema_documents
                .contains_key("bank-transaction.schema.json")
        );
    }

    #[test]
    fn rejects_concrete_customer_sorx_url_in_handoff() {
        let temp = tempfile::tempdir().unwrap();
        write_handoff(temp.path());
        fs::write(
            temp.path().join("operala-handoff.json"),
            r#"{"url":"https://sorx.customer.example"}"#,
        )
        .unwrap();
        let error = load_operational_pack(temp.path()).unwrap_err();
        assert_eq!(error.code, "secret_like_value");
    }

    #[test]
    fn rejects_invalid_archive_paths() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        {
            let file = temp.reopen().unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file("../bad", opts).unwrap();
            zip.write_all(b"bad").unwrap();
            zip.finish().unwrap();
        }
        let path = temp.path().with_extension("gtpack");
        fs::copy(temp.path(), &path).unwrap();
        let error = load_operational_pack(&path).unwrap_err();
        assert_eq!(error.code, "invalid_gtpack");
        assert!(error.message.contains("greentic-pack-lib"));
    }
}
