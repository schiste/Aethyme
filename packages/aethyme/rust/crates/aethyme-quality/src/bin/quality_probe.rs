//! quality-probe — migration parity tooling for retirement Phase 4.
//!
//! Runs the native scorecard engine on a repository and prints the
//! report exactly as the formatters render it, so the scratch parity
//! harness can diff Python vs Rust per detector and per format. Not
//! part of the frozen CLI surface; retires with the harness in Phase 6.
//!
//! Usage:
//!   quality-probe <repo> [--detectors a,b,c] [--format json|md]

use std::path::PathBuf;
use std::process::ExitCode;

use aethyme_quality::engine::ScorecardEngine;
use aethyme_quality::format::{format_json, format_markdown};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut repo: Option<String> = None;
    let mut detectors: Option<Vec<String>> = None;
    let mut format = "json".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--detectors" => {
                let Some(v) = args.get(i + 1) else {
                    eprintln!("--detectors requires a value");
                    return ExitCode::from(2);
                };
                detectors = Some(v.split(',').map(|s| s.trim().to_string()).collect());
                i += 2;
            }
            "--format" => {
                let Some(v) = args.get(i + 1) else {
                    eprintln!("--format requires a value");
                    return ExitCode::from(2);
                };
                format = v.clone();
                i += 2;
            }
            other => {
                if repo.is_some() {
                    eprintln!("unexpected argument: {other}");
                    return ExitCode::from(2);
                }
                repo = Some(other.to_string());
                i += 1;
            }
        }
    }
    let Some(repo) = repo else {
        eprintln!("usage: quality-probe <repo> [--detectors a,b,c] [--format json|md]");
        return ExitCode::from(2);
    };
    // Mirror the CLI: Path(repo).resolve().
    let repo_path = std::fs::canonicalize(&repo).unwrap_or_else(|_| PathBuf::from(&repo));
    let engine = match ScorecardEngine::new(&repo_path, None, None) {
        Ok(engine) => engine,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let report = engine.scan(detectors.as_deref());
    match format.as_str() {
        "json" => println!("{}", format_json(&report)),
        "md" => println!("{}", format_markdown(&report)),
        other => {
            eprintln!("unknown format: {other}");
            return ExitCode::from(2);
        }
    }
    ExitCode::SUCCESS
}
