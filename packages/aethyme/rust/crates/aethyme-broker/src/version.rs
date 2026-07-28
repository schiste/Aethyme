//! Local binary/source drift detection.
//!
//! The broker can run against any repository, but source-drift comparison
//! is only meaningful when the target repository is the Aethyme source
//! checkout itself. In that case, compare the running binary's build
//! commit/describe to the repository's `aethyme/integration` branch.

use std::path::Path;

use crate::git::GitRepo;
use crate::merge::PromoteConfig;

#[derive(Debug, Clone, serde::Serialize)]
pub struct BinaryBuild {
    pub version: String,
    pub describe: Option<String>,
    pub commit: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct VersionDriftReport {
    pub binary: BinaryBuild,
    pub repo_is_aethyme_source: bool,
    pub integration_branch: String,
    pub integration_head: Option<String>,
    pub integration_describe: Option<String>,
    pub release_tag: Option<String>,
    pub status: VersionDriftStatus,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionDriftStatus {
    Current,
    BehindIntegration,
    ReleaseBehindIntegration,
    AheadOfIntegration,
    NotAethymeSource,
    Unknown,
}

impl VersionDriftStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::BehindIntegration => "behind integration",
            Self::ReleaseBehindIntegration => "release behind integration",
            Self::AheadOfIntegration => "ahead of integration",
            Self::NotAethymeSource => "not aethyme source",
            Self::Unknown => "unknown",
        }
    }

    pub fn is_drift(self) -> bool {
        matches!(
            self,
            Self::BehindIntegration | Self::ReleaseBehindIntegration
        )
    }
}

pub fn current_binary_build() -> BinaryBuild {
    BinaryBuild {
        version: env!("CARGO_PKG_VERSION").to_string(),
        describe: non_empty(option_env!("AETHYME_BROKER_GIT_DESCRIBE")),
        commit: non_empty(option_env!("AETHYME_BROKER_GIT_COMMIT")),
        path: std::env::current_exe()
            .ok()
            .map(|path| path.to_string_lossy().into_owned()),
    }
}

pub fn inspect_version(main_root: &Path) -> VersionDriftReport {
    inspect_version_with_binary(main_root, current_binary_build())
}

pub(crate) fn inspect_version_with_binary(
    main_root: &Path,
    binary: BinaryBuild,
) -> VersionDriftReport {
    let integration_branch = PromoteConfig::load(main_root).branch;
    let repo_is_aethyme_source = is_aethyme_source_checkout(main_root);
    let Ok(repo) = GitRepo::discover(main_root) else {
        return VersionDriftReport {
            binary,
            repo_is_aethyme_source,
            integration_branch,
            integration_head: None,
            integration_describe: None,
            release_tag: None,
            status: VersionDriftStatus::Unknown,
            message: "not inside a git repository; cannot compare CLI build to repo state".into(),
        };
    };
    let integration_ref = format!("refs/heads/{integration_branch}");
    let integration_head = repo.resolve_ref(&integration_ref);
    let integration_describe = integration_head
        .as_deref()
        .and_then(|head| repo.describe_ref(head));
    let release_tag = integration_head
        .as_deref()
        .and_then(|head| repo.nearest_tag(head));

    if !repo_is_aethyme_source {
        return VersionDriftReport {
            binary,
            repo_is_aethyme_source,
            integration_branch,
            integration_head,
            integration_describe,
            release_tag,
            status: VersionDriftStatus::NotAethymeSource,
            message: "not an Aethyme source checkout; binary/source drift is not comparable here"
                .into(),
        };
    }

    let Some(integration) = integration_head.as_deref() else {
        return VersionDriftReport {
            binary,
            repo_is_aethyme_source,
            integration_branch,
            integration_head,
            integration_describe,
            release_tag,
            status: VersionDriftStatus::Unknown,
            message: "integration branch is missing; cannot compare CLI build to repo state".into(),
        };
    };

    let binary_commit = resolve_binary_commit(&repo, &binary);
    let Some(binary_commit) = binary_commit else {
        return VersionDriftReport {
            binary,
            repo_is_aethyme_source,
            integration_branch,
            integration_head: Some(integration.to_string()),
            integration_describe,
            release_tag,
            status: VersionDriftStatus::Unknown,
            message: "running CLI build did not expose a commit that resolves in this repo".into(),
        };
    };

    let binary_label = binary_label(&binary);
    let integration_label = integration_describe
        .as_deref()
        .unwrap_or_else(|| short_commit(integration));
    let (status, message) = if binary_commit == integration {
        (
            VersionDriftStatus::Current,
            format!("running CLI {binary_label} matches {integration_branch} {integration_label}"),
        )
    } else if repo.is_ancestor(&binary_commit, integration) {
        if release_tag
            .as_deref()
            .is_some_and(|tag| binary.describe.as_deref() == Some(tag))
        {
            (
                VersionDriftStatus::ReleaseBehindIntegration,
                format!(
                    "running CLI is on release {}; {integration_branch} is at {integration_label} with newer unreleased broker changes",
                    binary_label
                ),
            )
        } else {
            (
                VersionDriftStatus::BehindIntegration,
                format!(
                    "running CLI {binary_label} is older than {integration_branch} {integration_label}; reinstall from integration or a newer release"
                ),
            )
        }
    } else if repo.is_ancestor(integration, &binary_commit) {
        (
            VersionDriftStatus::AheadOfIntegration,
            format!(
                "running CLI {binary_label} is newer than this repo's {integration_branch} {integration_label}"
            ),
        )
    } else {
        (
            VersionDriftStatus::Unknown,
            format!(
                "running CLI {binary_label} and {integration_branch} {integration_label} are unrelated; cannot determine drift"
            ),
        )
    };

    VersionDriftReport {
        binary,
        repo_is_aethyme_source,
        integration_branch,
        integration_head: Some(integration.to_string()),
        integration_describe,
        release_tag,
        status,
        message,
    }
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn is_aethyme_source_checkout(root: &Path) -> bool {
    root.join("packages/aethyme/rust/crates/aethyme-broker/Cargo.toml")
        .is_file()
        && root
            .join("packages/aethyme/rust/crates/aethyme-engine/Cargo.toml")
            .is_file()
}

fn resolve_binary_commit(repo: &GitRepo, binary: &BinaryBuild) -> Option<String> {
    if let Some(commit) = binary
        .commit
        .as_deref()
        .and_then(|commit| repo.resolve_ref(commit))
    {
        return Some(commit);
    }
    let Some(describe) = binary.describe.as_deref() else {
        return None;
    };
    let commitish = commitish_from_describe(describe)?;
    repo.resolve_ref(&commitish)
}

fn commitish_from_describe(describe: &str) -> Option<String> {
    let clean = describe.strip_suffix("-dirty").unwrap_or(describe);
    if let Some((_, suffix)) = clean.rsplit_once("-g")
        && !suffix.is_empty()
        && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Some(suffix.to_string());
    }
    Some(clean.to_string())
}

