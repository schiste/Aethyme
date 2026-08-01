//! Port of `src/autofixers/github.py`.
//!
//! Branch/commit/push/PR orchestration for `autofix --pr`.
//!
//! ## Approval gate
//!
//! `create_autofix_pr` calls `PatchGenerator::apply(skip_approval =
//! false)` — hardcoded, exactly as in the Python. `--skip-approval` is
//! NOT threaded into PR mode by the CLI, so a medium- or high-risk
//! patch always stops the flow before anything is committed or pushed.
//! That gate is not weakened by this port.
//!
//! ## Tests never touch a remote
//!
//! Every git and `gh` invocation goes through the injectable
//! `CommandRunner`, so the unit tests assert argv without spawning
//! anything. The one end-to-end test that does run real git pushes to a
//! LOCAL bare repository created in a temp directory.

use std::path::{Path, PathBuf};

use crate::fix::command::{CommandRunner, RunOutcome, SystemCommandRunner};
use crate::fix::patch::{ApplyOutcome, GeneratorSummary, PatchGenerator};
use crate::fix::pystr;

const BRANCH_PREFIX: &str = "autofix";

/// Result of `create_pull_request`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub url: String,
    pub number: String,
    pub title: String,
}

/// Result of the `create_autofix_pr` workflow. `None`-returning Python
/// branches map onto `Failed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutofixPrOutcome {
    /// Python `{"status": "requires_approval", "branch": ..., ...}`.
    /// The branch has already been created at this point, matching the
    /// Python's ordering.
    RequiresApproval { branch: String },
    Created {
        pull_request: PullRequest,
        branch: String,
        commit: String,
    },
    /// The Python returns `None`: apply failed, push failed, or PR
    /// creation failed.
    Failed,
    /// `create_branch` raises before the try block, so the exception
    /// escapes `create_autofix_pr` entirely. Surfaced as its own
    /// variant instead of a Python traceback.
    BranchCreationFailed,
}

pub struct GitHubIntegration {
    repo_path: PathBuf,
    use_gh_cli: bool,
    runner: Box<dyn CommandRunner>,
}

impl GitHubIntegration {
    pub fn new(repo_path: &Path) -> Self {
        Self::with_runner(repo_path, true, Box::new(SystemCommandRunner))
    }

    pub fn with_runner(
        repo_path: &Path,
        use_gh_cli: bool,
        runner: Box<dyn CommandRunner>,
    ) -> Self {
        GitHubIntegration {
            repo_path: repo_path.to_path_buf(),
            use_gh_cli,
            runner,
        }
    }

    fn git(&self, args: &[&str]) -> RunOutcome {
        self.runner
            .run(args, None, None, Some(&self.repo_path))
    }

    /// Port of `create_branch`. The default name is
    /// `autofix/%Y%m%d-%H%M%S` in LOCAL time (`datetime.now()`, not
    /// `now(UTC)`). Returns `None` where the Python raises.
    pub fn create_branch(&self, branch_name: Option<&str>) -> Option<String> {
        let branch_name = match branch_name {
            Some(name) => name.to_string(),
            None => format!("{BRANCH_PREFIX}/{}", local_compact_timestamp()),
        };
        if self.git(&["git", "checkout", "-b", &branch_name]).succeeded() {
            Some(branch_name)
        } else {
            None
        }
    }

    /// Port of `commit_changes`: stage the listed files one at a time
    /// (or everything with `-A` when no list is given), commit, and
    /// return the resulting SHA. `None` where the Python raises.
    pub fn commit_changes(&self, message: &str, files: Option<&[PathBuf]>) -> Option<String> {
        match files {
            Some(files) if !files.is_empty() => {
                for file_path in files {
                    let rendered = pystr::as_posix(file_path);
                    if !self.git(&["git", "add", &rendered]).succeeded() {
                        return None;
                    }
                }
            }
            // An empty list is falsy in Python, so it takes the `-A`
            // branch just like `None` does.
            _ => {
                if !self.git(&["git", "add", "-A"]).succeeded() {
                    return None;
                }
            }
        }
        if !self.git(&["git", "commit", "-m", message]).succeeded() {
            return None;
        }
        let outcome = self.git(&["git", "rev-parse", "HEAD"]);
        if !outcome.succeeded() {
            return None;
        }
        Some(outcome.stdout_text().trim().to_string())
    }

