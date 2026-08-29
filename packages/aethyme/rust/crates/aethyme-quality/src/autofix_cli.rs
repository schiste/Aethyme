//! Native `aethyme autofix` front end (retirement Phase 5 flip).
//!
//! Stdout is byte-compatible with the deleted Click command (`autofix`
//! in `src/cli.py` before the flip). Divergences follow the Phase 2/3/4
//! precedent and are recorded in the `cli-surface-v1.md` header:
//! - usage errors keep Click's `Error: {message}` line (exact Click
//!   8.4 wording) but drop the preceding usage/`Try ... --help` block;
//! - `--help` prints the Click help body with the router spelling
//!   (`Usage: aethyme autofix [OPTIONS] REPO_PATH`);
//! - a failed `git checkout -b` in PR mode reports
//!   `Error: failed to create the autofix branch` on stderr and exits 1
//!   instead of the Python traceback (same exit code — `create_branch`
//!   is outside the Python try block, so its exception escaped the
//!   command).

use std::io::{BufRead, Write};
use std::path::PathBuf;

use crate::fix::FixSelection;
use crate::fix::github::{AutofixPrOutcome, GitHubIntegration};
use crate::fix::patch::{ApplyOutcome, PatchGenerator};
use crate::fix::safety::SafetyEngine;

const HELP: &str = "Usage: aethyme autofix [OPTIONS] REPO_PATH

  Apply automated fixes to codebase issues.

Options:
  --dry-run                       Show changes without applying
  --apply                         Apply changes to disk
  --pr                            Create pull request
  --fix-type [all|docs|links|selectors|i18n|format]
                                  Type of fix to apply
  --skip-approval                 Skip approval for risky changes
  --help                          Show this message and exit.
";

const RULE: &str = "============================================================";

#[derive(Debug)]
struct Args {
    repo_path: String,
    dry_run: bool,
    do_apply: bool,
    pr: bool,
    fix_type: String,
    skip_approval: bool,
}

/// Run `autofix` with `args` excluding the leading command name.
pub fn run(args: &[String]) -> u8 {
    let parsed = match parse_args(args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => {
            print!("{HELP}");
            return 0;
        }
        Err(message) => {
            eprintln!("Error: {message}");
            return 2;
        }
    };

    // click.Path(exists=True) validates before the command body runs.
    let given = PathBuf::from(&parsed.repo_path);
    if !given.exists() {
        eprintln!(
            "Error: Invalid value for 'REPO_PATH': Path '{}' does not exist.",
            parsed.repo_path
        );
        return 2;
    }
    let repository_path = std::fs::canonicalize(&given).unwrap_or(given);

    println!();
    println!("Aethyme Autofixer");
    println!("{RULE}");
    println!("Repository: {}", repository_path.display());
    println!("Fix type: {}", parsed.fix_type);
    println!(
        "Mode: {}",
        if parsed.dry_run {
            "DRY RUN"
        } else if parsed.do_apply {
            "APPLY"
        } else if parsed.pr {
            "PR"
        } else {
            "DRY RUN"
        }
    );
    println!();

    let selection = FixSelection::parse(&parsed.fix_type).expect("validated during parsing");
    let mut patch_gen = PatchGenerator::new(&repository_path, SafetyEngine::new());

    // Scan order is the Python's, and it is patch order, hence diff
    // order.
    for (group, scanning, noun) in [
        (FixSelection::Docs, "documentation issues", "documentation"),
        (FixSelection::Links, "link issues", "link"),
        (
            FixSelection::Selectors,
            "missing test selectors",
            "selector",
        ),
        (FixSelection::I18n, "hardcoded strings", "i18n"),
        (FixSelection::Format, "formatting issues", "formatting"),
    ] {
        let Some(proposals) = crate::fix::collect_group(&repository_path, selection, group) else {
            continue;
        };
        println!("Scanning for {scanning}...");
        println!("  Found {} {noun} fixes", proposals.len());
        for proposal in proposals {
            patch_gen.add_patch(
                &proposal.file_path,
                &proposal.original_content,
                &proposal.new_content,
                &proposal.fix_type,
            );
        }
    }

    println!();
    println!("Generating patches...");

    let summary = patch_gen.get_summary();

    println!();
    println!("{RULE}");
    println!("Summary");
    println!("{RULE}");
    println!("Total files: {}", summary.total_files);
    println!("Low risk: {}", summary.total_low_risk);
    println!("Medium risk: {}", summary.total_medium_risk);
    println!("High risk: {}", summary.total_high_risk);
    println!();

    for (fix_type, count) in &summary.by_fix_type {
        println!("  {fix_type}: {count} files");
    }

    if summary.total_files == 0 {
        println!();
        println!("No fixes needed!");
        return 0;
    }

    if parsed.dry_run || (!parsed.do_apply && !parsed.pr) {
        let (_, diff, _) = patch_gen.dry_run();
        println!();
        println!("{RULE}");
        println!("Changes Preview (Dry Run)");
        println!("{RULE}");
        if diff.is_empty() {
            println!("No changes to show");
        } else {
            println!("{diff}");
        }
        println!();
        println!("Run with --apply to apply these changes");
        println!("Run with --pr to create a pull request");
        return 0;
    }

    if parsed.do_apply {
        return run_apply(&patch_gen, &summary, parsed.skip_approval);
    }

    run_pr(&repository_path, &patch_gen)
}

