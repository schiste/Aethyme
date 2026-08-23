//! Typed contract for Aethyme release manifests.
//!
//! Release tooling and update consumers share this module so a syntactically
//! valid manifest cannot weaken the paired-binary or compatibility contract.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    BROKER_STORAGE_CURRENT_SCHEMA, BROKER_STORAGE_MINIMUM_SCHEMA, ENGINE_PROTOCOL_VERSION,
    MINIMUM_GIT_VERSION,
};

pub const RELEASE_MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const RELEASE_TARGETS: &[&str] = &[
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
];
pub const REQUIRED_RELEASE_BINARIES: &[&str] = &["aethyme", "aethyme-engine-cli"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseArtifact {
    pub archive: String,
    pub binaries: Vec<String>,
    pub sha256: String,
    pub size_bytes: u64,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseBrokerStorageCompatibility {
    pub current_schema: i64,
    pub minimum_readable_schema: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseCompatibility {
    pub broker_storage: ReleaseBrokerStorageCompatibility,
    pub engine_protocol: u32,
    pub minimum_git_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseInstaller {
    pub filename: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseManifest {
    pub artifacts: Vec<ReleaseArtifact>,
    pub compatibility: ReleaseCompatibility,
    pub installer: ReleaseInstaller,
    pub release_channel: String,
    pub required_binaries: Vec<String>,
    pub schema_version: u32,
    pub source_sha: String,
    pub supported_platforms: Vec<String>,
    pub version: String,
}

impl ReleaseManifest {
    pub fn new(
        version: impl Into<String>,
        source_sha: impl Into<String>,
        release_channel: impl Into<String>,
        artifacts: Vec<ReleaseArtifact>,
        installer: ReleaseInstaller,
    ) -> Self {
        Self {
            artifacts,
            compatibility: ReleaseCompatibility {
                broker_storage: ReleaseBrokerStorageCompatibility {
                    current_schema: BROKER_STORAGE_CURRENT_SCHEMA,
                    minimum_readable_schema: BROKER_STORAGE_MINIMUM_SCHEMA,
                },
                engine_protocol: ENGINE_PROTOCOL_VERSION,
                minimum_git_version: MINIMUM_GIT_VERSION.to_string(),
            },
            installer,
            release_channel: release_channel.into(),
            required_binaries: required_binaries(),
            schema_version: RELEASE_MANIFEST_SCHEMA_VERSION,
            source_sha: source_sha.into(),
            supported_platforms: RELEASE_TARGETS
                .iter()
                .map(|target| (*target).to_string())
                .collect(),
            version: version.into(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != RELEASE_MANIFEST_SCHEMA_VERSION {
            return Err(format!(
                "unsupported release manifest schema {}",
                self.schema_version
            ));
        }
        if !valid_version(&self.version) {
            return Err("release manifest version is invalid".into());
        }
        if !matches!(self.source_sha.len(), 40 | 64) || !is_lower_hex(&self.source_sha) {
            return Err("release manifest source SHA must be a full lowercase object id".into());
        }
        if !matches!(self.release_channel.as_str(), "stable" | "beta" | "nightly") {
            return Err("release manifest channel must be stable, beta, or nightly".into());
        }
        if self.required_binaries != required_binaries() {
            return Err("release manifest must require the complete Aethyme binary pair".into());
        }
        if self.compatibility.engine_protocol != ENGINE_PROTOCOL_VERSION {
            return Err(format!(
                "release engine protocol {} is incompatible with updater protocol {}",
                self.compatibility.engine_protocol, ENGINE_PROTOCOL_VERSION
            ));
        }
        if self.compatibility.broker_storage.minimum_readable_schema
            > self.compatibility.broker_storage.current_schema
        {
            return Err("release broker storage compatibility range is inverted".into());
        }
        validate_digest("installer", &self.installer.sha256)?;

        let supported = self
            .supported_platforms
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        if supported.len() != self.supported_platforms.len() {
            return Err("release manifest contains duplicate supported platforms".into());
        }
        if self.artifacts.len() != supported.len() {
            return Err("release manifest must contain one artifact per supported platform".into());
        }
        let mut artifact_targets = BTreeSet::new();
        for artifact in &self.artifacts {
            if !supported.contains(&artifact.target) {
                return Err(format!(
                    "release artifact target {} is not supported",
                    artifact.target
                ));
            }
            if !artifact_targets.insert(artifact.target.clone()) {
                return Err(format!(
                    "release manifest contains duplicate artifact target {}",
                    artifact.target
                ));
            }
            if artifact.binaries != required_binaries() {
                return Err(format!(
                    "release artifact {} does not contain the complete Aethyme binary pair",
                    artifact.archive
                ));
            }
            let expected_archive = format!("aethyme-v{}-{}.tar.gz", self.version, artifact.target);
            if artifact.archive != expected_archive {
                return Err(format!(
                    "release artifact {} does not match expected name {expected_archive}",
                    artifact.archive
                ));
            }
            validate_digest(&artifact.archive, &artifact.sha256)?;
        }
        if artifact_targets != supported {
            return Err("release manifest is missing a supported platform artifact".into());
        }
        Ok(())
    }

    pub fn artifact_for_target(&self, target: &str) -> Option<&ReleaseArtifact> {
        self.artifacts
            .iter()
            .find(|artifact| artifact.target == target)
    }
}

fn required_binaries() -> Vec<String> {
    REQUIRED_RELEASE_BINARIES
        .iter()
        .map(|binary| (*binary).to_string())
        .collect()
}

fn validate_digest(label: &str, digest: &str) -> Result<(), String> {
    if digest.len() != 64 || !is_lower_hex(digest) {
        return Err(format!("{label} SHA-256 is invalid"));
    }
    Ok(())
}

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_version(version: &str) -> bool {
    !version.is_empty()
        && !version.starts_with('.')
        && !version.ends_with('.')
        && version
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(target: &str) -> ReleaseArtifact {
        ReleaseArtifact {
            archive: format!("aethyme-v0.2.0-{target}.tar.gz"),
            binaries: required_binaries(),
            sha256: "a".repeat(64),
            size_bytes: 42,
            target: target.to_string(),
        }
    }

    fn manifest() -> ReleaseManifest {
        ReleaseManifest::new(
            "0.2.0",
            "b".repeat(40),
            "stable",
            RELEASE_TARGETS
                .iter()
                .map(|target| artifact(target))
                .collect(),
            ReleaseInstaller {
                filename: "install.sh".into(),
                sha256: "c".repeat(64),
                size_bytes: 7,
            },
        )
    }

    #[test]
    fn validates_the_complete_release_contract() {
        assert_eq!(manifest().validate(), Ok(()));
    }

    #[test]
    fn rejects_an_artifact_that_splits_the_binary_pair() {
        let mut manifest = manifest();
        manifest.artifacts[0].binaries.pop();

        assert!(
            manifest
                .validate()
                .unwrap_err()
                .contains("complete Aethyme binary pair")
        );
    }

    #[test]
    fn rejects_duplicate_missing_or_misnamed_targets() {
        let mut duplicate = manifest();
        duplicate.artifacts[1].target = duplicate.artifacts[0].target.clone();
        assert!(
            duplicate
                .validate()
                .unwrap_err()
                .contains("duplicate artifact target")
        );

        let mut missing = manifest();
        missing.artifacts.pop();
        assert!(
            missing
                .validate()
                .unwrap_err()
                .contains("one artifact per supported")
        );

        let mut misnamed = manifest();
        misnamed.artifacts[0].archive = "router-only.tar.gz".into();
        assert!(misnamed.validate().unwrap_err().contains("expected name"));
    }

    #[test]
    fn rejects_invalid_digests_protocols_and_storage_ranges() {
        let mut invalid = manifest();
        invalid.artifacts[0].sha256 = "NOT-A-DIGEST".into();
        assert!(invalid.validate().unwrap_err().contains("SHA-256"));

        let mut invalid = manifest();
        invalid.compatibility.engine_protocol += 1;
        assert!(invalid.validate().unwrap_err().contains("engine protocol"));

        let mut invalid = manifest();
        invalid.compatibility.broker_storage.minimum_readable_schema =
            invalid.compatibility.broker_storage.current_schema + 1;
        assert!(invalid.validate().unwrap_err().contains("inverted"));
    }
}
