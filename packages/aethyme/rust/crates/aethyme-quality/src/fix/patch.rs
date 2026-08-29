//! Patch generation and application (port of `src/autofixers/patch.py`).
//!
//! `FilePatch` holds a proposed whole-file rewrite and renders it as a
//! unified diff; `PatchGenerator` collects patches, runs them past the
//! safety engine, and executes the dry-run / apply flow including the
//! approval gate.

use std::path::{Path, PathBuf};

use super::difflib;
use super::pystr;
use super::safety::{RiskLevel, SafetyEngine, ValidationResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchMode {
    DryRun,
    Apply,
    Pr,
}

impl PatchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            PatchMode::DryRun => "dry_run",
            PatchMode::Apply => "apply",
            PatchMode::Pr => "pr",
        }
    }
}

/// Port of `FilePatch.get_summary()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchSummary {
    pub file: String,
    pub fix_type: String,
    pub risk_level: RiskLevel,
    pub lines_added: i64,
    pub size_change: i64,
    pub has_changes: bool,
}

/// A proposed rewrite of a single file.
#[derive(Debug, Clone)]
pub struct FilePatch {
    pub file_path: PathBuf,
    pub original_content: String,
    pub new_content: String,
    pub fix_type: String,
    pub risk_level: RiskLevel,
    pub validation: Option<ValidationResult>,
}

impl FilePatch {
    pub fn new(
        file_path: PathBuf,
        original_content: String,
        new_content: String,
        fix_type: String,
        risk_level: RiskLevel,
    ) -> Self {
        FilePatch {
            file_path,
            original_content,
            new_content,
            fix_type,
            risk_level,
            validation: None,
        }
    }

    /// Port of `generate_diff`: `difflib.unified_diff` over
    /// `splitlines(keepends=True)` with `lineterm=""`, joined with `""`.
    /// The empty line terminator means the `---`/`+++`/`@@` records
    /// carry no newline, so they run together in the joined result —
    /// see the note on `difflib::unified_diff`.
    pub fn generate_diff(&self) -> String {
        let original_lines = pystr::splitlines(&self.original_content, true);
        let new_lines = pystr::splitlines(&self.new_content, true);
        let name = pystr::as_posix(&self.file_path);
        difflib::unified_diff(
            &original_lines,
            &new_lines,
            &format!("a/{name}"),
            &format!("b/{name}"),
            3,
            "",
        )
        .join("")
    }

    pub fn get_summary(&self) -> PatchSummary {
        let orig_lines = pystr::splitlines(&self.original_content, false);
        let new_lines = pystr::splitlines(&self.new_content, false);
        PatchSummary {
            file: pystr::as_posix(&self.file_path),
            fix_type: self.fix_type.clone(),
            risk_level: self.risk_level,
            lines_added: new_lines.len() as i64 - orig_lines.len() as i64,
            size_change: pystr::char_len(&self.new_content) as i64
                - pystr::char_len(&self.original_content) as i64,
            has_changes: self.original_content != self.new_content,
        }
    }

    /// Port of `apply`: write `new_content` to `repo_path / file_path`
    /// (or to `file_path` itself when it is absolute, or when no repo
    /// root was supplied). Returns false on any write failure — the
    /// Python swallows the exception the same way, which is what makes
    /// a "partial" apply status reachable.
    ///
    /// No parent directory is created: the Python `open(..., "w")` does
    /// not, and every fixer proposes files in directories it already
    /// walked.
    pub fn apply(&self, repo_path: Option<&Path>) -> bool {
        let target_path = match repo_path {
            Some(root) if !self.file_path.is_absolute() => root.join(&self.file_path),
            _ => self.file_path.clone(),
        };
        std::fs::write(&target_path, &self.new_content).is_ok()
    }
}

/// Result of `PatchGenerator::apply`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Python `{"status": "requires_approval", ...}` — nothing written.
    RequiresApproval {
        message: String,
        patches: Vec<PatchSummary>,
    },
    /// Python `{"status": "success" | "partial", ...}`. Per-file
    /// failure tolerance is preserved: every patch is attempted, and a
    /// single failure downgrades the status to "partial" rather than
    /// aborting the run.
    Executed {
        status: &'static str,
        applied: Vec<String>,
        failed: Vec<String>,
    },
}

