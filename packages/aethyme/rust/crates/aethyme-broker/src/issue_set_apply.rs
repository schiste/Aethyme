//! Crash-safe issue-set application. The backend owns GitHub policy and
//! coordinated writes; this layer owns the per-set lock and create journal.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::issue_set::{Entry, ValidatedManifest};

/// A backend must make reads for the target repository, verify exact markers,
/// and journal each GitHub write through the coordinator. Ensure operations
/// must succeed without mutating an already-present relationship.
pub(crate) trait IssueSetBackend {
    fn validate_labels(&mut self, labels: &BTreeSet<String>) -> Result<(), String>;
    fn find_marker(&mut self, marker: &str) -> Result<Vec<u64>, String>;
    fn has_marker(&mut self, number: u64, marker: &str) -> Result<bool, String>;
    fn create(&mut self, entry: &Entry, marker: &str) -> Result<u64, String>;
    fn update(&mut self, number: u64, entry: &Entry, body: &str) -> Result<(), String>;
    fn ensure_sub_issue(&mut self, parent: u64, child: u64) -> Result<(), String>;
    fn ensure_dependency(&mut self, issue: u64, blocked_by: u64) -> Result<(), String>;
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    repository: String,
    set_id: String,
    /// Set before create, cleared only after its number is persisted.
    pending: BTreeSet<String>,
    numbers: BTreeMap<String, u64>,
}

fn save(path: &Path, journal: &Journal) -> Result<(), String> {
    let bytes = serde_json::to_vec(journal).map_err(|e| e.to_string())?;
    crate::atomic_file::with_synced_temporary(path, &bytes, |temporary| {
        std::fs::rename(temporary, path)
    })
    .map_err(|e| format!("cannot persist issue-set journal: {e}"))?;
    #[cfg(unix)]
    std::fs::File::open(path.parent().ok_or("journal has no parent")?)
        .and_then(|directory| directory.sync_all())
        .map_err(|e| format!("cannot sync issue-set journal directory: {e}"))?;
    Ok(())
}

fn open_journal(
    manifest: &ValidatedManifest,
    run_dir: &Path,
    repository: &str,
) -> Result<
    (
        Journal,
        std::path::PathBuf,
        crate::file_lock::ExclusiveFileLock,
    ),
    String,
> {
    let parts: Vec<_> = repository.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || matches!(*part, "." | "..")
                || !part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
    {
        return Err("invalid repository; expected owner/name".into());
    }
    let repository = repository.to_ascii_lowercase();
    std::fs::create_dir_all(run_dir)
        .map_err(|e| format!("cannot create journal directory: {e}"))?;
    let digest = format!(
        "{:x}",
        Sha256::digest(format!("{repository}\0{}", manifest.manifest.set_id))
    );
    let journal_path = run_dir.join(format!("issue-set-{digest}.json"));
    let lock_path = run_dir.join(format!("issue-set-{digest}.lock"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("cannot open issue-set lock: {e}"))?;
    let lock = crate::file_lock::ExclusiveFileLock::acquire(file)
        .map_err(|e| format!("cannot lock issue set: {e}"))?;
    let journal: Journal = match std::fs::read(&journal_path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| format!("invalid issue-set journal: {e}"))?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Journal {
            repository: repository.clone(),
            set_id: manifest.manifest.set_id.clone(),
            ..Journal::default()
        },
        Err(error) => return Err(format!("cannot read issue-set journal: {error}")),
    };
    if journal.repository != repository || journal.set_id != manifest.manifest.set_id {
        return Err("issue-set journal target does not match the requested set".into());
    }
    Ok((journal, journal_path, lock))
}

fn resolve_existing(
    manifest: &ValidatedManifest,
    journal: &mut Journal,
    backend: &mut dyn IssueSetBackend,
) -> Result<(), String> {
    let entries: BTreeMap<_, _> = std::iter::once(&manifest.manifest.parent)
        .chain(&manifest.manifest.issues)
        .map(|entry| (entry.key.clone(), entry))
        .collect();
    let labels = entries
        .values()
        .flat_map(|entry| entry.labels.iter().cloned())
        .collect();
    backend.validate_labels(&labels)?;
    // Resolve every existing issue and pending create before the first write.
    for key in &manifest.order {
        let marker = format!(
            "<!-- aethyme-issue-set:{}/{key} -->",
            manifest.manifest.set_id
        );
        if let Some(number) = journal.numbers.get(key) {
            if *number == 0 || !backend.has_marker(*number, &marker)? {
                return Err(format!(
                    "issue #{number} no longer carries the expected marker for {key}"
                ));
            }
            continue;
        }
        let matches = backend.find_marker(&marker)?;
        match matches.as_slice() {
            [number] if *number > 0 => {
                journal.numbers.insert(key.clone(), *number);
                journal.pending.remove(key);
            }
            [] if journal.pending.contains(key) => {
                return Err(format!(
                    "create for {key} has an unknown outcome; inspect GitHub and reconcile before retrying"
                ));
            }
            [] => {}
            _ => return Err(format!("ambiguous issue-set marker for {key}")),
        }
    }
    let mut allocated = BTreeSet::new();
    if journal
        .numbers
        .iter()
        .filter(|(key, _)| entries.contains_key(*key))
        .any(|(_, number)| !allocated.insert(*number))
    {
        return Err("multiple issue-set keys refer to the same GitHub issue".into());
    }
    Ok(())
}

