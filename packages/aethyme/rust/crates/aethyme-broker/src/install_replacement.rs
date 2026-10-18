//! Whether the installed Aethyme changed under a live session (#293).
//!
//! The router and engine on PATH are one machine-wide pair that any session
//! may replace with `cargo install` or an update, at any time. A session that
//! started on one build and keeps running commands on another gets different
//! behaviour with no announcement. `start` and `adopt` record the build that
//! ran them; `status` compares that record with the build running now and
//! says when they differ. Nothing is locked or refused: the decision on #293
//! was to record the swap and provide one supported upgrade path
//! (`aethyme self-update`), not to forbid replacing the pair.

use crate::version::BinaryBuild;

/// The build a session recorded, as read back from its event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedInstall {
    pub version: String,
    pub revision: Option<String>,
    pub describe: Option<String>,
    pub commit: Option<String>,
    pub recorded_at_ms: i64,
}

/// Record the build running this command as the session's installed build.
/// Best effort: a session exists whether or not this succeeds.
pub fn record(store: &mut crate::BrokerStore, session_id: i64) {
    let engine = crate::install_health::version_banner(crate::install_health::ENGINE_BINARY);
    if let Err(error) = store.append_event(
        crate::events::SESSION_INSTALL_RECORDED,
        Some(session_id),
        Some(&crate::events::session_install_recorded_payload(
            &crate::version::current_binary_build(),
            engine.as_deref(),
        )),
    ) {
        eprintln!("warning: could not record session {session_id}'s installed build: {error}");
    }
}

/// The session's recorded build, or `None` when it has none or it is unreadable.
pub fn recorded(store: &crate::BrokerStore, session_id: i64) -> Option<RecordedInstall> {
    let event = store.latest_session_install_event(session_id).ok()??;
    let payload: serde_json::Value = serde_json::from_str(event.payload_json.as_deref()?).ok()?;
    let text = |key: &str| payload.get(key)?.as_str().map(str::to_string);
    Some(RecordedInstall {
        version: text("version")?,
        revision: text("revision").or_else(|| text("describe")),
        describe: text("describe"),
        commit: text("commit"),
        recorded_at_ms: event.ts,
    })
}

/// A build's identity for comparison. Full revisions include tracked dirty
/// state; historical events may only carry a legacy `describe` value.
fn identity(version: &str, revision: Option<&str>, commit: Option<&str>) -> String {
    if let Some(revision) = revision.filter(|revision| is_exact_revision(revision)) {
        return format!("{version} ({revision})");
    }
    match commit {
        Some(commit) => commit.to_string(),
        None => format!("{version} ({})", revision.unwrap_or("unknown revision")),
    }
}

