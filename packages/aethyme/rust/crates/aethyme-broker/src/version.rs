//! Local binary/source drift detection.
//!
//! The broker can run against any repository, but source-drift comparison
//! is only meaningful when the target repository is the Aethyme source
//! checkout itself. In that case, compare the running binary's build
//! commit/describe to the repository's `aethyme/integration` branch.

use std::path::Path;

use crate::git::GitRepo;
use crate::merge::PromoteConfig;

/// Changes that can make an installed broker unsafe to use with the source
/// checkout it is coordinating. Keep this list deliberately narrow: a docs,
/// engine, or unrelated CLI change should not turn every session start into a
/// stale-binary warning.
const BROKER_CORRECTNESS_PATHS: &[&str] = &[
    "packages/aethyme/rust/crates/aethyme-broker/src/operations.rs",
    "packages/aethyme/rust/crates/aethyme-broker/src/gates.rs",
    "packages/aethyme/rust/crates/aethyme-broker/src/retention.rs",
    "packages/aethyme/rust/crates/aethyme-broker/src/gc.rs",
    "packages/aethyme/rust/crates/aethyme-broker/src/reclaim.rs",
];

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

/// Return the one-time advisory shown before a new session is created when
/// this broker binary predates correctness changes already on local `main`.
///
/// This is intentionally separate from [`inspect_version`]. Doctor compares
/// a binary with the integration tip and is useful for a full installation
/// diagnosis; session start needs a cheaper, non-blocking check against the
/// enrolled checkout's `main`, and must not treat unrelated source drift as a
/// correctness risk. A missing local `main`, an unresolvable build revision,
/// or unrelated history is inconclusive and therefore silent.
pub(crate) fn broker_start_warning(main_root: &Path) -> Option<String> {
    broker_start_warning_with_binary(main_root, current_binary_build())
}

fn broker_start_warning_with_binary(main_root: &Path, binary: BinaryBuild) -> Option<String> {
    if !is_aethyme_source_checkout(main_root) {
        return None;
    }
    let repo = GitRepo::discover(main_root).ok()?;
    let main = repo.resolve_ref("refs/heads/main")?;
    let binary_commit = resolve_binary_commit(&repo, &binary)?;
    if binary_commit == main || !repo.is_ancestor(&binary_commit, &main) {
        return None;
    }

    let mut changed = repo
        .changed_between(&binary_commit, &main)
        .ok()?
        .into_iter()
        .filter(|path| BROKER_CORRECTNESS_PATHS.contains(&path.as_str()))
        .collect::<Vec<_>>();
    if changed.is_empty() {
        return None;
    }
    changed.sort();
    changed.dedup();

    let binary_label = binary.describe.as_deref().unwrap_or(binary_commit.as_str());
    Some(format!(
        concat!(
            "installed broker build {} ({}) predates main {}; newer ",
            "broker correctness changes affect {}; this is advisory and the session will ",
            "continue, but reinstall Aethyme before relying on broker operations",
        ),
        binary_label,
        binary_commit,
        main,
        changed.join(", ")
    ))
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

    #[test]
    fn session_start_warns_for_stale_broker_correctness_code() {
        let tmp = tempfile::tempdir().unwrap();
        sh(tmp.path(), &["init", "-q", "-b", "main"]);
        marker_source_checkout(tmp.path());
        sh(tmp.path(), &["add", "-A"]);
        sh(tmp.path(), &["commit", "-qm", "installed build"]);
        let installed = GitRepo::discover(tmp.path())
            .unwrap()
            .head_commit()
            .unwrap();

        let operations = tmp
            .path()
            .join("packages/aethyme/rust/crates/aethyme-broker/src/operations.rs");
        std::fs::create_dir_all(operations.parent().unwrap()).unwrap();
        std::fs::write(operations, "new broker correctness code\n").unwrap();
        sh(tmp.path(), &["add", "-A"]);
        sh(tmp.path(), &["commit", "-qm", "broker correctness fix"]);

        let warning = broker_start_warning_with_binary(tmp.path(), binary(&installed, "v0.1.1"))
            .expect("a correctness change after the installed build must warn");
        assert!(warning.contains("predates main"), "{warning}");
        assert!(warning.contains("operations.rs"), "{warning}");
        assert!(warning.contains("advisory"), "{warning}");
    }

    #[test]
    fn session_start_ignores_unrelated_drift_and_missing_main() {
        let docs_only = tempfile::tempdir().unwrap();
        sh(docs_only.path(), &["init", "-q", "-b", "main"]);
        marker_source_checkout(docs_only.path());
        sh(docs_only.path(), &["add", "-A"]);
        sh(docs_only.path(), &["commit", "-qm", "installed build"]);
        let installed = GitRepo::discover(docs_only.path())
            .unwrap()
            .head_commit()
            .unwrap();
        std::fs::write(docs_only.path().join("README.md"), "docs\n").unwrap();
        sh(docs_only.path(), &["add", "-A"]);
        sh(docs_only.path(), &["commit", "-qm", "docs change"]);
        assert!(
            broker_start_warning_with_binary(docs_only.path(), binary(&installed, "v0.1.1"),)
                .is_none()
        );

        let no_main = tempfile::tempdir().unwrap();
        sh(no_main.path(), &["init", "-q", "-b", "feature"]);
        marker_source_checkout(no_main.path());
        sh(no_main.path(), &["add", "-A"]);
        sh(no_main.path(), &["commit", "-qm", "feature"]);
        let installed = GitRepo::discover(no_main.path())
            .unwrap()
            .head_commit()
            .unwrap();
        let gates = no_main
            .path()
            .join("packages/aethyme/rust/crates/aethyme-broker/src/gates.rs");
        std::fs::create_dir_all(gates.parent().unwrap()).unwrap();
        std::fs::write(gates, "new gate code\n").unwrap();
        sh(no_main.path(), &["add", "-A"]);
        sh(no_main.path(), &["commit", "-qm", "gate change"]);
        assert!(
            broker_start_warning_with_binary(no_main.path(), binary(&installed, "v0.1.1"),)
                .is_none()
        );
    }
}