/// Port of `PatchGenerator.get_summary()`. `by_fix_type` keeps
/// first-seen insertion order (Python dict semantics) because it is
/// rendered verbatim to stdout and into the PR body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratorSummary {
    pub total_files: i64,
    pub total_low_risk: i64,
    pub total_medium_risk: i64,
    pub total_high_risk: i64,
    pub by_fix_type: Vec<(String, i64)>,
    pub requires_approval: i64,
}

pub struct PatchGenerator {
    pub repo_path: PathBuf,
    pub safety_engine: SafetyEngine,
    pub patches: Vec<FilePatch>,
}

impl PatchGenerator {
    pub fn new(repo_path: &Path, safety_engine: SafetyEngine) -> Self {
        PatchGenerator {
            repo_path: repo_path.to_path_buf(),
            safety_engine,
            patches: Vec::new(),
        }
    }

    /// Port of `add_patch`. Returns the index of the stored patch, or
    /// `None` where the Python returns `None`: a path outside the repo,
    /// a no-op change, or a file the safety engine refuses.
    ///
    /// The low→medium escalation on an unsafe validation is preserved.
    /// Combined with the doubling check tripping on empty originals,
    /// that is why every newly created file lands as medium risk and so
    /// requires approval before `--apply` writes it.
    pub fn add_patch(
        &mut self,
        file_path: &Path,
        original_content: &str,
        new_content: &str,
        fix_type: &str,
    ) -> Option<usize> {
        let mut file_path = file_path.to_path_buf();
        if file_path.is_absolute() {
            match relative_to(&file_path, &self.repo_path) {
                Some(rel) => file_path = rel,
                None => return None, // Python: logs "File outside repo".
            }
        }

        if original_content == new_content {
            return None;
        }

        let full_path = self.repo_path.join(&file_path);
        let mut risk_level = match self.safety_engine.assess_risk(&full_path, fix_type) {
            Ok(level) => level,
            Err(_) => return None, // Python: logs "Cannot patch file".
        };

        let validation = self
            .safety_engine
            .validate_changes(original_content, new_content);
        if !validation.safe && risk_level == RiskLevel::Low {
            risk_level = RiskLevel::Medium;
        }

        let mut patch = FilePatch::new(
            file_path,
            original_content.to_string(),
            new_content.to_string(),
            fix_type.to_string(),
            risk_level,
        );
        patch.validation = Some(validation);
        self.patches.push(patch);
        Some(self.patches.len() - 1)
    }

