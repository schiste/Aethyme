//! fix-probe — migration parity tooling for retirement Phase 5.
//!
//! Exposes the autofix internals in a line-oriented form the scratch
//! parity harness can diff against the Python `src/autofixers/`
//! package: safety verdicts per path, validation results per content
//! pair, and (once the fixers land) the produced patches. Not part of
//! the frozen CLI surface; retires with the harness in Phase 6.
//!
//! Usage:
//!   fix-probe safety <fix_type> < paths-on-stdin
//!   fix-probe validate <original-file> <new-file>

use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

use aethyme_quality::fix::patch::FilePatch;
use aethyme_quality::fix::safety::{RiskLevel, SafetyEngine};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("safety") => safety_mode(&args[1..]),
        Some("validate") => validate_mode(&args[1..]),
        Some("diff") => diff_mode(&args[1..]),
        other => {
            eprintln!("unknown mode: {}", other.unwrap_or("<none>"));
            eprintln!(
                "usage: fix-probe safety <fix_type> | validate <a> <b> | diff <a> <b> <name>"
            );
            ExitCode::from(2)
        }
    }
}

/// One `path\tgenerated\trisk-or-error` row per stdin line, so the
/// harness can feed a large path corpus through both implementations.
fn safety_mode(args: &[String]) -> ExitCode {
    let fix_type = args.first().map(String::as_str).unwrap_or("any_fix");
    let engine = SafetyEngine::new();
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        eprintln!("could not read stdin");
        return ExitCode::from(2);
    }
    for line in input.lines() {
        if line.is_empty() {
            continue;
        }
        let path = Path::new(line);
        let generated = engine.detector.is_generated(path);
        let risk = match engine.assess_risk(path, fix_type) {
            Ok(level) => level.as_str().to_string(),
            Err(message) => format!("ERROR:{message}"),
        };
        let skip = engine.should_skip_file(path);
        println!("{line}\t{generated}\t{risk}\t{skip}");
    }
    ExitCode::SUCCESS
}

/// The produced unified diff for a content pair, byte-for-byte as
/// `FilePatch.generate_diff` renders it.
fn diff_mode(args: &[String]) -> ExitCode {
    let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
        eprintln!("usage: fix-probe diff <original-file> <new-file> [name]");
        return ExitCode::from(2);
    };
    let name = args.get(2).cloned().unwrap_or_else(|| "x.py".to_string());
    let (Ok(original), Ok(new)) = (std::fs::read_to_string(a), std::fs::read_to_string(b)) else {
        eprintln!("could not read inputs");
        return ExitCode::from(2);
    };
    let patch = FilePatch::new(
        std::path::PathBuf::from(name),
        original,
        new,
        "probe".to_string(),
        RiskLevel::Low,
    );
    print!("{}", patch.generate_diff());
    ExitCode::SUCCESS
}

fn validate_mode(args: &[String]) -> ExitCode {
    let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
        eprintln!("usage: fix-probe validate <original-file> <new-file>");
        return ExitCode::from(2);
    };
    let (Ok(original), Ok(new)) = (std::fs::read(a), std::fs::read(b)) else {
        eprintln!("could not read inputs");
        return ExitCode::from(2);
    };
    let original = String::from_utf8_lossy(&original).to_string();
    let new = String::from_utf8_lossy(&new).to_string();
    let engine = SafetyEngine::new();
    let result = engine.validate_changes(&original, &new);
    println!("safe\t{}", result.safe);
    println!("original_lines\t{}", result.stats.original_lines);
    println!("new_lines\t{}", result.stats.new_lines);
    println!("lines_added\t{}", result.stats.lines_added);
    println!("size_change_bytes\t{}", result.stats.size_change_bytes);
    for warning in &result.warnings {
        println!("warning\t{warning}");
    }
    ExitCode::SUCCESS
}
