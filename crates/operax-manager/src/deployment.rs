//! Multi-deployment registry for the `operax serve` daemon.
//!
//! One `DeploymentSlot` per operala deployment wraps an `Arc<ManagerRuntime>`
//! (the active pack version) plus persisted metadata. `run`/`test`/`events`/
//! `presence` are unaffected.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The whole persisted registry file.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeploymentRegistry {
    pub deployments: Vec<DeploymentRecord>,
}

/// Persisted per-deployment metadata (the runtime is rebuilt from `active.gtpack_path`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentRecord {
    pub id: String,
    pub tenant: String,
    pub team: Option<String>,
    pub locale: Option<String>,
    pub sorx_url: String,
    pub active: DeploymentVersion,
    /// Previous versions, newest-first, capped (see `HISTORY_CAP`).
    #[serde(default)]
    pub history: Vec<DeploymentVersion>,
}

/// One deployed pack version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentVersion {
    pub version: u64,
    pub gtpack_path: PathBuf,
    pub pack_digest: String,
    pub deployed_at_unix: u64,
}

/// In-memory readiness of a slot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DeploymentStatus {
    Ready,
    Failed { error: String },
}

/// Max previous versions retained for a future rollback slice.
pub const HISTORY_CAP: usize = 5;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample_version(v: u64) -> DeploymentVersion {
        DeploymentVersion {
            version: v,
            gtpack_path: PathBuf::from("/tmp/x.gtpack"),
            pack_digest: "sha256:abc".to_string(),
            deployed_at_unix: 1_700_000_000,
        }
    }

    #[test]
    fn registry_serde_roundtrip() {
        let reg = DeploymentRegistry {
            deployments: vec![DeploymentRecord {
                id: "rent-recon".to_string(),
                tenant: "demo".to_string(),
                team: Some("property-ops".to_string()),
                locale: None,
                sorx_url: "http://localhost:8088".to_string(),
                active: sample_version(1),
                history: vec![],
            }],
        };
        let json = serde_json::to_string(&reg).expect("serialize");
        let back: DeploymentRegistry = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.deployments.len(), 1);
        assert_eq!(back.deployments[0].id, "rent-recon");
        assert_eq!(back.deployments[0].active.version, 1);
    }
}