    /// Port of `generate_unified_diff`: per-patch diffs, empties
    /// dropped, joined with a single newline.
    pub fn generate_unified_diff(&self) -> String {
        self.patches
            .iter()
            .map(|patch| patch.generate_diff())
            .filter(|diff| !diff.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn get_summary(&self) -> GeneratorSummary {
        let mut low = 0i64;
        let mut medium = 0i64;
        let mut high = 0i64;
        let mut by_fix_type: Vec<(String, i64)> = Vec::new();
        for patch in &self.patches {
            match patch.risk_level {
                RiskLevel::Low => low += 1,
                RiskLevel::Medium => medium += 1,
                RiskLevel::High => high += 1,
            }
            match by_fix_type.iter_mut().find(|(k, _)| *k == patch.fix_type) {
                Some((_, count)) => *count += 1,
                None => by_fix_type.push((patch.fix_type.clone(), 1)),
            }
        }
        GeneratorSummary {
            total_files: self.patches.len() as i64,
            total_low_risk: low,
            total_medium_risk: medium,
            total_high_risk: high,
            by_fix_type,
            requires_approval: medium + high,
        }
    }

    /// Port of `dry_run` — the diff and per-patch summaries, nothing
    /// written.
    pub fn dry_run(&self) -> (GeneratorSummary, String, Vec<PatchSummary>) {
        (
            self.get_summary(),
            self.generate_unified_diff(),
            self.patches.iter().map(FilePatch::get_summary).collect(),
        )
    }

    /// Port of `apply`. The approval gate is evaluated BEFORE any file
    /// is touched: if any patch is medium or high risk and approval was
    /// not explicitly skipped, nothing at all is written.
    pub fn apply(&self, skip_approval: bool) -> ApplyOutcome {
        if !skip_approval {
            let requires_approval: Vec<&FilePatch> = self
                .patches
                .iter()
                .filter(|patch| matches!(patch.risk_level, RiskLevel::Medium | RiskLevel::High))
                .collect();
            if !requires_approval.is_empty() {
                return ApplyOutcome::RequiresApproval {
                    message: format!("{} patches require approval", requires_approval.len()),
                    patches: requires_approval
                        .into_iter()
                        .map(FilePatch::get_summary)
                        .collect(),
                };
            }
        }

        let mut applied: Vec<String> = Vec::new();
        let mut failed: Vec<String> = Vec::new();
        for patch in &self.patches {
            let name = pystr::as_posix(&patch.file_path);
            if patch.apply(Some(&self.repo_path)) {
                applied.push(name);
            } else {
                failed.push(name);
            }
        }

        ApplyOutcome::Executed {
            status: if failed.is_empty() {
                "success"
            } else {
                "partial"
            },
            applied,
            failed,
        }
    }

    pub fn create_commit_message(&self) -> String {
        let summary = self.get_summary();
        let mut lines = vec!["fix: apply autofixes".to_string(), String::new()];
        for (fix_type, count) in &summary.by_fix_type {
            lines.push(format!("- {fix_type}: {count} files"));
        }
        lines.push(String::new());
        lines.push(format!("Total files modified: {}", summary.total_files));
        lines.push(format!(
            "Risk levels: {} low, {} medium, {} high",
            summary.total_low_risk, summary.total_medium_risk, summary.total_high_risk
        ));
        lines.push(String::new());
        lines.push("Generated with Aethyme Autofixer".to_string());
        lines.join("\n")
    }

    pub fn save_patch_file(&self, output_path: &Path) -> std::io::Result<PathBuf> {
        std::fs::write(output_path, self.generate_unified_diff())?;
        Ok(output_path.to_path_buf())
    }

    pub fn get_changed_files(&self) -> Vec<PathBuf> {
        self.patches
            .iter()
            .map(|patch| self.repo_path.join(&patch.file_path))
            .collect()
    }
}

/// `Path.relative_to`: purely lexical, `None` where Python raises
/// `ValueError`. A path equal to the base yields `.` (Python's
/// `PurePath('.')`), not the empty path.
fn relative_to(path: &Path, base: &Path) -> Option<PathBuf> {
    let rel = path.strip_prefix(base).ok()?;
    if rel.as_os_str().is_empty() {
        Some(PathBuf::from("."))
    } else {
        Some(rel.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsupport::tmpdir;
    use std::fs;

    fn generator(root: &Path) -> PatchGenerator {
        PatchGenerator::new(root, SafetyEngine::new())
    }

    // ── FilePatch ────────────────────────────────────────────────────

    #[test]
    fn generates_diff() {
        let patch = FilePatch::new(
            PathBuf::from("test.py"),
            "line1\nline2\nline3".to_string(),
            "line1\nline2 modified\nline3".to_string(),
            "test_fix".to_string(),
            RiskLevel::Low,
        );
        let diff = patch.generate_diff();
        assert!(diff.contains("--- a/test.py"));
        assert!(diff.contains("+++ b/test.py"));
        assert!(diff.contains("-line2\n"));
        assert!(diff.contains("+line2 modified\n"));
        // The exact bytes, including the run-together headers.
        assert_eq!(
            diff,
            "--- a/test.py+++ b/test.py@@ -1,3 +1,3 @@ line1\n-line2\n+line2 modified\n line3"
        );
    }

    #[test]
    fn get_summary_reports_python_fields() {
        let patch = FilePatch::new(
            PathBuf::from("test.py"),
            "original".to_string(),
            "modified".to_string(),
            "format_fix".to_string(),
            RiskLevel::Medium,
        );
        let summary = patch.get_summary();
        assert_eq!(summary.file, "test.py");
        assert_eq!(summary.fix_type, "format_fix");
        assert_eq!(summary.risk_level.as_str(), "medium");
        assert!(summary.has_changes);
        assert_eq!(summary.lines_added, 0);
        assert_eq!(summary.size_change, 0);
    }

    #[test]
    fn apply_writes_absolute_paths_without_a_repo_root() {
        let tmp = tmpdir("patch-apply-abs");
        let file_path = tmp.join("test.txt");
        fs::write(&file_path, "original content").unwrap();
        let patch = FilePatch::new(
            file_path.clone(),
            "original content".to_string(),
            "new content".to_string(),
            "test_fix".to_string(),
            RiskLevel::Low,
        );
        assert!(patch.apply(None));
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "new content");
        // An absolute path ignores the repo root, as in the Python.
        assert!(patch.apply(Some(Path::new("/nonexistent"))));
    }

    #[test]
    fn apply_reports_failure_instead_of_panicking() {
        let patch = FilePatch::new(
            PathBuf::from("/nonexistent-dir-xyz/file.txt"),
            "a".to_string(),
            "b".to_string(),
            "test_fix".to_string(),
            RiskLevel::Low,
        );
        assert!(!patch.apply(None));
    }

    // ── PatchGenerator ───────────────────────────────────────────────

    #[test]
    fn add_patch_with_changes() {
        let tmp = tmpdir("patch-add");
        let mut pg = generator(&tmp);
        let file_path = tmp.join("test.py");
        fs::write(&file_path, "original").unwrap();
        assert!(
            pg.add_patch(&file_path, "original", "modified", "test_fix")
                .is_some()
        );
        assert_eq!(pg.patches.len(), 1);
        // Stored relative to the repo root.
        assert_eq!(pg.patches[0].file_path, PathBuf::from("test.py"));
    }

    #[test]
    fn skips_patch_without_changes() {
        let tmp = tmpdir("patch-nochange");
        let mut pg = generator(&tmp);
        let file_path = tmp.join("test.py");
        fs::write(&file_path, "content").unwrap();
        assert!(
            pg.add_patch(&file_path, "content", "content", "test_fix")
                .is_none()
        );
        assert!(pg.patches.is_empty());
    }

    #[test]
    fn skips_generated_files() {
        let tmp = tmpdir("patch-generated");
        let mut pg = generator(&tmp);
        let file_path = tmp.join("generated.gen.py");
        fs::write(&file_path, "original").unwrap();
        assert!(
            pg.add_patch(&file_path, "original", "modified", "test_fix")
                .is_none()
        );
        assert!(pg.patches.is_empty());
    }

    #[test]
    fn skips_files_outside_the_repo() {
        let tmp = tmpdir("patch-outside");
        let other = tmpdir("patch-outside-other");
        let mut pg = generator(&tmp);
        let file_path = other.join("test.py");
        fs::write(&file_path, "original").unwrap();
        assert!(
            pg.add_patch(&file_path, "original", "modified", "test_fix")
                .is_none()
        );
        assert!(pg.patches.is_empty());
    }

    #[test]
    fn unsafe_validation_escalates_low_to_medium_only() {
        let tmp = tmpdir("patch-escalate");
        let mut pg = generator(&tmp);
        // New file: "" -> content trips the doubling check, so a
        // docs_regen (nominally low) lands as medium.
        let doc = tmp.join("FOLDER.md");
        pg.add_patch(&doc, "", "# docs\n", "docs_regen").unwrap();
        assert_eq!(pg.patches[0].risk_level, RiskLevel::Medium);
        assert!(!pg.patches[0].validation.as_ref().unwrap().safe);

        // A high-risk file is NOT downgraded or re-escalated.
        let manifest = tmp.join("package.json");
        pg.add_patch(&manifest, "", "{}\n", "docs_regen").unwrap();
        assert_eq!(pg.patches[1].risk_level, RiskLevel::High);
    }

    #[test]
    fn safe_low_risk_change_stays_low() {
        let tmp = tmpdir("patch-low");
        let mut pg = generator(&tmp);
        let doc = tmp.join("README.md");
        pg.add_patch(&doc, "aaaa\n", "bbbb\n", "docs_regen")
            .unwrap();
        assert_eq!(pg.patches[0].risk_level, RiskLevel::Low);
    }

    #[test]
    fn generates_a_joined_unified_diff() {
        let tmp = tmpdir("patch-joined");
        let mut pg = generator(&tmp);
        pg.add_patch(
            &tmp.join("file1.py"),
            "content1\n",
            "modified1\n",
            "test_fix",
        );
        pg.add_patch(
            &tmp.join("file2.py"),
            "content2\n",
            "modified2\n",
            "test_fix",
        );
        let diff = pg.generate_unified_diff();
        assert_eq!(
            diff,
            "--- a/file1.py+++ b/file1.py@@ -1 +1 @@-content1\n+modified1\n\
             \n--- a/file2.py+++ b/file2.py@@ -1 +1 @@-content2\n+modified2\n"
        );
    }

    #[test]
    fn summary_counts_risks_and_preserves_fix_type_order() {
        let tmp = tmpdir("patch-summary");
        let mut pg = generator(&tmp);
        pg.add_patch(&tmp.join("README.md"), "aaaa", "bbbb", "docs_regen");
        pg.add_patch(&tmp.join("test_file.py"), "aaaa", "bbbb", "format_fix");
        pg.add_patch(&tmp.join("other.md"), "aaaa", "bbbb", "docs_regen");
        let summary = pg.get_summary();
        assert_eq!(summary.total_files, 3);
        assert_eq!(summary.total_low_risk, 2);
        assert_eq!(summary.total_medium_risk, 1);
        assert_eq!(summary.total_high_risk, 0);
        assert_eq!(
            summary.by_fix_type,
            vec![("docs_regen".to_string(), 2), ("format_fix".to_string(), 1)]
        );
        assert_eq!(summary.requires_approval, 1);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let tmp = tmpdir("patch-dryrun");
        let mut pg = generator(&tmp);
        let file_path = tmp.join("test.py");
        fs::write(&file_path, "original").unwrap();
        pg.add_patch(&file_path, "original", "modified", "test_fix");
        let (summary, diff, patches) = pg.dry_run();
        assert_eq!(summary.total_files, 1);
        assert!(!diff.is_empty());
        assert_eq!(patches.len(), 1);
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "original");
    }

    // ── The approval gate ────────────────────────────────────────────

    #[test]
    fn applies_low_risk_without_approval() {
        let tmp = tmpdir("patch-apply-low");
        let mut pg = generator(&tmp);
        let file_path = tmp.join("README.md");
        fs::write(&file_path, "original").unwrap();
        pg.add_patch(&file_path, "original", "modified", "docs_regen");
        match pg.apply(false) {
            ApplyOutcome::Executed {
                status,
                applied,
                failed,
            } => {
                assert_eq!(status, "success");
                assert_eq!(applied, vec!["README.md".to_string()]);
                assert!(failed.is_empty());
            }
            other => panic!("expected Executed, got {other:?}"),
        }
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "modified");
    }

    #[test]
    fn medium_risk_requires_approval_and_writes_nothing() {
        let tmp = tmpdir("patch-apply-medium");
        let mut pg = generator(&tmp);
        let file_path = tmp.join("routes.py");
        fs::write(&file_path, "original").unwrap();
        pg.add_patch(&file_path, "original", "modified", "format_fix");
        match pg.apply(false) {
            ApplyOutcome::RequiresApproval { message, patches } => {
                assert_eq!(message, "1 patches require approval");
                assert_eq!(patches.len(), 1);
                assert_eq!(patches[0].risk_level, RiskLevel::Medium);
            }
            other => panic!("expected RequiresApproval, got {other:?}"),
        }
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "original");
    }