fn binary_label(binary: &BinaryBuild) -> String {
    binary
        .describe
        .clone()
        .unwrap_or_else(|| binary.version.clone())
}

fn short_commit(commit: &str) -> &str {
    &commit[..12.min(commit.len())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn sh(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
            .status;
        assert!(status.success(), "git {args:?} failed");
    }

    fn marker_source_checkout(root: &Path) {
        let broker = root.join("packages/aethyme/rust/crates/aethyme-broker");
        let engine = root.join("packages/aethyme/rust/crates/aethyme-engine");
        std::fs::create_dir_all(&broker).unwrap();
        std::fs::create_dir_all(&engine).unwrap();
        std::fs::write(
            broker.join("Cargo.toml"),
            "[package]\nname = \"aethyme-broker\"\n",
        )
        .unwrap();
        std::fs::write(
            engine.join("Cargo.toml"),
            "[package]\nname = \"aethyme-engine\"\n",
        )
        .unwrap();
    }

    fn binary(commit: &str, describe: &str) -> BinaryBuild {
        BinaryBuild {
            version: "0.1.1".into(),
            describe: Some(describe.into()),
            commit: Some(commit.into()),
            path: Some("/tmp/aethyme".into()),
        }
    }

    #[test]
    fn release_binary_behind_integration_is_detected() {
        let tmp = tempfile::tempdir().unwrap();
        sh(tmp.path(), &["init", "-q", "-b", "main"]);
        marker_source_checkout(tmp.path());
        sh(tmp.path(), &["add", "-A"]);
        sh(tmp.path(), &["commit", "-qm", "release"]);
        sh(tmp.path(), &["tag", "v0.1.1"]);
        let release = GitRepo::discover(tmp.path())
            .unwrap()
            .head_commit()
            .unwrap();

        std::fs::write(tmp.path().join("README.md"), "new broker work\n").unwrap();
        sh(tmp.path(), &["add", "-A"]);
        sh(tmp.path(), &["commit", "-qm", "new"]);
        sh(
            tmp.path(),
            &["update-ref", "refs/heads/aethyme/integration", "HEAD"],
        );

        let report = inspect_version_with_binary(tmp.path(), binary(&release, "v0.1.1"));
        assert_eq!(report.status, VersionDriftStatus::ReleaseBehindIntegration);
        assert!(report.message.contains("newer unreleased broker changes"));
    }

    #[test]
    fn matching_binary_and_integration_is_current() {
        let tmp = tempfile::tempdir().unwrap();
        sh(tmp.path(), &["init", "-q", "-b", "main"]);
        marker_source_checkout(tmp.path());
        sh(tmp.path(), &["add", "-A"]);
        sh(tmp.path(), &["commit", "-qm", "current"]);
        sh(
            tmp.path(),
            &["update-ref", "refs/heads/aethyme/integration", "HEAD"],
        );
        let head = GitRepo::discover(tmp.path())
            .unwrap()
            .head_commit()
            .unwrap();

        let report = inspect_version_with_binary(tmp.path(), binary(&head, "v0.1.1-1-g1234567"));
        assert_eq!(report.status, VersionDriftStatus::Current);
    }

    #[test]
    fn ordinary_target_repo_is_not_compared_to_binary_source() {
        let tmp = tempfile::tempdir().unwrap();
        sh(tmp.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(tmp.path().join("README.md"), "target repo\n").unwrap();
        sh(tmp.path(), &["add", "-A"]);
        sh(tmp.path(), &["commit", "-qm", "init"]);
        sh(
            tmp.path(),
            &["update-ref", "refs/heads/aethyme/integration", "HEAD"],
        );

        let report = inspect_version_with_binary(tmp.path(), binary("deadbeef", "v0.1.1"));
        assert_eq!(report.status, VersionDriftStatus::NotAethymeSource);
    }
}
