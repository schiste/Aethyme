//! End-to-end `quality inspect` over a real Git repository (issue #384).
//!
//! The fixture holds one genuinely non-portable path and the shapes that
//! used to be reported as false positives: temp-directory fixtures in tests
//! and a recorded run result the repository marks `linguist-generated`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_quality::engine::ScorecardEngine;
use aethyme_quality::snapshot::GeneratedFiles;

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture_repo(name: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!(
        "aethyme-quality-inspect-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q"]);

    // Genuine: a documentation link that only resolves on its author's machine.
    write(
        &root,
        "README.md",
        "See [the notes](/Users/alice/notes.md).\n",
    );
    // Temp-directory fixtures and socket paths exist on every POSIX host.
    write(
        &root,
        "tests/test_paths.py",
        concat!(
            "SOCKET_DIR = Path(\"/tmp/example-sockets\")\n",
            "assert run(repo=\"/private/tmp/example/repo\")\n",
            "CACHE = \"/var/folders/xy/T/example\"\n",
        ),
    );
    // A recorded run result: machine-specific by nature, declared generated.
    write(
        &root,
        "results/run.json",
        "{\"root\": \"/Users/alice/checkouts/run-1\"}\n",
    );
    write(&root, ".gitattributes", "results/** linguist-generated\n");
    git(&root, &["add", "."]);
    Fixture(root)
}

fn reported(root: &Path, generated: GeneratedFiles) -> Vec<(String, Option<i64>)> {
    let engine = ScorecardEngine::new(root, None, None).unwrap();
    let report = engine
        .inspect_tracked_with(Some(&["relative-links".to_string()]), generated)
        .unwrap();
    let mut findings: Vec<_> = report
        .findings
        .iter()
        .map(|finding| (finding.file_path.clone(), finding.line_number))
        .collect();
    findings.sort();
    findings
}

#[test]
fn inspect_reports_only_the_genuinely_non_portable_path() {
    let repo = fixture_repo("default");
    assert_eq!(
        reported(&repo.0, GeneratedFiles::Exclude),
        [("README.md".to_string(), Some(1))]
    );
}

#[test]
fn include_generated_brings_back_recorded_results() {
    let repo = fixture_repo("include-generated");
    assert_eq!(
        reported(&repo.0, GeneratedFiles::Include),
        [
            ("README.md".to_string(), Some(1)),
            ("results/run.json".to_string(), Some(1)),
        ]
    );
}
