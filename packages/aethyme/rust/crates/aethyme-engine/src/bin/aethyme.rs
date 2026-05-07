//! Aethyme — top-level CLI binary. Entry point for agents using the
//! `aethyme` command.
//!
//! Today this is a thin client that:
//!   1. Detects whether an `aethyme` daemon is running for the target repo
//!      (`<repo>/.aethyme/aethyme.sock`) and routes the request through it
//!      if so. Daemon mode amortizes Python interpreter + import startup
//!      cost across many calls — the headline latency win.
//!   2. Falls back to invoking the Python CLI (`python -m src.cli ...`)
//!      when no daemon is present. Same behaviour as today; just a friendlier
//!      entry point.
//!
//! Future direction: individual hot-path subcommands (explore in particular)
//! move to native Rust by linking `aethyme-engine` as a library and
//! reimplementing the orchestration layer that currently lives in Python.
//! See packages/aethyme/docs/architecture/aethyme-cli-design.md (TBD).
//!
//! Subcommands today:
//!   aethyme explore --repo X --request "..."   → daemon-routed if available
//!   aethyme daemon start --repo X              → spawn the Python daemon
//!   aethyme daemon stop --repo X               → terminate the daemon
//!   aethyme daemon status --repo X             → check daemon health
//!   aethyme <anything else>                    → delegate to Python CLI
//!
//! The repo path is required for daemon routing and is found by:
//!   1. `--repo <path>` flag (explicit)
//!   2. `$AETHYME_REPO` env var
//!   3. current directory

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use sha2::{Digest, Sha256};

const AETHYME_DIR: &str = ".aethyme";
const SOCKET_INFO_FILENAME: &str = "aethyme-socket.path";
const DAEMON_CONNECT_TIMEOUT_MS: u64 = 200;

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
        "daemon" => run_daemon_subcommand(&args[1..]),
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
    eprintln!("                              high-level localization, daemon-routed if available");
    eprintln!();
    eprintln!("Daemon:");
    eprintln!("  daemon start --repo <path>  start the warm-state daemon for a repo");
    eprintln!("  daemon stop  --repo <path>  terminate the daemon");
    eprintln!("  daemon status --repo <path> health-check the daemon");
    eprintln!();
    eprintln!("Everything else delegates to the Python CLI:");
    eprintln!("  intents, enhance, eval, analyze, task, graph, ...");
}

// ── explore ─────────────────────────────────────────────────────────────────
//
// Routing precedence (best to worst):
//   1. Native Rust path via the engine daemon (in-process, no subprocess
//      spawn). Returns answer-json directly. Session-1 scope: anchors +
//      in-scope files only; trust capped at needs_verification.
//   2. Python daemon socket (the Python explore orchestrator running as a
//      long-lived process). Richer evidence — symbol search, source-text,
//      callsite expansion. Pays Python startup once at daemon start.
//   3. Python subprocess fallback. Pays Python startup every call.
//
// Each step falls through on failure so the caller always gets an answer
// (or a clean "daemon not running" + subprocess output).

