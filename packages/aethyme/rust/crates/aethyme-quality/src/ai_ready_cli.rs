//! Native `aethyme ai-ready` front end (retirement Phase 4 flip).
//!
//! Stdout is byte-compatible with the deleted Click command
//! (`ai_ready` in `src/cli.py` before the flip), verified by the
//! Phase 4 CLI golden capture (python vs router, volatile fields
//! scrubbed). Divergences follow the Phase 2/3 precedent, recorded in
//! the `cli-surface-v1.md` header:
//! - usage errors keep Click's `Error: {message}` line (exact Click
//!   8.3 wording) but drop the preceding usage block;
//! - `--help` prints the Click help body with the router spelling
//!   (`Usage: aethyme ai-ready [OPTIONS]`) instead of
//!   `python -m src.cli ai-ready`;
//! - the interactive progress bar renders as its piped form (the bare
//!   `Scanning repository` label line) on TTYs too — cosmetic only,
//!   scripted output is identical.

use std::path::PathBuf;

use crate::engine::ScorecardEngine;
use crate::format::{format_json, format_markdown};
use crate::model::ScorecardReport;

const HELP: &str = "Usage: aethyme ai-ready [OPTIONS]

  Run AI-readiness scorecard on a repository.

Options:
  --repo PATH                  Repository path (defaults to current directory)
  --repo-id TEXT               Repository ID for API mode
  -f, --format [json|md|both]  Output format
  -o, --output PATH            Output file (defaults to stdout)
  --detectors TEXT             Comma-separated list of detectors to run (runs
                               all if not specified)
  --help                       Show this message and exit.
";

#[derive(Debug)]
struct Args {
    repo: Option<String>,
    repo_id: Option<String>,
    format: String,
    output: Option<String>,
    detectors: Option<Vec<String>>,
}

/// Run `ai-ready` with `args` excluding the leading command name.
/// Prints its own output/errors; returns the process exit code.
pub fn run(args: &[String]) -> u8 {
    let parsed = match parse_args(args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => {
            // --help
            print!("{HELP}");
            return 0;
        }
        Err(message) => {
            eprintln!("Error: {message}");
            return 2;
        }
    };

    // Determine repository path (Python: Path(repo).resolve() / cwd).
    let repo_path: PathBuf = match &parsed.repo {
        Some(repo) => {
            let given = PathBuf::from(repo);
            // click.Path(exists=True) validated before the command body.
            if !given.exists() {
                eprintln!(
                    "Error: Invalid value for '--repo': Path '{repo}' does not exist."
                );
                return 2;
            }
            std::fs::canonicalize(&given).unwrap_or(given)
        }
        None => match std::env::current_dir() {
            Ok(cwd) => cwd,
            Err(e) => {
                eprintln!("Error during scan: {e}");
                return 2;
            }
        },
    };

    println!("Running AI-readiness scorecard on: {}", repo_path.display());
    println!();

    let tenant_id = std::env::var("AETHYME_TENANT_ID")
        .ok()
        .filter(|v| !v.is_empty());

    let engine = match ScorecardEngine::new(&repo_path, parsed.repo_id.clone(), tenant_id) {
        Ok(engine) => engine,
        Err(message) => {
            eprintln!("Error during scan: {message}");
            return 2;
        }
    };

    // click.progressbar prints only its label when output is piped;
    // rendered identically here (see module docs for the TTY note).
    println!("Scanning repository");
    let report = engine.scan(parsed.detectors.as_deref());

    println!();
    println!("Scan completed: Score {}/100", report.score);
    println!(
        "Findings: {} total ({} blockers, {} warnings, {} info)",
        report.total_findings, report.blocker_count, report.warning_count, report.info_count
    );
    println!();

    if let Some(code) = emit_reports(&parsed, &report) {
        return code;
    }

    // Exit code: 0 ready (>=90, no findings above info), 1 warnings,
    // 2 blockers/low score — the exact Python ladder.
    if report.blocker_count > 0 || report.score < 50 {
        2
    } else if report.warning_count > 0 || report.score < 90 {
        1
    } else {
        0
    }
}