fn is_exact_revision(revision: &str) -> bool {
    let oid = revision.strip_suffix("-dirty").unwrap_or(revision);
    matches!(oid.len(), 40 | 64) && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// `Some((recorded, current))` when the installed build differs from the
/// session's record. Compare package version and exact full revisions when
/// both sides carry them so a tracked dirty build differs from a clean build
/// of the same commit. Otherwise compare commit OIDs when both are known; if
/// either is missing, compare package version and legacy `describe` labels so
/// adding commit metadata alone does not appear to replace the build.
pub fn replacement(recorded: &RecordedInstall, current: &BinaryBuild) -> Option<(String, String)> {
    let recorded_revision = recorded
        .revision
        .as_deref()
        .or(recorded.describe.as_deref());
    let current_revision = current.revision.as_deref().or(current.describe.as_deref());
    let both_have_exact_revisions = recorded_revision.is_some_and(is_exact_revision)
        && current_revision.is_some_and(is_exact_revision);
    let both_have_commits = recorded.commit.is_some() && current.commit.is_some();
    let before = identity(
        &recorded.version,
        recorded_revision,
        recorded.commit.as_deref(),
    );
    let after = identity(
        &current.version,
        current_revision,
        current.commit.as_deref(),
    );
    let changed = if both_have_exact_revisions {
        before != after
    } else if both_have_commits {
        recorded.commit.as_deref() != current.commit.as_deref()
    } else {
        (recorded.version.as_str(), recorded.describe.as_deref())
            != (current.version.as_str(), current.describe.as_deref())
    };
    changed.then_some((before, after))
}

/// `install.replaced` advice for each live session whose recorded build is
/// not the build running this command.
pub(crate) fn advice(
    store: &crate::BrokerStore,
    session_ids: &[i64],
    current: &BinaryBuild,
) -> Vec<crate::StatusAdvice> {
    session_ids
        .iter()
        .filter_map(|session_id| {
            let recorded = recorded(store, *session_id)?;
            let (before, after) = replacement(&recorded, current)?;
            Some(crate::StatusAdvice {
                id: "install.replaced",
                severity: crate::StatusAdviceSeverity::Warning,
                reason: "the installed aethyme changed after this session started",
                summary: format!(
                    "session {session_id} started on aethyme {before} (recorded at {} ms), but \
                     this command runs {after}; behaviour may differ from what the session \
                     saw so far",
                    recorded.recorded_at_ms
                ),
                session_id: Some(*session_id),
                queue_entry_id: None,
                evidence: vec![
                    format!("recorded: {before} at {} ms", recorded.recorded_at_ms),
                    format!(
                        "running: {after}{}",
                        current
                            .path
                            .as_deref()
                            .map(|path| format!(" ({path})"))
                            .unwrap_or_default()
                    ),
                ],
                commands: vec!["aethyme --version".into(), "aethyme self-update".into()],
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorded(version: &str, describe: Option<&str>, commit: Option<&str>) -> RecordedInstall {
        RecordedInstall {
            version: version.into(),
            revision: commit
                .map(str::to_string)
                .or_else(|| describe.map(str::to_string)),
            describe: describe.map(str::to_string),
            commit: commit.map(str::to_string),
            recorded_at_ms: 1,
        }
    }

    fn build(version: &str, describe: Option<&str>, commit: Option<&str>) -> BinaryBuild {
        BinaryBuild {
            version: version.into(),
            revision: commit
                .map(str::to_string)
                .or_else(|| describe.map(str::to_string)),
            describe: describe.map(str::to_string),
            commit: commit.map(str::to_string),
            path: Some("/usr/local/bin/aethyme".into()),
        }
    }

    #[test]
    fn the_same_commit_is_not_a_replacement() {
        let same = recorded("0.8.22", Some("v0.8.22"), Some("aaa"));
        assert_eq!(
            replacement(&same, &build("0.8.22", Some("v0.8.22"), Some("aaa"))),
            None
        );
    }

    #[test]
    fn a_different_commit_is_a_replacement_named_on_both_sides() {
        let (before, after) = replacement(
            &recorded("0.8.22", Some("v0.8.22"), Some("aaa")),
            &build("0.8.22", Some("v0.8.22-3-gbbb"), Some("bbb")),
        )
        .expect("a different commit is a replacement");
        assert_eq!((before.as_str(), after.as_str()), ("aaa", "bbb"));
    }

    #[test]
    fn tracked_dirty_revision_is_a_replacement_in_both_directions() {
        const COMMIT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let clean = recorded("0.8.22", Some(COMMIT), Some(COMMIT));
        let clean_build = build("0.8.22", Some(COMMIT), Some(COMMIT));
        let dirty_revision = format!("{COMMIT}-dirty");
        let mut dirty = recorded("0.8.22", Some(&dirty_revision), Some(COMMIT));
        dirty.revision = Some(dirty_revision.clone());
        let mut dirty_build = build("0.8.22", Some(&dirty_revision), Some(COMMIT));
        dirty_build.revision = Some(dirty_revision.clone());

        assert_eq!(
            replacement(&clean, &dirty_build).map(|(_, after)| after),
            Some(format!("0.8.22 ({dirty_revision})"))
        );
        assert_eq!(
            replacement(&dirty, &clean_build).map(|(before, _)| before),
            Some(format!("0.8.22 ({dirty_revision})"))
        );
        assert_eq!(replacement(&clean, &clean_build), None);
    }

    #[test]
    fn a_legacy_describe_and_new_revision_for_the_same_commit_are_compatible() {
        const COMMIT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let legacy_describe = "v0.8.22-3-gaaaaaaaaaaaa";
        let mut old_build = recorded("0.8.22", Some(legacy_describe), Some(COMMIT));
        old_build.revision = Some(legacy_describe.into());
        let new_build = build("0.8.22", Some(COMMIT), Some(COMMIT));

        assert_eq!(replacement(&old_build, &new_build), None);
    }

    #[test]
    fn one_sided_commit_metadata_keeps_full_revision_builds_legacy_compatible() {
        const COMMIT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let legacy_describe = "v0.8.22-3-gaaaaaaaaaaaa";
        let old_record = recorded("0.8.22", Some(legacy_describe), None);
        let new_build = build("0.8.22", Some(legacy_describe), Some(COMMIT));
        assert_eq!(replacement(&old_record, &new_build), None);

        let new_record = recorded("0.8.22", Some(legacy_describe), Some(COMMIT));
        let old_build = build("0.8.22", Some(legacy_describe), None);
        assert_eq!(replacement(&new_record, &old_build), None);
    }

    #[test]
    fn without_commits_version_and_describe_decide() {
        assert_eq!(
            replacement(
                &recorded("0.8.22", Some("v0.8.22"), None),
                &build("0.8.22", Some("v0.8.22"), Some("bbb"))
            ),
            None,
            "a build that merely gained commit metadata is not a change"
        );
        assert!(
            replacement(
                &recorded("0.8.21", Some("v0.8.21"), None),
                &build("0.8.22", Some("v0.8.22"), None)
            )
            .is_some()
        );
    }
}