fn run_explore(args: &[String]) -> ExitCode {
    let repo = resolve_repo(args);

    if let Some(repo_path) = repo.as_ref() {
        // Native Rust path is the preferred route when the engine daemon
        // is up. In-process call into the engine library; no subprocess
        // overhead, no inter-binary roundtrip.
        if let Some(exit) = try_native_explore(repo_path, args) {
            return exit;
        }

        // Python daemon (warm Python state). One subprocess hop instead
        // of two (skips the per-call interpreter startup).
        if let Some(response) = try_daemon(repo_path, "explore", args) {
            print!("{response}");
            return ExitCode::SUCCESS;
        }
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
/// Python daemon / Python subprocess path.
///
/// Pre-widening this used to gate on session-1 limitations (compact
/// detail only, no --intent, no --params) and bail to Python for
/// anything richer. Since sessions 2-5 + the four deployment items
/// closed those gaps, the native binary handles everything; the gate
/// is now removed.
fn try_native_explore(repo: &Path, args: &[String]) -> Option<ExitCode> {
    let _ = repo; // No longer needed for capability gating.
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
            // daemon / subprocess path is the right move.
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

fn try_daemon(repo: &Path, command: &str, args: &[String]) -> Option<String> {
    let socket_path = daemon_socket_path(repo)?;
    if !socket_path.exists() {
        return None;
    }

    let mut stream = match UnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(_) => return None,
    };

    // Bound the read so a hung daemon doesn't block forever; falling back to
    // the Python CLI is always safe.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(120)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(DAEMON_CONNECT_TIMEOUT_MS * 5)));

    let request = serde_json::json!({
        "command": command,
        "args": args,
    });
    let request_line = format!("{}\n", request);
    if stream.write_all(request_line.as_bytes()).is_err() {
        return None;
    }
    if stream.flush().is_err() {
        return None;
    }

    let mut response = String::new();
    if stream.read_to_string(&mut response).is_err() {
        return None;
    }
    if response.is_empty() {
        return None;
    }
    Some(response)
}

// ── daemon subcommand ───────────────────────────────────────────────────────

fn run_daemon_subcommand(args: &[String]) -> ExitCode {
    if args.is_empty() {
        eprintln!("usage: aethyme daemon <start|stop|status> --repo <path>");
        return ExitCode::from(2);
    }

    let action = &args[0];
    let repo = match resolve_repo(&args[1..]) {
        Some(p) => p,
        None => {
            eprintln!(
                "aethyme daemon: --repo <path> is required (or set $AETHYME_REPO)"
            );
            return ExitCode::from(2);
        }
    };

    match action.as_str() {
        "status" => daemon_status(&repo),
        "start" => daemon_start(&repo),
        "stop" => daemon_stop(&repo),
        other => {
            eprintln!("aethyme daemon: unknown action '{other}'");
            ExitCode::from(2)
        }
    }
}

fn daemon_status(repo: &Path) -> ExitCode {
    let Some(socket_path) = daemon_socket_path(repo) else {
        println!("daemon: not running (no socket pointer for {})", repo.display());
        return ExitCode::from(1);
    };
    if !socket_path.exists() {
        println!("daemon: not running ({} does not exist)", socket_path.display());
        return ExitCode::from(1);
    }
    match UnixStream::connect(&socket_path) {
        Ok(_) => {
            println!("daemon: running ({})", socket_path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!(
                "daemon: socket exists but connection failed: {e} ({})",
                socket_path.display()
            );
            ExitCode::from(1)
        }
    }
}

fn daemon_start(repo: &Path) -> ExitCode {
    // Delegate to the Python daemon implementation. The Python CLI owns the
    // explore orchestration logic; the daemon just keeps the interpreter and
    // its imports warm across many calls.
    delegate_to_python("daemon", &["start".into(), "--repo".into(), repo.display().to_string()])
}

fn daemon_stop(repo: &Path) -> ExitCode {
    delegate_to_python("daemon", &["stop".into(), "--repo".into(), repo.display().to_string()])
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
            eprintln!("aethyme: failed to spawn {} -m src.cli: {e}", python_bin.display());
            ExitCode::from(127)
        }
    }
}

// ── socket path resolution (must match src/daemon.py) ──────────────────────
//
// The daemon binds to $TMPDIR/aethyme/daemon-<hash>.sock, where <hash> is the
// first 16 hex chars of SHA-256 of the repo's resolved (canonical) path.
// macOS limits AF_UNIX paths to ~104 bytes, which many Aethyme-enhanced repo
// paths exceed; the indirection is mandatory.
//
// As a safety net we also read .aethyme/aethyme-socket.path inside the repo
// when present — that's the pointer file the daemon writes for any tool that
// would rather not recompute the hash.

fn daemon_socket_path(repo: &Path) -> Option<PathBuf> {
    let canonical = match repo.canonicalize() {
        Ok(p) => p,
        Err(_) => repo.to_path_buf(),
    };

    // Pointer file path takes priority: it's authoritative for any daemon
    // started with a different convention (or via a different binary).
    let info = canonical.join(AETHYME_DIR).join(SOCKET_INFO_FILENAME);
    if let Ok(text) = fs::read_to_string(&info) {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }

    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    let tmp = env::var("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    Some(tmp.join("aethyme").join(format!("daemon-{hex}.sock")))
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn resolve_repo(args: &[String]) -> Option<PathBuf> {
    if let Some(value) = find_arg(args, "--repo") {
        return Some(PathBuf::from(value));
    }
    if let Ok(repo) = env::var("AETHYME_REPO") {
        if !repo.is_empty() {
            return Some(PathBuf::from(repo));
        }
    }
    env::current_dir().ok()
}

fn find_arg(args: &[String], flag: &str) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == flag {
            if i + 1 < args.len() {
                return Some(args[i + 1].clone());
            }
            return None;
        }
        if let Some(rest) = args[i].strip_prefix(&format!("{flag}=")) {
            return Some(rest.to_string());
        }
        i += 1;
    }
    None
}