fn run_apply(
    patch_gen: &PatchGenerator,
    summary: &crate::fix::patch::GeneratorSummary,
    skip_approval: bool,
) -> u8 {
    if summary.requires_approval > 0 && !skip_approval {
        println!();
        println!(
            "Warning: {} files require approval",
            summary.requires_approval
        );
        match confirm("Continue anyway?") {
            Confirmation::Yes => {}
            Confirmation::No => return 0,
            // Click 8.4's Abort handler prints only "Aborted!" to
            // stderr (no leading blank line) and exits 1.
            Confirmation::Aborted => {
                eprintln!("Aborted!");
                return 1;
            }
        }
    }

    println!();
    println!("Applying fixes...");
    match patch_gen.apply(skip_approval) {
        ApplyOutcome::RequiresApproval { patches, .. } => {
            println!();
            println!("Some fixes require approval:");
            for patch in &patches {
                println!("  {} ({})", patch.file, patch.risk_level.as_str());
            }
            println!();
            println!("Use --skip-approval to apply anyway (not recommended)");
            0
        }
        ApplyOutcome::Executed {
            applied, failed, ..
        } => {
            println!();
            println!("Applied {} files", applied.len());
            if !failed.is_empty() {
                println!("Failed {} files:", failed.len());
                for file in &failed {
                    println!("  {file}");
                }
            }
            0
        }
    }
}

fn run_pr(repository_path: &std::path::Path, patch_gen: &PatchGenerator) -> u8 {
    println!();
    println!("Creating pull request...");

    let integration = GitHubIntegration::new(repository_path);
    if !integration.is_clean_working_tree() {
        println!("Error: Working tree has uncommitted changes");
        println!("Please commit or stash changes before creating PR");
        return 0;
    }

    match integration.create_autofix_pr(patch_gen, "main", None) {
        AutofixPrOutcome::Created {
            pull_request,
            branch,
            commit,
        } => {
            println!();
            println!("Pull request created: {}", pull_request.url);
            println!("Branch: {branch}");
            // Python `commit[:8]` — a plain slice, short SHAs included.
            println!("Commit: {}", commit.chars().take(8).collect::<String>());
            0
        }
        AutofixPrOutcome::RequiresApproval { .. } => {
            println!();
            println!("Some fixes require approval before creating PR");
            println!("Use the web UI to approve, or use --apply --skip-approval");
            0
        }
        AutofixPrOutcome::Failed => {
            println!();
            println!("Failed to create pull request");
            0
        }
        // See the module docs: the Python raises out of the command
        // here, dying with a traceback and exit 1.
        AutofixPrOutcome::BranchCreationFailed => {
            eprintln!("Error: failed to create the autofix branch");
            1
        }
    }
}

enum Confirmation {
    Yes,
    No,
    Aborted,
}

/// `click.confirm(text)` with `default=False`: prompt on stdout without
/// a newline, accept y/yes/n/no case-insensitively, treat empty input
/// as the default, re-prompt on anything else, and abort on EOF.
fn confirm(text: &str) -> Confirmation {
    let stdin = std::io::stdin();
    let mut handle = stdin.lock();
    loop {
        print!("{text} [y/N]: ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        match handle.read_line(&mut line) {
            Ok(0) | Err(_) => return Confirmation::Aborted,
            Ok(_) => {}
        }
        match line.trim_end_matches(['\n', '\r']).to_lowercase().trim() {
            "y" | "yes" => return Confirmation::Yes,
            "n" | "no" => return Confirmation::No,
            "" => return Confirmation::No, // default=False
            _ => println!("Error: invalid input"),
        }
    }
}

