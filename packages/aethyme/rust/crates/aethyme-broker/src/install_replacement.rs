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
        describe: text("describe"),
        commit: text("commit"),
        recorded_at_ms: event.ts,
    })
}

/// A build's identity for comparison: the commit when both sides know it,
/// otherwise version plus `git describe`. `None` when nothing identifies it.
fn identity(version: &str, describe: Option<&str>, commit: Option<&str>) -> String {
    match commit {
        Some(commit) => commit.to_string(),
        None => format!("{version} ({})", describe.unwrap_or("unknown describe")),
    }
}

/// `Some((recorded, current))` when the installed build differs from the
/// session's record. Builds are compared by commit when both sides carry
/// one, and by version and describe otherwise, so a build without commit
/// metadata never reads as a change on its own.
pub fn replacement(recorded: &RecordedInstall, current: &BinaryBuild) -> Option<(String, String)> {
    let both_have_commits = recorded.commit.is_some() && current.commit.is_some();
    let (before, after) = if both_have_commits {
        (
            identity(&recorded.version, None, recorded.commit.as_deref()),
            identity(&current.version, None, current.commit.as_deref()),
        )
    } else {
        (
            identity(&recorded.version, recorded.describe.as_deref(), None),
            identity(&current.version, current.describe.as_deref(), None),
        )
    };
    (before != after).then_some((before, after))
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
            describe: describe.map(str::to_string),
            commit: commit.map(str::to_string),
            recorded_at_ms: 1,
        }
    }

    fn build(version: &str, describe: Option<&str>, commit: Option<&str>) -> BinaryBuild {
        BinaryBuild {
            version: version.into(),
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