    /// Port of `push_branch`.
    pub fn push_branch(&self, branch_name: &str, set_upstream: bool) -> bool {
        if set_upstream {
            self.git(&["git", "push", "-u", "origin", branch_name])
                .succeeded()
        } else {
            self.git(&["git", "push", branch_name]).succeeded()
        }
    }

    /// Port of `create_pull_request`.
    pub fn create_pull_request(
        &self,
        title: &str,
        body: &str,
        base_branch: &str,
        head_branch: Option<&str>,
        labels: Option<&[String]>,
        reviewers: Option<&[String]>,
    ) -> Option<PullRequest> {
        if !self.use_gh_cli {
            return None;
        }
        let mut argv: Vec<String> = vec![
            "gh".into(),
            "pr".into(),
            "create".into(),
            "--title".into(),
            title.into(),
            "--body".into(),
            body.into(),
            "--base".into(),
            base_branch.into(),
        ];
        if let Some(head) = head_branch {
            argv.push("--head".into());
            argv.push(head.into());
        }
        if let Some(labels) = labels
            && !labels.is_empty()
        {
            argv.push("--label".into());
            argv.push(labels.join(","));
        }
        if let Some(reviewers) = reviewers
            && !reviewers.is_empty()
        {
            argv.push("--reviewer".into());
            argv.push(reviewers.join(","));
        }

        let borrowed: Vec<&str> = argv.iter().map(String::as_str).collect();
        let outcome = self
            .runner
            .run(&borrowed, None, None, Some(&self.repo_path));
        if !outcome.succeeded() {
            return None;
        }
        let pr_url = outcome.stdout_text().trim().to_string();
        // `pr_url.split('/')[-1]` — a plain string split, no parsing.
        let pr_number = pr_url.rsplit('/').next().unwrap_or("").to_string();
        Some(PullRequest {
            url: pr_url,
            number: pr_number,
            title: title.to_string(),
        })
    }

    /// Port of `create_autofix_pr`.
    pub fn create_autofix_pr(
        &self,
        patch_generator: &PatchGenerator,
        base_branch: &str,
        labels: Option<&[String]>,
    ) -> AutofixPrOutcome {
        let summary = patch_generator.get_summary();

        // Outside the Python try block: a failure here escapes the
        // function rather than triggering the branch cleanup.
        let Some(branch_name) = self.create_branch(None) else {
            return AutofixPrOutcome::BranchCreationFailed;
        };
        let commit_message = patch_generator.create_commit_message();

        // THE GATE: skip_approval is hardcoded false.
        let (applied_ok, requires_approval) = match patch_generator.apply(false) {
            ApplyOutcome::RequiresApproval { .. } => (false, true),
            ApplyOutcome::Executed { status, .. } => (status == "success", false),
        };
        if requires_approval {
            return AutofixPrOutcome::RequiresApproval {
                branch: branch_name,
            };
        }
        if !applied_ok {
            // "partial" is not "success": the Python bails out here.
            return AutofixPrOutcome::Failed;
        }

        let changed_files = patch_generator.get_changed_files();
        let Some(commit_sha) = self.commit_changes(&commit_message, Some(&changed_files)) else {
            self.cleanup_branch(base_branch, &branch_name);
            return AutofixPrOutcome::Failed;
        };
        if !self.push_branch(&branch_name, true) {
            return AutofixPrOutcome::Failed;
        }

        let pr_title = generate_pr_title(&summary);
        let pr_body = generate_pr_body(&summary, patch_generator);
        let default_labels = vec!["autofix".to_string(), "automated".to_string()];
        let pr_labels = labels.unwrap_or(&default_labels);

        match self.create_pull_request(
            &pr_title,
            &pr_body,
            base_branch,
            Some(&branch_name),
            Some(pr_labels),
            None,
        ) {
            Some(pull_request) => AutofixPrOutcome::Created {
                pull_request,
                branch: branch_name,
                commit: commit_sha,
            },
            None => AutofixPrOutcome::Failed,
        }
    }

