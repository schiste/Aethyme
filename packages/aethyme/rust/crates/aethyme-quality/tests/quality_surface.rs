//! Integration coverage for the assembled quality surface and each public autofix fixer (#381).
//!
//! The checked-in fixture is copied to a temporary repository so these tests
//! exercise realistic tracked files without measuring or changing Aethyme.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use aethyme_quality::detectors::DetectorApplicability;
use aethyme_quality::engine::ScorecardEngine;
use aethyme_quality::fix::FixSelection;
use aethyme_quality::fix::command::{CommandRunner, RunOutcome};
use aethyme_quality::fix::fixers::{FixProposal, FormatFixer, process_directory};
use aethyme_quality::fix::patch::{ApplyOutcome, PatchGenerator};
use aethyme_quality::fix::safety::SafetyEngine;

const DETECTORS: [&str; 8] = [
    "data-ui-coverage",
    "folder-docs",
    "relative-links",
    "i18n-gaps",
    "generated-files",
    "schema-drift",
    "route-coverage",
    "ability-coverage",
];

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "aethyme-quality-surface-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/quality_surface"),
            &root,
        );
        git(&root, &["init", "-q"]);
        git(&root, &["add", "-A"]);
        Self(root)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.0.join(path)).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!("could not remove {}: {error}", self.0.display());
        }
    }
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = destination.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            fs::copy(from, to).unwrap();
        }
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn assembled_scorecard_and_tracked_inspection_cover_every_detector() {
    let fixture = Fixture::new();
    let engine = ScorecardEngine::new(fixture.path(), None, None).unwrap();

    // The legacy assembled scan exercises all eight detectors, including the
    // generated-file detector that the maintained tracked inspection excludes.
    let report = engine.scan(None);
    assert_eq!(report.detector_results.len(), DETECTORS.len());
    for name in DETECTORS {
        let detector = report
            .detector_results
            .iter()
            .find(|result| result.detector_name == name)
            .unwrap_or_else(|| panic!("registry did not run {name}"));
        assert!(
            !detector.findings.is_empty(),
            "fixture did not exercise {name}"
        );
    }

    // The maintained surface must report applicable detector findings and
    // explicitly mark generated-files as excluded rather than silently clean.
    let inspection = engine.inspect_tracked(None).unwrap();
    assert_eq!(inspection.detector_results.len(), DETECTORS.len());
    for name in DETECTORS {
        let detector = inspection
            .detector_results
            .iter()
            .find(|result| result.detector_name == name)
            .unwrap_or_else(|| panic!("inspection omitted {name}"));
        if name == "generated-files" {
            assert!(matches!(
                &detector.applicability,
                DetectorApplicability::NotApplicable { .. }
            ));
            assert!(detector.findings.is_empty());
        } else {
            assert!(
                matches!(
                    &detector.applicability,
                    DetectorApplicability::Applicable { .. }
                ),
                "{name}: {:?}",
                detector.applicability
            );
            assert!(
                !detector.findings.is_empty(),
                "fixture did not exercise {name}"
            );
        }
    }
    assert!(inspection.findings.iter().any(|finding| {
        finding.detector == "relative-links" && finding.file_path == "README.md"
    }));
}

#[derive(Default)]
struct FixtureFormatter;

impl CommandRunner for FixtureFormatter {
    fn run(
        &self,
        argv: &[&str],
        stdin: Option<&[u8]>,
        _timeout: Option<std::time::Duration>,
        _cwd: Option<&Path>,
    ) -> RunOutcome {
        if argv.last() == Some(&"--version") {
            return if argv.first() == Some(&"black") {
                RunOutcome::Completed {
                    code: 0,
                    stdout: b"fixture black\n".to_vec(),
                }
            } else {
                RunOutcome::Failed
            };
        }
        if argv.first() == Some(&"black") {
            let input = stdin.unwrap_or_default();
            let output = if String::from_utf8_lossy(input).contains("router = APIRouter()") {
                b"# formatted route fixture\n".to_vec()
            } else {
                input.to_vec()
            };
            return RunOutcome::Completed {
                code: 0,
                stdout: output,
            };
        }
        RunOutcome::Failed
    }
}

fn collect(repo: &Path, group: FixSelection) -> Vec<FixProposal> {
    aethyme_quality::fix::collect_group(repo, FixSelection::All, group).unwrap()
}

fn append_to_patch(patch: &mut PatchGenerator, proposals: Vec<FixProposal>) {
    for proposal in proposals {
        assert!(
            patch
                .add_patch(
                    &proposal.file_path,
                    &proposal.original_content,
                    &proposal.new_content,
                    &proposal.fix_type,
                )
                .is_some(),
            "safety engine rejected {:?}",
            proposal.file_path
        );
    }
}

#[test]
fn every_public_fixer_proposes_and_applies_a_fixture_change() {
    let fixture = Fixture::new();
    let root = fixture.path();
    let mut patch = PatchGenerator::new(root, SafetyEngine::new());

    let docs = collect(root, FixSelection::Docs);
    assert!(
        docs.iter()
            .any(|proposal| proposal.file_path.ends_with("src/api/FOLDER.md"))
    );
    append_to_patch(&mut patch, docs);

    let links = collect(root, FixSelection::Links);
    assert!(
        links
            .iter()
            .any(|proposal| proposal.new_content.contains("[API](docs/api.md)"))
    );
    append_to_patch(&mut patch, links);

    let selectors = collect(root, FixSelection::Selectors);
    assert!(
        selectors
            .iter()
            .any(|proposal| proposal.new_content.contains("data-ui="))
    );
    append_to_patch(&mut patch, selectors);

    let i18n = collect(root, FixSelection::I18n);
    assert!(
        i18n.iter()
            .any(|proposal| proposal.file_path.ends_with("Notice.tsx"))
    );
    append_to_patch(&mut patch, i18n);

    let formatter = FormatFixer::with_runner(Box::new(FixtureFormatter));
    let formatting = process_directory(&formatter, root);
    assert!(formatting.iter().any(|proposal| {
        proposal.file_path.ends_with("routes.py")
            && proposal.new_content == "# formatted route fixture\n"
    }));
    append_to_patch(&mut patch, formatting);

    let outcome = patch.apply(true);
    match outcome {
        ApplyOutcome::Executed {
            applied, failed, ..
        } => {
            // Button.tsx is proposed by both the selector inserter and the
            // i18n scaffolder, each from the untouched file. The first patch
            // applies; the second is reported instead of silently erasing it.
            assert_eq!(failed, ["src/components/Button.tsx"], "failed patches");
            assert!(!applied.is_empty());
        }
        ApplyOutcome::RequiresApproval { message, patches } => {
            panic!("test fixture changes should be approved: {message}; {patches:?}")
        }
    }

    assert!(fixture.read("src/api/FOLDER.md").contains("auto-generated"));
    assert!(fixture.read("README.md").contains("[API](docs/api.md)"));
    assert!(
        fixture
            .read("src/components/Button.tsx")
            .contains("data-ui=")
    );
    assert_ne!(
        fixture.read("src/components/Notice.tsx"),
        include_str!("fixtures/quality_surface/src/components/Notice.tsx")
    );
    assert_eq!(
        fixture.read("src/api/routes.py"),
        "# formatted route fixture\n"
    );

    // A second pass finds nothing left to fix where the first one applied.
    assert!(collect(root, FixSelection::Docs).is_empty());
    assert!(
        collect(root, FixSelection::Links)
            .iter()
            .all(|proposal| !proposal.file_path.ends_with("README.md"))
    );
}