fn parse_args(args: &[String]) -> Result<Option<Args>, String> {
    let mut repo_path: Option<String> = None;
    let mut dry_run = false;
    let mut do_apply = false;
    let mut pr = false;
    let mut fix_type = "all".to_string();
    let mut skip_approval = false;

    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--help" => return Ok(None),
            "--dry-run" => dry_run = true,
            "--apply" => do_apply = true,
            "--pr" => pr = true,
            "--skip-approval" => skip_approval = true,
            "--fix-type" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("Option '--fix-type' requires an argument.".to_string());
                };
                fix_type = value.clone();
                i += 1;
            }
            other => {
                if let Some(value) = other.strip_prefix("--fix-type=") {
                    fix_type = value.to_string();
                } else if other.starts_with('-') && other.len() > 1 {
                    return Err(format!("No such option '{other}'."));
                } else if repo_path.is_some() {
                    return Err(format!("Got unexpected extra argument ({other})"));
                } else {
                    repo_path = Some(other.to_string());
                }
            }
        }
        i += 1;
    }

    if FixSelection::parse(&fix_type).is_none() {
        return Err(format!(
            "Invalid value for '--fix-type': '{fix_type}' is not one of 'all', 'docs', 'links', 'selectors', 'i18n', 'format'."
        ));
    }
    let Some(repo_path) = repo_path else {
        return Err("Missing argument 'REPO_PATH'.".to_string());
    };

    Ok(Some(Args {
        repo_path,
        dry_run,
        do_apply,
        pr,
        fix_type,
        skip_approval,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Args>, String> {
        parse_args(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn defaults_match_the_click_command() {
        let parsed = parse(&["/repo"]).unwrap().unwrap();
        assert_eq!(parsed.repo_path, "/repo");
        assert_eq!(parsed.fix_type, "all");
        assert!(!parsed.dry_run && !parsed.do_apply && !parsed.pr && !parsed.skip_approval);
    }

    #[test]
    fn flags_and_the_argument_may_be_interleaved() {
        let parsed = parse(&[
            "--dry-run",
            "/repo",
            "--fix-type",
            "docs",
            "--skip-approval",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(parsed.repo_path, "/repo");
        assert_eq!(parsed.fix_type, "docs");
        assert!(parsed.dry_run && parsed.skip_approval);
        let parsed = parse(&["/repo", "--fix-type=links", "--apply", "--pr"])
            .unwrap()
            .unwrap();
        assert_eq!(parsed.fix_type, "links");
        assert!(parsed.do_apply && parsed.pr);
    }

    #[test]
    fn help_short_circuits() {
        assert!(parse(&["--help"]).unwrap().is_none());
        assert!(parse(&["/repo", "--help"]).unwrap().is_none());
    }

    #[test]
    fn usage_errors_use_click_wording() {
        assert_eq!(parse(&[]).unwrap_err(), "Missing argument 'REPO_PATH'.");
        assert_eq!(
            parse(&["/repo", "--bogus"]).unwrap_err(),
            "No such option '--bogus'."
        );
        assert_eq!(
            parse(&["/repo", "--fix-type", "bogus"]).unwrap_err(),
            "Invalid value for '--fix-type': 'bogus' is not one of 'all', 'docs', 'links', 'selectors', 'i18n', 'format'."
        );
        assert_eq!(
            parse(&["/repo", "--fix-type"]).unwrap_err(),
            "Option '--fix-type' requires an argument."
        );
        assert_eq!(
            parse(&["/a", "/b"]).unwrap_err(),
            "Got unexpected extra argument (/b)"
        );
    }

    #[test]
    fn every_fix_type_choice_is_accepted() {
        for choice in ["all", "docs", "links", "selectors", "i18n", "format"] {
            assert_eq!(
                parse(&["/repo", "--fix-type", choice])
                    .unwrap()
                    .unwrap()
                    .fix_type,
                choice
            );
        }
    }

    #[test]
    fn help_body_lists_every_flag() {
        for flag in [
            "--dry-run",
            "--apply",
            "--pr",
            "--fix-type",
            "--skip-approval",
        ] {
            assert!(HELP.contains(flag), "{flag}");
        }
        assert!(HELP.starts_with("Usage: aethyme autofix [OPTIONS] REPO_PATH"));
    }

    #[test]
    fn nonexistent_repo_path_exits_two() {
        assert_eq!(run(&["/nonexistent-path-xyz".to_string()]), 2);
    }
}