    #[test]
    fn one_risky_patch_blocks_the_whole_batch() {
        // The gate is global, not per-file: a single medium-risk patch
        // stops the safe ones from being written too.
        let tmp = tmpdir("patch-apply-mixed");
        let mut pg = generator(&tmp);
        let safe_file = tmp.join("README.md");
        let risky_file = tmp.join("routes.py");
        fs::write(&safe_file, "original").unwrap();
        fs::write(&risky_file, "original").unwrap();
        pg.add_patch(&safe_file, "original", "modified", "docs_regen");
        pg.add_patch(&risky_file, "original", "modified", "format_fix");
        assert!(matches!(
            pg.apply(false),
            ApplyOutcome::RequiresApproval { .. }
        ));
        assert_eq!(fs::read_to_string(&safe_file).unwrap(), "original");
        assert_eq!(fs::read_to_string(&risky_file).unwrap(), "original");
    }

    #[test]
    fn skip_approval_applies_everything() {
        let tmp = tmpdir("patch-skip-approval");
        let mut pg = generator(&tmp);
        let file_path = tmp.join("routes.py");
        fs::write(&file_path, "original").unwrap();
        pg.add_patch(&file_path, "original", "modified", "format_fix");
        match pg.apply(true) {
            ApplyOutcome::Executed {
                status, applied, ..
            } => {
                assert_eq!(status, "success");
                assert_eq!(applied.len(), 1);
            }
            other => panic!("expected Executed, got {other:?}"),
        }
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "modified");
    }

    #[test]
    fn per_file_failure_yields_partial_not_abort() {
        // Python applies in list order with per-file tolerance; the
        // "improvement" of aborting on first failure would change
        // observable behavior, so partial is preserved.
        let tmp = tmpdir("patch-partial");
        let mut pg = generator(&tmp);
        let good = tmp.join("README.md");
        fs::write(&good, "original").unwrap();
        pg.add_patch(&good, "original", "modified", "docs_regen");
        // A path whose parent does not exist: write fails, the run
        // continues.
        pg.add_patch(
            &tmp.join("missing-dir/other.md"),
            "original",
            "modified",
            "docs_regen",
        );
        match pg.apply(true) {
            ApplyOutcome::Executed {
                status,
                applied,
                failed,
            } => {
                assert_eq!(status, "partial");
                assert_eq!(applied, vec!["README.md".to_string()]);
                assert_eq!(failed, vec!["missing-dir/other.md".to_string()]);
            }
            other => panic!("expected Executed, got {other:?}"),
        }
        assert_eq!(fs::read_to_string(&good).unwrap(), "modified");
    }

    #[test]
    fn empty_generator_applies_successfully() {
        let tmp = tmpdir("patch-empty");
        let pg = generator(&tmp);
        assert!(matches!(
            pg.apply(false),
            ApplyOutcome::Executed {
                status: "success",
                ..
            }
        ));
    }

    // ── Ancillary surfaces ───────────────────────────────────────────

    #[test]
    fn commit_message_matches_python_layout() {
        let tmp = tmpdir("patch-commitmsg");
        let mut pg = generator(&tmp);
        pg.add_patch(&tmp.join("file1.md"), "aaaa", "bbbb", "docs_regen");
        pg.add_patch(&tmp.join("file2.py"), "aaaa", "bbbb", "format_fix");
        assert_eq!(
            pg.create_commit_message(),
            "fix: apply autofixes\n\n\
             - docs_regen: 1 files\n\
             - format_fix: 1 files\n\n\
             Total files modified: 2\n\
             Risk levels: 2 low, 0 medium, 0 high\n\n\
             Generated with Aethyme Autofixer"
        );
    }

    #[test]
    fn save_patch_file_and_changed_files() {
        let tmp = tmpdir("patch-save");
        let mut pg = generator(&tmp);
        let file_path = tmp.join("test.py");
        fs::write(&file_path, "original").unwrap();
        pg.add_patch(&file_path, "original", "modified", "test_fix");
        let output = tmp.join("changes.patch");
        assert_eq!(pg.save_patch_file(&output).unwrap(), output);
        assert!(!fs::read_to_string(&output).unwrap().is_empty());
        assert_eq!(pg.get_changed_files(), vec![tmp.join("test.py")]);
    }

    #[test]
    fn relative_to_is_lexical() {
        assert_eq!(
            relative_to(Path::new("/a/b/c"), Path::new("/a/b")),
            Some(PathBuf::from("c"))
        );
        assert_eq!(
            relative_to(Path::new("/a/b"), Path::new("/a/b")),
            Some(PathBuf::from("."))
        );
        assert_eq!(relative_to(Path::new("/x/y"), Path::new("/a/b")), None);
    }
}