/// Plan using the same locked journal and recovery checks as apply. This never
/// writes GitHub or changes the journal, including for uncertain creates.
pub(crate) fn plan(
    manifest: &ValidatedManifest,
    run_dir: &Path,
    repository: &str,
    backend: &mut dyn IssueSetBackend,
) -> Result<BTreeMap<String, Option<u64>>, String> {
    let (mut journal, _path, _lock) = open_journal(manifest, run_dir, repository)?;
    resolve_existing(manifest, &mut journal, backend)?;
    Ok(manifest
        .order
        .iter()
        .map(|key| (key.clone(), journal.numbers.get(key).copied()))
        .collect())
}

/// The caller must have checked the target repository's trusted policy before
/// constructing a backend. An unknown create is never repeated without first
/// finding its exact marker; an absent marker leaves the set blocked.
pub(crate) fn apply(
    manifest: &ValidatedManifest,
    run_dir: &Path,
    repository: &str,
    backend: &mut dyn IssueSetBackend,
) -> Result<BTreeMap<String, u64>, String> {
    let (mut journal, journal_path, _lock) = open_journal(manifest, run_dir, repository)?;
    resolve_existing(manifest, &mut journal, backend)?;
    let entries: BTreeMap<_, _> = std::iter::once(&manifest.manifest.parent)
        .chain(&manifest.manifest.issues)
        .map(|entry| (entry.key.clone(), entry))
        .collect();
    let mut allocated: BTreeSet<u64> = journal.numbers.values().copied().collect();
    save(&journal_path, &journal)?;
    for key in &manifest.order {
        if journal.numbers.contains_key(key) {
            continue;
        }
        let marker = format!(
            "<!-- aethyme-issue-set:{}/{key} -->",
            manifest.manifest.set_id
        );
        journal.pending.insert(key.clone());
        save(&journal_path, &journal)?;
        let number = backend.create(entries[key], &marker)?;
        if number == 0 || !allocated.insert(number) {
            return Err(format!(
                "create for {key} returned an invalid or duplicate issue number"
            ));
        }
        journal.numbers.insert(key.clone(), number);
        journal.pending.remove(key);
        save(&journal_path, &journal)?;
    }
    for key in &manifest.order {
        let body = format!(
            "{}\n\n<!-- aethyme-issue-set:{}/{key} -->\n",
            manifest.render_body(key, &journal.numbers)?,
            manifest.manifest.set_id
        );
        backend.update(journal.numbers[key], entries[key], &body)?;
    }
    let parent = journal.numbers[&manifest.manifest.parent.key];
    for entry in &manifest.manifest.issues {
        backend.ensure_sub_issue(parent, journal.numbers[&entry.key])?;
    }
    for entry in entries.values() {
        for dependency in &entry.depends_on {
            backend.ensure_dependency(journal.numbers[&entry.key], journal.numbers[dependency])?;
        }
    }
    Ok(journal.numbers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        issues: BTreeMap<u64, (String, String)>,
        links: BTreeSet<(u64, u64)>,
        dependencies: BTreeSet<(u64, u64)>,
        create_calls: usize,
        lose_create_response: bool,
    }
    impl IssueSetBackend for Fake {
        fn validate_labels(&mut self, _labels: &BTreeSet<String>) -> Result<(), String> {
            Ok(())
        }
        fn find_marker(&mut self, marker: &str) -> Result<Vec<u64>, String> {
            Ok(self
                .issues
                .iter()
                .filter(|(_, (body, _))| body.contains(marker))
                .map(|(id, _)| *id)
                .collect())
        }
        fn has_marker(&mut self, number: u64, marker: &str) -> Result<bool, String> {
            Ok(self
                .issues
                .get(&number)
                .is_some_and(|(body, _)| body.contains(marker)))
        }
        fn create(&mut self, entry: &Entry, marker: &str) -> Result<u64, String> {
            self.create_calls += 1;
            let id = self.create_calls as u64 + 100;
            self.issues.insert(id, (marker.into(), entry.title.clone()));
            if self.lose_create_response {
                self.lose_create_response = false;
                return Err("lost response".into());
            }
            Ok(id)
        }
        fn update(&mut self, number: u64, entry: &Entry, body: &str) -> Result<(), String> {
            self.issues
                .insert(number, (body.into(), entry.title.clone()));
            Ok(())
        }
        fn ensure_sub_issue(&mut self, parent: u64, child: u64) -> Result<(), String> {
            self.links.insert((parent, child));
            Ok(())
        }
        fn ensure_dependency(&mut self, issue: u64, blocked_by: u64) -> Result<(), String> {
            self.dependencies.insert((issue, blocked_by));
            Ok(())
        }
    }
    fn fixture() -> (tempfile::TempDir, ValidatedManifest) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("parent.md"), "Child {{child}}").unwrap();
        std::fs::write(dir.path().join("child.md"), "Parent {{epic}}").unwrap();
        let path = dir.path().join("set.toml");
        std::fs::write(
            &path,
            r#"set_id = "sample"
[parent]
key = "epic"
title = "Epic"
body_file = "parent.md"
[[issues]]
key = "child"
title = "Child"
body_file = "child.md"
depends_on = ["epic"]
"#,
        )
        .unwrap();
        let manifest = ValidatedManifest::load(&path).unwrap();
        (dir, manifest)
    }
    #[test]
    fn applies_and_reruns_without_new_issues() {
        let (dir, manifest) = fixture();
        let mut backend = Fake::default();
        let numbers = apply(&manifest, dir.path(), "owner/repo", &mut backend).unwrap();
        assert_eq!(backend.create_calls, 2);
        assert_eq!(
            backend.links,
            BTreeSet::from([(numbers["epic"], numbers["child"])])
        );
        assert_eq!(
            backend.dependencies,
            BTreeSet::from([(numbers["child"], numbers["epic"])])
        );
        assert!(
            backend.issues[&numbers["epic"]]
                .0
                .contains(&format!("Child #{}", numbers["child"]))
        );
        assert_eq!(
            apply(&manifest, dir.path(), "owner/repo", &mut backend).unwrap(),
            numbers
        );
        assert_eq!(backend.create_calls, 2);
        assert_eq!(
            apply(&manifest, dir.path(), "Owner/Repo", &mut backend).unwrap(),
            numbers
        );
        assert_eq!(backend.create_calls, 2);
    }
    #[test]
    fn plan_is_read_only_and_matches_apply_recovery_state() {
        let (dir, manifest) = fixture();
        let mut backend = Fake::default();
        let planned = plan(&manifest, dir.path(), "owner/repo", &mut backend).unwrap();
        assert_eq!(
            planned.values().filter(|number| number.is_none()).count(),
            2
        );
        assert_eq!(backend.create_calls, 0);
        let numbers = apply(&manifest, dir.path(), "owner/repo", &mut backend).unwrap();
        let planned = plan(&manifest, dir.path(), "owner/repo", &mut backend).unwrap();
        assert_eq!(planned["epic"], Some(numbers["epic"]));
        assert_eq!(planned["child"], Some(numbers["child"]));
        assert_eq!(backend.create_calls, 2);
    }
    #[test]
    fn interrupted_create_is_adopted_when_marker_is_found() {
        let (dir, manifest) = fixture();
        let mut backend = Fake {
            lose_create_response: true,
            ..Fake::default()
        };
        assert!(apply(&manifest, dir.path(), "owner/repo", &mut backend).is_err());
        let numbers = apply(&manifest, dir.path(), "owner/repo", &mut backend).unwrap();
        assert_eq!(numbers["epic"], 101);
        assert_eq!(backend.create_calls, 2);
    }
    #[test]
    fn uncertain_create_without_visible_marker_never_retries_blindly() {
        let (dir, manifest) = fixture();
        let mut backend = Fake {
            lose_create_response: true,
            ..Fake::default()
        };
        assert!(apply(&manifest, dir.path(), "owner/repo", &mut backend).is_err());
        backend.issues.clear();
        let error = plan(&manifest, dir.path(), "owner/repo", &mut backend).unwrap_err();
        assert!(error.contains("unknown outcome"));
        let error = apply(&manifest, dir.path(), "owner/repo", &mut backend).unwrap_err();
        assert!(error.contains("unknown outcome"));
        assert_eq!(backend.create_calls, 1);
    }
}
