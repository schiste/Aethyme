//! Bounded, read-only navigation when no trustworthy graph is available.
//! These are lexical source hints, never evidence of callers or impact closure.
use std::io::Read;
use std::path::{Component, Path};
use std::process::{Command, Stdio};

use super::{AnswerItem, ExploreSubsystem, ExploreSubsystemTarget};

const MAX_INDEX_BYTES: u64 = 1_048_576;
const MAX_PATHS: usize = 10_000;
const MAX_FILES: usize = 128;
const PREFIX_BYTES: u64 = 8_192;
const MAX_HINTS: usize = 4;

#[derive(Default)]
pub(super) struct SourceFallback {
    pub hints: Vec<AnswerItem>,
    pub subsystems: Vec<ExploreSubsystem>,
    pub scanned_files: usize,
    pub truncated: bool,
}

pub(super) fn inspect(repo: &Path, request: &str) -> SourceFallback {
    let mut result = SourceFallback::default();
    let Ok(root) = repo.canonicalize() else {
        return result;
    };
    // Only indexed paths: no ignored secrets, build output, or untracked data.
    // Stream a bounded prefix, then reap the process even when the cap is hit.
    let Ok(mut child) = Command::new("git")
        .args(["ls-files", "--cached", "-z", "--"])
        .current_dir(&root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return result;
    };
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = stdout.take(MAX_INDEX_BYTES + 1).read_to_end(&mut bytes);
        let _ = sender.send((read, bytes));
    });
    let Ok((read, mut bytes)) = receiver.recv_timeout(std::time::Duration::from_secs(2)) else {
        let _ = child.kill();
        let _ = child.wait();
        result.truncated = true;
        return result;
    };
    if bytes.len() as u64 > MAX_INDEX_BYTES {
        result.truncated = true;
        let _ = child.kill();
        bytes.truncate(MAX_INDEX_BYTES as usize);
        if let Some(last) = bytes.iter().rposition(|byte| *byte == 0) {
            bytes.truncate(last + 1);
        } else {
            bytes.clear();
        }
    }
    let status = child.wait();
    if read.is_err() || (!result.truncated && !status.is_ok_and(|status| status.success())) {
        return result;
    }
    let stop_words = [
        "the", "and", "for", "from", "with", "this", "that", "where", "what", "how", "does",
        "which", "find", "show",
    ];
    let terms = request
        .split(|ch: char| !ch.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|word| word.len() >= 3 && !stop_words.contains(&word.as_str()))
        .take(12)
        .collect::<Vec<_>>();
    let mut paths = Vec::new();
    for entry in bytes
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        if paths.len() == MAX_PATHS {
            result.truncated = true;
            break;
        }
        let Ok(path) = std::str::from_utf8(entry) else {
            continue;
        };
        if !Path::new(path).components().all(|component| matches!(component, Component::Normal(name) if !name.to_string_lossy().starts_with('.'))) {
            continue;
        }
        let lower = path.to_lowercase();
        let score = terms
            .iter()
            .filter(|term| lower.contains(term.as_str()))
            .count()
            * 4;
        let entrypoint = matches!(
            path,
            "README.md" | "Cargo.toml" | "package.json" | "pyproject.toml" | "go.mod"
        );
        paths.push((score + usize::from(entrypoint), path.to_string()));
    }
    paths.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    paths.dedup();
    result.truncated |= paths.len() > MAX_FILES;
    let mut candidates = Vec::new();
    for (mut score, path) in paths.into_iter().take(MAX_FILES) {
        let full = root.join(&path);
        let Ok(metadata) = full.symlink_metadata() else {
            continue;
        };
        if !metadata.is_file()
            || !full
                .canonicalize()
                .is_ok_and(|resolved| resolved.starts_with(&root))
        {
            continue;
        }
        let Ok(file) = std::fs::File::open(&full) else {
            continue;
        };
        let mut prefix = Vec::new();
        if file.take(PREFIX_BYTES).read_to_end(&mut prefix).is_err() || prefix.contains(&0) {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&prefix) else {
            continue;
        };
        result.scanned_files += 1;
        result.truncated |= metadata.len() > PREFIX_BYTES;
        let line = text
            .lines()
            .position(|line| {
                let line = line.to_lowercase();
                terms.iter().any(|term| line.contains(term))
            })
            .map(|index| index + 1);
        score += usize::from(line.is_some());
        if score == 0 {
            continue;
        }
        candidates.push((score, AnswerItem {
            kind: "source_file".into(), target: path.clone(), path: Some(path),
            status: "navigation_hint".into(), confidence: 0.3,
            reason: "Bounded tracked-file/path match; inspect source before making semantic claims".into(),
            role: "source_navigation".into(),
            evidence: serde_json::json!({ "source": "bounded_tracked_source", "graph_available": false,
                "line_refs": line.into_iter().map(|line| serde_json::json!({"line": line})).collect::<Vec<_>>() }),
        }));
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.target.cmp(&b.1.target)));
    result.truncated |= candidates.len() > MAX_HINTS;
    result.hints = candidates
        .into_iter()
        .take(MAX_HINTS)
        .map(|(_, hint)| hint)
        .collect();
    if !result.hints.is_empty() {
        result.subsystems.push(ExploreSubsystem {
            rank: 1, id: "bounded_source_fallback".into(), label: "Source locations to verify".into(),
            role: "navigation_only".into(), confidence: 0.3,
            paths: result.hints.iter().filter_map(|hint| hint.path.clone()).collect(), token_subsystems: vec![],
            top_verification_targets: result.hints.iter().map(|hint| ExploreSubsystemTarget {
                kind: hint.kind.clone(), target: hint.target.clone(), path: hint.path.clone(),
                reason: hint.reason.clone(), confidence: hint.confidence,
            }).collect(),
            signals: vec!["bounded_tracked_source".into()],
            missing_coverage_warnings: vec!["No graph: callers, dependency closure, test selection and documentation impact are unavailable".into()],
        });
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fallback_is_bounded_tracked_navigation_not_graph_evidence() {
        let repo = tempfile::tempdir().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(repo.path())
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(repo.path().join("storage.rs"), "fn cleanup_cache() {}\n").unwrap();
        std::fs::write(repo.path().join("untracked.rs"), "cleanup cache\n").unwrap();
        std::fs::write(repo.path().join(".env"), "cleanup cache secret\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/passwd", repo.path().join("cleanup.rs")).unwrap();
        assert!(
            Command::new("git")
                .args(["add", "--all"])
                .current_dir(repo.path())
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(repo.path().join("later.rs"), "cleanup cache\n").unwrap();
        let result = inspect(repo.path(), "where is cleanup cache");
        assert!(!result.hints.is_empty());
        assert!(result.hints.iter().any(|hint| hint.target == "storage.rs"));
        assert!(
            result
                .hints
                .iter()
                .all(|hint| ![".env", "cleanup.rs", "later.rs"].contains(&hint.target.as_str()))
        );
        assert!(result.hints.len() <= MAX_HINTS);
        for hint in &result.hints {
            assert_eq!(hint.evidence["graph_available"], false);
        }
        let envelope = super::super::graph_unavailable_response(
            repo.path(),
            "cleanup cache",
            "task_localization_query",
            "test",
            "missing",
            "fixture".into(),
        );
        assert!(!envelope.safe_to_use_as_answer);
        assert!(envelope.safe_to_use_as_navigation);
        assert!(envelope.answer.is_empty());
        assert!(!repo.path().join(".aethyme").exists());
    }

    #[test]
    fn missing_repository_has_no_claims() {
        let repo = tempfile::tempdir().unwrap();
        let result = inspect(&repo.path().join("missing"), "cleanup");
        assert!(result.hints.is_empty());
    }

    #[test]
    fn scan_and_hint_caps_are_explicit_and_deterministic() {
        let repo = tempfile::tempdir().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(repo.path())
                .status()
                .unwrap()
                .success()
        );
        for index in 0..MAX_FILES + 5 {
            std::fs::write(repo.path().join(format!("cache-{index:04}.rs")), "cache\n").unwrap();
        }
        assert!(
            Command::new("git")
                .args(["add", "--all"])
                .current_dir(repo.path())
                .status()
                .unwrap()
                .success()
        );
        let first = inspect(repo.path(), "cache");
        let second = inspect(repo.path(), "cache");
        assert!(first.truncated);
        assert_eq!(first.scanned_files, MAX_FILES);
        assert_eq!(first.hints.len(), MAX_HINTS);
        assert_eq!(
            first
                .hints
                .iter()
                .map(|hint| &hint.target)
                .collect::<Vec<_>>(),
            second
                .hints
                .iter()
                .map(|hint| &hint.target)
                .collect::<Vec<_>>()
        );
    }
}
