//! Disk persistence for the deployment registry (single JSON file), mirroring
//! greentic-sorx's `LocalDeploymentRegistryStore`.

use crate::deployment::DeploymentRegistry;
use operax_core::{OperaxError, Result};
use std::path::{Path, PathBuf};

pub struct OperaxDeploymentStore {
    path: PathBuf,
}

impl OperaxDeploymentStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load the registry; an absent file yields an empty registry.
    pub fn load(&self) -> Result<DeploymentRegistry> {
        match std::fs::read(&self.path) {
            // `?` converts serde_json::Error via operax_core's From impl.
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(DeploymentRegistry::default())
            }
            Err(err) => Err(OperaxError::new(
                "registry_read_failed",
                format!(
                    "reading deployment registry at {}: {err}",
                    self.path.display()
                ),
            )),
        }
    }

    /// Persist the registry via write-tmp-then-rename for atomicity.
    pub fn save(&self, registry: &DeploymentRegistry) -> Result<()> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| {
                OperaxError::new(
                    "registry_dir_failed",
                    format!("creating registry dir {}: {e}", parent.display()),
                )
            })?;
        }
        // `?` converts serde_json::Error via operax_core's From impl.
        let bytes = serde_json::to_vec_pretty(registry)?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes).map_err(|e| {
            OperaxError::new(
                "registry_write_failed",
                format!("writing {}: {e}", tmp.display()),
            )
        })?;
        std::fs::rename(&tmp, &self.path).map_err(|e| {
            OperaxError::new(
                "registry_rename_failed",
                format!("renaming into {}: {e}", self.path.display()),
            )
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deployment::{DeploymentRecord, DeploymentRegistry, DeploymentVersion};
    use std::path::PathBuf;

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "operax-store-test-{}-{name}.json",
            std::process::id()
        ));
        p
    }

    #[test]
    fn load_missing_returns_empty() {
        let store = OperaxDeploymentStore::new(tmp_path("missing"));
        let reg = store.load().expect("load");
        assert!(reg.deployments.is_empty());
    }

    #[test]
    fn save_then_load_roundtrip() {
        let path = tmp_path("roundtrip");
        let _ = std::fs::remove_file(&path);
        let store = OperaxDeploymentStore::new(path.clone());
        let reg = DeploymentRegistry {
            deployments: vec![DeploymentRecord {
                id: "d1".into(),
                tenant: "t".into(),
                team: None,
                locale: None,
                sorx_url: Some("http://x".into()),
                sor: None,
                active: DeploymentVersion {
                    version: 2,
                    gtpack_path: PathBuf::from("/tmp/p.gtpack"),
                    pack_digest: "sha256:z".into(),
                    deployed_at_unix: 42,
                },
                history: vec![],
            }],
        };
        store.save(&reg).expect("save");
        let back = store.load().expect("load");
        assert_eq!(back.deployments.len(), 1);
        assert_eq!(back.deployments[0].active.version, 2);
        let _ = std::fs::remove_file(&path);
    }
}