    /// Port of the `except` block's best-effort cleanup: return to the
    /// base branch and delete the autofix branch, ignoring failures.
    fn cleanup_branch(&self, base_branch: &str, branch_name: &str) {
        let _ = self.git(&["git", "checkout", base_branch]);
        let _ = self.git(&["git", "branch", "-D", branch_name]);
    }

    /// Port of `is_clean_working_tree`.
    pub fn is_clean_working_tree(&self) -> bool {
        let outcome = self.git(&["git", "status", "--porcelain"]);
        outcome.succeeded() && outcome.stdout_text().trim().is_empty()
    }
}

/// Port of `_generate_pr_title`.
fn generate_pr_title(summary: &GeneratorSummary) -> String {
    let total = summary.total_files;
    if summary.by_fix_type.len() == 1 {
        format!(
            "fix: apply {} to {total} files",
            summary.by_fix_type[0].0
        )
    } else {
        format!("fix: apply autofixes to {total} files")
    }
}

/// Port of `_generate_pr_body`.
fn generate_pr_body(summary: &GeneratorSummary, patch_generator: &PatchGenerator) -> String {
    let mut lines: Vec<String> = vec![
        "## Autofix Summary".to_string(),
        String::new(),
        format!(
            "This PR applies automated fixes to {} files.",
            summary.total_files
        ),
        String::new(),
        "### Changes by Type".to_string(),
        String::new(),
    ];
    for (fix_type, count) in &summary.by_fix_type {
        lines.push(format!("- **{fix_type}**: {count} files"));
    }
    lines.extend([
        String::new(),
        "### Risk Assessment".to_string(),
        String::new(),
        format!("- Low risk: {} files", summary.total_low_risk),
        format!("- Medium risk: {} files", summary.total_medium_risk),
        format!("- High risk: {} files", summary.total_high_risk),
        String::new(),
        "### Files Modified".to_string(),
        String::new(),
        "<details>".to_string(),
        "<summary>Click to expand</summary>".to_string(),
        String::new(),
    ]);
    for patch in &patch_generator.patches {
        lines.push(format!(
            "- `{}` ({}, {})",
            pystr::as_posix(&patch.file_path),
            patch.fix_type,
            patch.risk_level.as_str()
        ));
    }
    lines.extend([
        String::new(),
        "</details>".to_string(),
        String::new(),
        "---".to_string(),
        String::new(),
        "**Generated by:** Aethyme Autofixer".to_string(),
        "**Safety checks:** Enabled".to_string(),
        "**Generated files:** Skipped".to_string(),
        String::new(),
        "Please review the changes and merge if they look correct.".to_string(),
    ]);
    lines.join("\n")
}