fn parse_args(args: &[String]) -> Result<Option<Args>, String> {
    let mut parsed = Args {
        repo: None,
        repo_id: None,
        format: "md".to_string(),
        output: None,
        detectors: None,
    };
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        let mut take_value = |name: &str| -> Result<String, String> {
            match args.get(i + 1) {
                Some(v) => Ok(v.clone()),
                None => Err(format!("Option '{name}' requires an argument.")),
            }
        };
        match arg {
            "--help" => return Ok(None),
            "--repo" => {
                parsed.repo = Some(take_value("--repo")?);
                i += 2;
            }
            "--repo-id" => {
                parsed.repo_id = Some(take_value("--repo-id")?);
                i += 2;
            }
            "--format" | "-f" => {
                parsed.format = take_value(arg)?;
                i += 2;
            }
            "--output" | "-o" => {
                parsed.output = Some(take_value(arg)?);
                i += 2;
            }
            "--detectors" => {
                let raw = take_value("--detectors")?;
                parsed.detectors =
                    Some(raw.split(',').map(|s| s.trim().to_string()).collect());
                i += 2;
            }
            other => {
                if let Some(value) = other.strip_prefix("--repo-id=") {
                    parsed.repo_id = Some(value.to_string());
                } else if let Some(value) = other.strip_prefix("--repo=") {
                    parsed.repo = Some(value.to_string());
                } else if let Some(value) = other.strip_prefix("--format=") {
                    parsed.format = value.to_string();
                } else if let Some(value) = other.strip_prefix("--output=") {
                    parsed.output = Some(value.to_string());
                } else if let Some(value) = other.strip_prefix("--detectors=") {
                    parsed.detectors =
                        Some(value.split(',').map(|s| s.trim().to_string()).collect());
                } else {
                    return Err(format!("No such option '{other}'."));
                }
                i += 1;
            }
        }
    }
    if !["json", "md", "both"].contains(&parsed.format.as_str()) {
        return Err(format!(
            "Invalid value for '--format' / '-f': '{}' is not one of 'json', 'md', 'both'.",
            parsed.format
        ));
    }
    Ok(Some(parsed))
}

/// Report emission: the exact Python branch structure for
/// stdout-vs-file and the `both` basename handling. Returns an exit
/// code on write failure.
fn emit_reports(args: &Args, report: &ScorecardReport) -> Option<u8> {
    let format = args.format.as_str();

    if format == "json" || format == "both" {
        let json_output = format_json(report);
        if args.output.is_some() && format == "json" {
            let output_path = PathBuf::from(args.output.as_ref().unwrap());
            if let Err(e) = std::fs::write(&output_path, &json_output) {
                eprintln!("Error during scan: {e}");
                return Some(2);
            }
            println!("JSON report written to: {}", output_path.display());
        } else if format == "both" {
            let output_base = args
                .output
                .clone()
                .unwrap_or_else(|| "scorecard-report".to_string());
            let json_path = with_py_suffix(&output_base, ".json");
            if let Err(e) = std::fs::write(&json_path, &json_output) {
                eprintln!("Error during scan: {e}");
                return Some(2);
            }
            println!("JSON report written to: {}", json_path.display());
        } else {
            println!("{json_output}");
        }
    }

    if format == "md" || format == "both" {
        let md_output = format_markdown(report);
        if args.output.is_some() && format == "md" {
            let output_path = PathBuf::from(args.output.as_ref().unwrap());
            if let Err(e) = std::fs::write(&output_path, &md_output) {
                eprintln!("Error during scan: {e}");
                return Some(2);
            }
            println!("Markdown report written to: {}", output_path.display());
        } else if format == "both" {
            let output_base = args
                .output
                .clone()
                .unwrap_or_else(|| "scorecard-report".to_string());
            let md_path = with_py_suffix(&output_base, ".md");
            if let Err(e) = std::fs::write(&md_path, &md_output) {
                eprintln!("Error during scan: {e}");
                return Some(2);
            }
            println!("Markdown report written to: {}", md_path.display());
        } else {
            println!("{md_output}");
        }
    }

    None
}

/// Python `path if path.suffix == want else path.with_suffix(want)`:
/// keep an exact-suffix path, otherwise REPLACE any existing suffix
/// (`rep.md` becomes `rep.json`, `rep` becomes `rep.json`).
fn with_py_suffix(base: &str, want: &str) -> PathBuf {
    let path = PathBuf::from(base);
    let current = crate::walk::py_suffix(&path);
    if current == want {
        return path;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let stem = if current.is_empty() {
        name.clone()
    } else {
        name[..name.len() - current.len()].to_string()
    };
    path.with_file_name(format!("{stem}{want}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_py_suffix_matches_pathlib() {
        assert_eq!(with_py_suffix("rep", ".json"), PathBuf::from("rep.json"));
        assert_eq!(with_py_suffix("rep.md", ".json"), PathBuf::from("rep.json"));
        assert_eq!(with_py_suffix("rep.json", ".json"), PathBuf::from("rep.json"));
        assert_eq!(
            with_py_suffix("a/b.c.md", ".json"),
            PathBuf::from("a/b.c.json")
        );
    }

    #[test]
    fn parse_rejects_click_style_errors() {
        let err = parse_args(&["--format".to_string(), "bogus".to_string()]).unwrap_err();
        assert_eq!(
            err,
            "Invalid value for '--format' / '-f': 'bogus' is not one of 'json', 'md', 'both'."
        );
        let err = parse_args(&["--bogus".to_string()]).unwrap_err();
        assert_eq!(err, "No such option '--bogus'.");
        let err = parse_args(&["--repo".to_string()]).unwrap_err();
        assert_eq!(err, "Option '--repo' requires an argument.");
    }
}
