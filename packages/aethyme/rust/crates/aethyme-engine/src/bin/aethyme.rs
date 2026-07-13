//! Aethyme — top-level CLI binary. Entry point for agents using the
//! `aethyme` command.
//!
//! Today this is a thin client that:
//!   1. Serves `explore` natively via the sibling `aethyme-engine-cli`
//!      binary (in-process engine, no Python involved).
//!   2. Serves `broker` and `certify` natively (aethyme-broker crate).
//!   3. Delegates everything else to the Python CLI
//!      (`python -m src.cli ...`).
//!
//! The Python warm-state daemon (`src/daemon.py`) and its socket routing
//! were removed on 2026-07-13 (issue #29): the route had been dead since
//! the 2026-05-08 Python `explore` deletion — the daemon answered nothing
//! but `ping`. Warm-state serving is owned by the engine daemon
//! (`aethyme-engine-cli daemon ...`), which is a separate, live contract.
//!
//! Subcommands today:
//!   aethyme explore --repo X --request "..."   → native engine path
//!   aethyme certify / aethyme broker ...        → native broker crate
//!   aethyme <anything else>                     → delegate to Python CLI
//!
//! The repo path for explore is found by:
//!   1. `--repo <path>` flag (explicit)
//!   2. `$AETHYME_REPO` env var
//!   3. current directory

use std::env;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        print_top_level_help();
        return ExitCode::from(2);
    }

    match args[0].as_str() {
        "-h" | "--help" => {
            print_top_level_help();
            ExitCode::SUCCESS
        }
        "explore" => run_explore(&args[1..]),
        // Broker commands are native Rust from birth (issue #31) — never
        // delegated to Python.
        "broker" => ExitCode::from(aethyme_broker::cli::run(&args[1..])),
        // Certification — top-level by design (the "airport certification"
        // inspection). Strictly read-only; adaptive setup lives in
        // `broker scaffold`.
        "certify" => ExitCode::from(aethyme_broker::cli::run(&args)),
        other => {
            // Unknown to the Rust client — pass straight through to Python.
            // This includes commands like `intents`, `enhance`, `eval`, etc.
            delegate_to_python(other, &args[1..])
        }
    }
}

fn print_top_level_help() {
    eprintln!("aethyme — repository navigation, task localization, dead-code analysis");
    eprintln!();
    eprintln!("Usage: aethyme <subcommand> [args...]");
    eprintln!();
    eprintln!("Hot path:");
    eprintln!("  explore --repo <path> --request \"<task>\" [--format answer-json]");
    eprintln!("                              high-level localization via the native engine");
    eprintln!();
    eprintln!("Agent broker:");
    eprintln!("  certify                     read-only certification checks for this repo");
    eprintln!("  broker adopt|start-agent|agents|cleanup   (see `broker --help`)");
    eprintln!();
    eprintln!("Everything else delegates to the Python CLI:");
    eprintln!("  intents, enhance, eval, analyze, task, graph, ...");
}

// ── explore ─────────────────────────────────────────────────────────────────
//
// Routing precedence (best to worst):
//   1. Native Rust path via the engine daemon (in-process, no subprocess
//      spawn). Returns answer-json directly.
//   2. Python subprocess fallback (cold start every call). Kept only so a
//      broken engine install degrades with a readable error instead of
//      nothing.

fn run_explore(args: &[String]) -> ExitCode {
    if let Some(exit) = try_native_explore(args) {
        return exit;
    }

    // Cold Python subprocess.
    delegate_to_python("explore", args)
}

/// Attempt the native Rust explore path by shelling out to the
/// `aethyme-engine-cli` binary, which lives next to this one in the
/// release directory and implements the full explore pipeline (all 3
/// intents, all detail levels, --intent auto, --max-answer-items,
/// --show-observability, callsite expansion, etc.).
///
/// We forward ALL args verbatim — no parsing or filtering — so any
/// flag the engine CLI supports is supported through this thin
/// client too. Returns Some(exit) when the spawn was successful (we
/// always honor whatever exit code the engine CLI returned, including
/// failures); only returns None on infrastructure errors (binary
/// missing, fork failure) so the caller can fall through to the
/// Python subprocess path.
fn try_native_explore(args: &[String]) -> Option<ExitCode> {
    let cli_path = engine_cli_binary_path()?;
    let mut cmd = Command::new(&cli_path);
    cmd.arg("explore");
    cmd.args(args);
    cmd.stdin(Stdio::inherit());
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());
    match cmd.status() {
        Ok(status) => {
            // exit code 2 from aethyme-engine-cli is the documented
            // "daemon not running" signal; falling back to the Python
            // subprocess path is the right move.
            if status.code() == Some(2) {
                return None;
            }
            Some(match status.code() {
                Some(code) if (0..=255).contains(&code) => ExitCode::from(code as u8),
                _ => ExitCode::from(1),
            })
        }
        Err(_) => None,
    }
}

/// Locate the `aethyme-engine-cli` binary. It's built into the same
/// release directory as this thin client, so resolving the current
/// exe's parent and looking for a sibling is the right strategy. Falls
/// back to PATH search if the sibling isn't present.
fn engine_cli_binary_path() -> Option<PathBuf> {
    if let Ok(mut exe) = env::current_exe() {
        exe.pop();
        let candidate = exe.join("aethyme-engine-cli");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // PATH fallback — useful when the binaries aren't co-located
    // (e.g. installed separately into different bin dirs).
    Some(PathBuf::from("aethyme-engine-cli"))
}

// ── delegation to Python ────────────────────────────────────────────────────

fn delegate_to_python(subcommand: &str, args: &[String]) -> ExitCode {
    let aethyme_root = match env::var("AETHYME_ROOT") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            eprintln!(
                "aethyme: AETHYME_ROOT not set. Export it to the package root \
                 (containing src/cli.py)."
            );
            return ExitCode::from(2);
        }
    };
    let venv_python = aethyme_root.join(".venv").join("bin").join("python");
    let python_bin: PathBuf = if venv_python.exists() {
        venv_python
    } else {
        PathBuf::from("python3")
    };

    let mut cmd = Command::new(&python_bin);
    cmd.arg("-m").arg("src.cli").arg(subcommand);
    cmd.args(args);
    cmd.current_dir(&aethyme_root);
    cmd.stdin(Stdio::inherit());
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());

    match cmd.status() {
        Ok(status) => match status.code() {
            Some(code) if (0..=255).contains(&code) => ExitCode::from(code as u8),
            _ => ExitCode::from(1),
        },
        Err(e) => {
            eprintln!(
                "aethyme: failed to spawn {} -m src.cli: {e}",
                python_bin.display()
            );
            ExitCode::from(127)
        }
    }
}