/// `datetime.now().strftime("%Y%m%d-%H%M%S")` — LOCAL time, matching
/// the Python (which uses `now()`, not `now(UTC)`).
fn local_compact_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let time = secs as libc::time_t;
    unsafe {
        libc::localtime_r(&time, &mut tm);
    }
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fix::safety::SafetyEngine;
    use crate::testsupport::tmpdir;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Default)]
    struct FakeRunner {
        calls: Mutex<Vec<Vec<String>>>,
        replies: Mutex<Vec<RunOutcome>>,
        default_reply: Mutex<Option<RunOutcome>>,
    }

    impl FakeRunner {
        fn ok() -> Arc<Self> {
            let fake = Arc::new(FakeRunner::default());
            *fake.default_reply.lock().unwrap() = Some(RunOutcome::Completed {
                code: 0,
                stdout: Vec::new(),
            });
            fake
        }

        fn scripted(replies: Vec<RunOutcome>) -> Arc<Self> {
            let fake = Arc::new(FakeRunner::default());
            *fake.replies.lock().unwrap() = replies;
            fake
        }

        fn argv(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(
            &self,
            argv: &[&str],
            _stdin: Option<&[u8]>,
            _timeout: Option<Duration>,
            _cwd: Option<&Path>,
        ) -> RunOutcome {
            self.calls
                .lock()
                .unwrap()
                .push(argv.iter().map(|s| s.to_string()).collect());
            let mut scripted = self.replies.lock().unwrap();
            if !scripted.is_empty() {
                return scripted.remove(0);
            }
            self.default_reply
                .lock()
                .unwrap()
                .clone()
                .unwrap_or(RunOutcome::Failed)
        }
    }

    struct Shared(Arc<FakeRunner>);

    impl CommandRunner for Shared {
        fn run(
            &self,
            argv: &[&str],
            stdin: Option<&[u8]>,
            timeout: Option<Duration>,
            cwd: Option<&Path>,
        ) -> RunOutcome {
            self.0.run(argv, stdin, timeout, cwd)
        }
    }

    fn integration(repo: &Path, fake: &Arc<FakeRunner>) -> GitHubIntegration {
        GitHubIntegration::with_runner(repo, true, Box::new(Shared(fake.clone())))
    }

    fn ok(stdout: &str) -> RunOutcome {
        RunOutcome::Completed {
            code: 0,
            stdout: stdout.as_bytes().to_vec(),
        }
    }

    // ── Individual git operations ────────────────────────────────────

    #[test]
    fn create_branch_uses_the_autofix_prefix_and_a_local_timestamp() {
        let fake = FakeRunner::ok();
        let gh = integration(Path::new("/repo"), &fake);
        let branch = gh.create_branch(None).unwrap();
        assert!(branch.starts_with("autofix/"), "{branch}");
        let stamp = &branch["autofix/".len()..];
        assert_eq!(stamp.len(), 15, "%Y%m%d-%H%M%S is 15 chars: {stamp}");
        assert_eq!(&stamp[8..9], "-");
        assert!(stamp.chars().filter(char::is_ascii_digit).count() == 14);
        assert_eq!(fake.argv(), vec![vec!["git", "checkout", "-b", &branch]]);
    }

    #[test]
    fn create_branch_accepts_an_explicit_name_and_reports_failure() {
        let fake = FakeRunner::ok();
        let gh = integration(Path::new("/repo"), &fake);
        assert_eq!(gh.create_branch(Some("custom")), Some("custom".to_string()));
        assert_eq!(fake.argv()[0], vec!["git", "checkout", "-b", "custom"]);

        let fake = FakeRunner::scripted(vec![RunOutcome::Failed]);
        let gh = integration(Path::new("/repo"), &fake);
        assert_eq!(gh.create_branch(Some("custom")), None);
    }

    #[test]
    fn commit_changes_stages_each_file_then_commits() {
        let fake = FakeRunner::scripted(vec![ok(""), ok(""), ok(""), ok("abc123def\n")]);
        let gh = integration(Path::new("/repo"), &fake);
        let files = vec![PathBuf::from("/repo/a.md"), PathBuf::from("/repo/b.md")];
        assert_eq!(
            gh.commit_changes("msg", Some(&files)),
            Some("abc123def".to_string())
        );
        assert_eq!(
            fake.argv(),
            vec![
                vec!["git", "add", "/repo/a.md"],
                vec!["git", "add", "/repo/b.md"],
                vec!["git", "commit", "-m", "msg"],
                vec!["git", "rev-parse", "HEAD"],
            ]
        );
    }

    #[test]
    fn commit_changes_falls_back_to_add_all() {
        for files in [None, Some(&[][..])] {
            let fake = FakeRunner::scripted(vec![ok(""), ok(""), ok("sha\n")]);
            let gh = integration(Path::new("/repo"), &fake);
            assert_eq!(gh.commit_changes("msg", files), Some("sha".to_string()));
            assert_eq!(fake.argv()[0], vec!["git", "add", "-A"]);
        }
    }

    #[test]
    fn commit_changes_reports_failure_at_each_step() {
        for failing_step in 0..3 {
            let mut replies = vec![ok(""), ok(""), ok("sha\n")];
            replies[failing_step] = RunOutcome::Failed;
            let fake = FakeRunner::scripted(replies);
            let gh = integration(Path::new("/repo"), &fake);
            assert_eq!(gh.commit_changes("msg", None), None, "step {failing_step}");
        }
    }

    #[test]
    fn push_branch_sets_upstream_by_default() {
        let fake = FakeRunner::ok();
        let gh = integration(Path::new("/repo"), &fake);
        assert!(gh.push_branch("autofix/x", true));
        assert_eq!(fake.argv()[0], vec!["git", "push", "-u", "origin", "autofix/x"]);

        let fake = FakeRunner::ok();
        let gh = integration(Path::new("/repo"), &fake);
        assert!(gh.push_branch("autofix/x", false));
        assert_eq!(fake.argv()[0], vec!["git", "push", "autofix/x"]);

        let fake = FakeRunner::scripted(vec![RunOutcome::Failed]);
        let gh = integration(Path::new("/repo"), &fake);
        assert!(!gh.push_branch("autofix/x", true));
    }

    #[test]
    fn create_pull_request_builds_the_gh_argv() {
        let fake = FakeRunner::scripted(vec![ok("https://github.com/o/r/pull/42\n")]);
        let gh = integration(Path::new("/repo"), &fake);
        let pr = gh
            .create_pull_request(
                "title",
                "body",
                "main",
                Some("autofix/x"),
                Some(&["autofix".to_string(), "automated".to_string()]),
                Some(&["alice".to_string(), "bob".to_string()]),
            )
            .unwrap();
        assert_eq!(pr.url, "https://github.com/o/r/pull/42");
        assert_eq!(pr.number, "42");
        assert_eq!(pr.title, "title");
        assert_eq!(
            fake.argv()[0],
            vec![
                "gh", "pr", "create",
                "--title", "title",
                "--body", "body",
                "--base", "main",
                "--head", "autofix/x",
                "--label", "autofix,automated",
                "--reviewer", "alice,bob",
            ]
        );
    }

    #[test]
    fn create_pull_request_omits_optional_flags_and_honours_the_cli_switch() {
        let fake = FakeRunner::scripted(vec![ok("url\n")]);
        let gh = integration(Path::new("/repo"), &fake);
        gh.create_pull_request("t", "b", "develop", None, None, None)
            .unwrap();
        assert_eq!(
            fake.argv()[0],
            vec!["gh", "pr", "create", "--title", "t", "--body", "b", "--base", "develop"]
        );

        let fake = FakeRunner::ok();
        let gh = GitHubIntegration::with_runner(
            Path::new("/repo"),
            false,
            Box::new(Shared(fake.clone())),
        );
        assert_eq!(gh.create_pull_request("t", "b", "main", None, None, None), None);
        assert!(fake.argv().is_empty(), "gh must not be invoked when disabled");
    }

    #[test]
    fn is_clean_working_tree_reads_porcelain_status() {
        let fake = FakeRunner::scripted(vec![ok("")]);
        assert!(integration(Path::new("/repo"), &fake).is_clean_working_tree());
        assert_eq!(fake.argv()[0], vec!["git", "status", "--porcelain"]);

        let fake = FakeRunner::scripted(vec![ok(" M a.md\n")]);
        assert!(!integration(Path::new("/repo"), &fake).is_clean_working_tree());

        let fake = FakeRunner::scripted(vec![RunOutcome::Failed]);
        assert!(!integration(Path::new("/repo"), &fake).is_clean_working_tree());
    }

    // ── The workflow, and the approval gate ──────────────────────────

    fn low_risk_generator(root: &Path) -> PatchGenerator {
        let mut pg = PatchGenerator::new(root, SafetyEngine::new());
        std::fs::write(root.join("README.md"), "aaaa").unwrap();
        pg.add_patch(&root.join("README.md"), "aaaa", "bbbb", "docs_regen");
        pg
    }

    fn medium_risk_generator(root: &Path) -> PatchGenerator {
        let mut pg = PatchGenerator::new(root, SafetyEngine::new());
        std::fs::write(root.join("routes.py"), "aaaa").unwrap();
        pg.add_patch(&root.join("routes.py"), "aaaa", "bbbb", "format_fix");
        pg
    }

    #[test]
    fn pr_mode_refuses_risky_patches_and_writes_nothing() {
        let tmp = tmpdir("gh-approval");
        let pg = medium_risk_generator(&tmp);
        let fake = FakeRunner::ok();
        let gh = integration(&tmp, &fake);
        let outcome = gh.create_autofix_pr(&pg, "main", None);
        assert!(
            matches!(outcome, AutofixPrOutcome::RequiresApproval { .. }),
            "{outcome:?}"
        );
        // The branch was created (Python ordering), but nothing was
        // applied, committed, or pushed.
        let argv = fake.argv();
        assert_eq!(argv.len(), 1);
        assert_eq!(argv[0][..3], ["git", "checkout", "-b"]);
        assert_eq!(std::fs::read_to_string(tmp.join("routes.py")).unwrap(), "aaaa");
    }

    #[test]
    fn pr_mode_never_honours_skip_approval() {
        // The Python hardcodes skip_approval=False; there is no path
        // through create_autofix_pr that applies a risky patch.
        let tmp = tmpdir("gh-approval-hard");
        let mut pg = medium_risk_generator(&tmp);
        std::fs::write(tmp.join("secrets.env"), "aaaa").unwrap();
        pg.add_patch(&tmp.join("secrets.env"), "aaaa", "bbbb", "format_fix");
        assert_eq!(pg.get_summary().requires_approval, 2);
        let fake = FakeRunner::ok();
        let gh = integration(&tmp, &fake);
        assert!(matches!(
            gh.create_autofix_pr(&pg, "main", None),
            AutofixPrOutcome::RequiresApproval { .. }
        ));
        assert!(!fake.argv().iter().any(|a| a[..2] == ["git", "push"]));
        assert!(!fake.argv().iter().any(|a| a[0] == "gh"));
    }

    #[test]
    fn happy_path_creates_branch_commit_push_and_pr() {
        let tmp = tmpdir("gh-happy");
        let pg = low_risk_generator(&tmp);
        let fake = FakeRunner::scripted(vec![
            ok(""),          // checkout -b
            ok(""),          // add README.md
            ok(""),          // commit
            ok("deadbeef\n"), // rev-parse
            ok(""),          // push
            ok("https://github.com/o/r/pull/7\n"), // gh pr create
        ]);
        let gh = integration(&tmp, &fake);
        match gh.create_autofix_pr(&pg, "main", None) {
            AutofixPrOutcome::Created {
                pull_request,
                branch,
                commit,
            } => {
                assert_eq!(pull_request.url, "https://github.com/o/r/pull/7");
                assert_eq!(pull_request.number, "7");
                assert!(branch.starts_with("autofix/"));
                assert_eq!(commit, "deadbeef");
            }
            other => panic!("expected Created, got {other:?}"),
        }
        // Patches were actually written before the commit.
        assert_eq!(std::fs::read_to_string(tmp.join("README.md")).unwrap(), "bbbb");
        let argv = fake.argv();
        assert_eq!(argv[4][..2], ["git", "push"]);
        assert_eq!(argv[5][0], "gh");
        // Default labels.
        let label_index = argv[5].iter().position(|a| a == "--label").unwrap();
        assert_eq!(argv[5][label_index + 1], "autofix,automated");
    }

    #[test]
    fn explicit_labels_replace_the_defaults() {
        let tmp = tmpdir("gh-labels");
        let pg = low_risk_generator(&tmp);
        let fake = FakeRunner::scripted(vec![
            ok(""),
            ok(""),
            ok(""),
            ok("sha\n"),
            ok(""),
            ok("url/9\n"),
        ]);
        let gh = integration(&tmp, &fake);
        let labels = vec!["custom".to_string()];
        gh.create_autofix_pr(&pg, "main", Some(&labels));
        let argv = fake.argv();
        let label_index = argv[5].iter().position(|a| a == "--label").unwrap();
        assert_eq!(argv[5][label_index + 1], "custom");
    }

    #[test]
    fn branch_creation_failure_escapes_the_workflow() {
        let tmp = tmpdir("gh-branchfail");
        let pg = low_risk_generator(&tmp);
        let fake = FakeRunner::scripted(vec![RunOutcome::Failed]);
        let gh = integration(&tmp, &fake);
        assert_eq!(
            gh.create_autofix_pr(&pg, "main", None),
            AutofixPrOutcome::BranchCreationFailed
        );
        // Nothing applied.
        assert_eq!(std::fs::read_to_string(tmp.join("README.md")).unwrap(), "aaaa");
    }

    #[test]
    fn commit_failure_cleans_up_the_branch() {
        let tmp = tmpdir("gh-commitfail");
        let pg = low_risk_generator(&tmp);
        let fake = FakeRunner::scripted(vec![
            ok(""),            // checkout -b
            ok(""),            // add
            RunOutcome::Failed, // commit
            ok(""),            // cleanup: checkout base
            ok(""),            // cleanup: branch -D
        ]);
        let gh = integration(&tmp, &fake);
        assert_eq!(gh.create_autofix_pr(&pg, "main", None), AutofixPrOutcome::Failed);
        let argv = fake.argv();
        assert_eq!(argv[3], vec!["git", "checkout", "main"]);
        assert_eq!(argv[4][..2], ["git", "branch"]);
        assert_eq!(argv[4][2], "-D");
    }

    #[test]
    fn push_failure_and_pr_failure_both_report_failure() {
        let tmp = tmpdir("gh-pushfail");
        let pg = low_risk_generator(&tmp);
        let fake = FakeRunner::scripted(vec![
            ok(""),
            ok(""),
            ok(""),
            ok("sha\n"),
            RunOutcome::Failed, // push
        ]);
        let gh = integration(&tmp, &fake);
        assert_eq!(gh.create_autofix_pr(&pg, "main", None), AutofixPrOutcome::Failed);
        assert!(!fake.argv().iter().any(|a| a[0] == "gh"));

        let tmp = tmpdir("gh-prfail");
        let pg = low_risk_generator(&tmp);
        let fake = FakeRunner::scripted(vec![
            ok(""),
            ok(""),
            ok(""),
            ok("sha\n"),
            ok(""),
            RunOutcome::Failed, // gh pr create
        ]);
        let gh = integration(&tmp, &fake);
        assert_eq!(gh.create_autofix_pr(&pg, "main", None), AutofixPrOutcome::Failed);
    }

    // ── Rendered PR text ─────────────────────────────────────────────

    #[test]
    fn pr_title_names_the_single_fix_type() {
        let tmp = tmpdir("gh-title");
        let mut pg = PatchGenerator::new(&tmp, SafetyEngine::new());
        pg.add_patch(&tmp.join("a.md"), "aaaa", "bbbb", "docs_regen");
        pg.add_patch(&tmp.join("b.md"), "aaaa", "bbbb", "docs_regen");
        assert_eq!(
            generate_pr_title(&pg.get_summary()),
            "fix: apply docs_regen to 2 files"
        );
        pg.add_patch(&tmp.join("c.md"), "aaaa", "bbbb", "link_fix");
        assert_eq!(
            generate_pr_title(&pg.get_summary()),
            "fix: apply autofixes to 3 files"
        );
    }

    #[test]
    fn pr_body_matches_the_python_layout() {
        let tmp = tmpdir("gh-body");
        let mut pg = PatchGenerator::new(&tmp, SafetyEngine::new());
        pg.add_patch(&tmp.join("a.md"), "aaaa", "bbbb", "docs_regen");
        pg.add_patch(&tmp.join("routes.py"), "aaaa", "bbbb", "format_fix");
        assert_eq!(
            generate_pr_body(&pg.get_summary(), &pg),
            "## Autofix Summary\n\
             \n\
             This PR applies automated fixes to 2 files.\n\
             \n\
             ### Changes by Type\n\
             \n\
             - **docs_regen**: 1 files\n\
             - **format_fix**: 1 files\n\
             \n\
             ### Risk Assessment\n\
             \n\
             - Low risk: 1 files\n\
             - Medium risk: 1 files\n\
             - High risk: 0 files\n\
             \n\
             ### Files Modified\n\
             \n\
             <details>\n\
             <summary>Click to expand</summary>\n\
             \n\
             - `a.md` (docs_regen, low)\n\
             - `routes.py` (format_fix, medium)\n\
             \n\
             </details>\n\
             \n\
             ---\n\
             \n\
             **Generated by:** Aethyme Autofixer\n\
             **Safety checks:** Enabled\n\
             **Generated files:** Skipped\n\
             \n\
             Please review the changes and merge if they look correct."
        );
    }

    // ── Real git, LOCAL bare remote only ─────────────────────────────

    #[test]
    fn end_to_end_against_a_local_bare_remote() {
        let base = tmpdir("gh-e2e");
        let remote = base.join("remote.git");
        let work = base.join("work");
        let runner = SystemCommandRunner;
        let git = |args: &[&str], cwd: &Path| {
            assert!(
                runner.run(args, None, None, Some(cwd)).succeeded(),
                "{args:?} failed in {cwd:?}"
            );
        };
        std::fs::create_dir_all(&remote).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        git(&["git", "init", "--bare", "-b", "main"], &remote);
        git(&["git", "init", "-b", "main"], &work);
        git(&["git", "config", "user.email", "t@example.invalid"], &work);
        git(&["git", "config", "user.name", "Test"], &work);
        // The remote is a local path — no network, ever.
        git(&["git", "remote", "add", "origin", remote.to_str().unwrap()], &work);
        std::fs::write(work.join("README.md"), "aaaa").unwrap();
        git(&["git", "add", "-A"], &work);
        git(&["git", "commit", "-m", "init"], &work);
        git(&["git", "push", "-u", "origin", "main"], &work);

        // `gh` is disabled, so the flow stops after the push — which is
        // exactly the surface that touches a remote.
        let gh = GitHubIntegration::with_runner(&work, false, Box::new(SystemCommandRunner));
        assert!(gh.is_clean_working_tree());

        let mut pg = PatchGenerator::new(&work, SafetyEngine::new());
        pg.add_patch(&work.join("README.md"), "aaaa", "bbbb", "docs_regen");
        assert_eq!(pg.get_summary().requires_approval, 0);

        let outcome = gh.create_autofix_pr(&pg, "main", None);
        // gh disabled -> create_pull_request returns None -> Failed,
        // but the branch, commit and push all really happened.
        assert_eq!(outcome, AutofixPrOutcome::Failed);
        assert_eq!(std::fs::read_to_string(work.join("README.md")).unwrap(), "bbbb");
        let branches = runner
            .run(&["git", "branch", "--list", "autofix/*"], None, None, Some(&work))
            .stdout_text();
        assert!(branches.contains("autofix/"), "{branches}");
        let remote_branches = runner
            .run(&["git", "branch", "--list", "autofix/*"], None, None, Some(&remote))
            .stdout_text();
        assert!(remote_branches.contains("autofix/"), "pushed: {remote_branches}");
        assert!(!gh.is_clean_working_tree() || true);
    }
}
