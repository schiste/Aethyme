//! Aethyme — top-level CLI binary. Entry point for agents using the
//! `aethyme` command.
//!
//! Single-entrypoint design (issue #31, 2026-07-14):
//!   1. `explore` runs **in-process** via the engine library (shared front
//!      end in `aethyme_engine::explore_cli`). When the engine daemon isn't
//!      running for the target repo, the router starts one (detached, via
//!      the sibling `aethyme-engine-cli` serve binary), waits for the
//!      socket, and retries — `aethyme explore` just works.
//!   2. `broker` and `certify` are native (aethyme-broker crate).
//!   3. Everything else delegates to the Python CLI (`python -m src.cli`),
//!      whose location is resolved by `resolve_aethyme_root()`:
//!      `$AETHYME_ROOT` → pointer file `~/.config/aethyme/root` → upward
//!      walk from the current directory. `aethyme root set/show` manages
//!      the pointer file.
//!
//! The pip console script that used to shadow this binary was removed from
//! pyproject.toml in the same change — `pip install -e .` no longer
//! installs an `aethyme` executable; `python -m src.cli` remains the
//! Python-side invocation.
//!
//! The repo path for explore is found by:
//!   1. `--repo <path>` flag (explicit)
//!   2. `$AETHYME_REPO` env var
//!   3. current directory

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

/// Upper bound for waiting on a freshly-spawned engine daemon. The socket
/// binds only after the initial map build (~70s on a 12K-file repo), so
/// this is generous rather than snappy on purpose.
const DAEMON_READY_TIMEOUT: Duration = Duration::from_secs(240);

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
        "-V" | "--version" => {
            print_version();
            ExitCode::SUCCESS
        }
        "explore" => run_explore(&args[1..]),
        "verify-targets" => run_verify_targets(&args[1..]),
        // Native since python-retirement Phase 1 (the Python `query`
        // group is deleted). Errors keep Click's `Error: {msg}` shape
        // and exit 1 so scripted consumers see the same surface.
        "graph" => match aethyme_engine::graph_cli::run(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("Error: {message}");
                ExitCode::from(1)
            }
        },
        "analyze" => match aethyme_engine::analyze_cli::run(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("Error: {message}");
                ExitCode::from(1)
            }
        },
        "facts" => match aethyme_engine::facts_cli::run_facts(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("Error: {message}");
                ExitCode::from(1)
            }
        },
        "intents" => match aethyme_engine::facts_cli::run_intents(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("Error: {message}");
                ExitCode::from(1)
            }
        },
        // Fully native since python-retirement Phase 3: the engine-facing
        // basics answer in aethyme_engine::repo_cli, the UX subcommands
        // (skills, overrides, telemetry, commit hygiene) in
        // aethyme_enhance::repo_cli. The Python repo group is deleted;
        // unknown subcommands (and `--help`) get the native error shape
        // like the other native groups.
        "repo" => match aethyme_engine::repo_cli::run(&args[1..]) {
            aethyme_engine::repo_cli::Outcome::Handled(Ok(())) => ExitCode::SUCCESS,
            aethyme_engine::repo_cli::Outcome::Handled(Err(message)) => {
                eprintln!("Error: {message}");
                ExitCode::from(1)
            }
            aethyme_engine::repo_cli::Outcome::Delegate => {
                // The UX slice resolves the package root lazily: only
                // deploy-skills/compile-skills need it (template
                // substitution); the hook-facing telemetry commands must
                // work in repos with no Aethyme checkout reachable.
                let resolve = || resolve_aethyme_root().map(|(path, _source)| path);
                match aethyme_enhance::repo_cli::run(&args[1..], &resolve) {
                    aethyme_enhance::repo_cli::Outcome::Handled(code) => ExitCode::from(code),
                    aethyme_enhance::repo_cli::Outcome::Delegate => {
                        eprintln!(
                            "Error: unsupported repo subcommand: {}",
                            args.get(1).map(String::as_str).unwrap_or("<none>")
                        );
                        ExitCode::from(2)
                    }
                }
            }
        },
        "task" => match aethyme_engine::task_cli::run(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("Error: {message}");
                ExitCode::from(1)
            }
        },
        "query" => match aethyme_engine::query_cli::run(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("Error: {message}");
                ExitCode::from(1)
            }
        },
        "root" => run_root_subcommand(&args[1..]),
        // Broker commands are native Rust from birth (issue #31) — never
        // delegated to Python.
        "broker" => ExitCode::from(aethyme_broker::cli::run(&args[1..])),
        // Certification — top-level by design (the "airport certification"
        // inspection). Strictly read-only; adaptive setup lives in
        // `broker scaffold`.
        "certify" => ExitCode::from(aethyme_broker::cli::run(&args)),
        // Guided setup — certify + scaffold + gates draft in sequence,
        // idempotent. Top-level like certify: it is the first command a
        // new repo runs.
        "init" => ExitCode::from(aethyme_broker::cli::run(&args)),
        // Native since python-retirement Phase 2 (the Python `enhance`
        // group is deleted). deploy/verify answer natively; unknown
        // subcommands (and `--help`) get a native error like the other
        // native groups — there is no Python surface to delegate to.
        // The package root is still resolved: rendered templates embed
        // `{{AETHYME_ROOT}}` substitutions pointing at the checkout.
        // Native since python-retirement Phase 4 (the Python `ai-ready`
        // command and src/scorecard/ are deleted). The 8 detectors,
        // integer scoring, and json/md renderers live in the
        // aethyme-quality crate; stdout and report bytes are parity-
        // verified against the last Python implementation. Usage errors
        // keep Click's `Error: {message}` line without the usage block
        // (Phase 2 precedent).
        "ai-ready" => ExitCode::from(aethyme_quality::ai_ready_cli::run(&args[1..])),
        // Native since python-retirement Phase 5 (the Python `autofix`
        // command and src/autofixers/ are deleted). The safety/risk
        // engine, patch generation, the 5 fixers, and the git/PR helper
        // live in the same aethyme-quality crate; stdout, produced
        // unified diffs, and post-apply trees are parity-verified
        // against the last Python implementation. The PR-mode approval
        // gate is unchanged: medium/high-risk patches stop the flow
        // before anything is applied, committed, or pushed. Usage
        // errors keep Click's `Error: {message}` line without the usage
        // block (Phase 2 precedent).
        "autofix" => ExitCode::from(aethyme_quality::autofix_cli::run(&args[1..])),
        "enhance" => {
            let Some((aethyme_root, _source)) = resolve_aethyme_root() else {
                eprintln!(
                    "aethyme: cannot locate the Aethyme package root for the \
                     'enhance' command."
                );
                print_root_guidance();
                return ExitCode::from(2);
            };
            ExitCode::from(aethyme_enhance::cli::run(&aethyme_root, &args[1..]))
        }
        other => {
            // Unknown to the Rust client — pass straight through to Python.
            // This includes commands like `intents`, `eval`, etc.
            delegate_to_python(other, &args[1..])
        }
    }
}

/// Crate version plus `git describe` from build time (empty when the
/// binary was built outside a git checkout — see build.rs).
fn print_version() {
    let describe = env!("AETHYME_GIT_DESCRIBE");
    if describe.is_empty() {
        println!("aethyme {}", env!("CARGO_PKG_VERSION"));
    } else {
        println!("aethyme {} ({describe})", env!("CARGO_PKG_VERSION"));
    }
}

fn print_top_level_help() {
    eprintln!("aethyme — repository navigation, task localization, agent brokering");
    eprintln!();
    eprintln!("Usage: aethyme <subcommand> [args...]");
    eprintln!("       aethyme --version | -V");
    eprintln!();
    eprintln!("Hot path:");
    eprintln!("  explore --repo <path> --request \"<task>\" [--format answer-json]");
    eprintln!("                              in-process engine; auto-starts the engine daemon");
    eprintln!(
        "  verify-targets --repo <path> --from explore.json [--max-targets 2 --max-lines 80]"
    );
    eprintln!("                              bounded source spans for Explore targets");
    eprintln!();
    eprintln!("Agent broker:");
    eprintln!("  init                        guided setup: certify + scaffold + gates draft");
    eprintln!("  certify                     read-only certification checks for this repo");
    eprintln!("  broker adopt|start-agent|agents|cleanup   (see `broker --help`)");
    eprintln!();
    eprintln!("Setup:");
    eprintln!(
        "  root show|set <path>        where the Python package lives (for delegated commands)"
    );
    eprintln!();
    eprintln!("Quality:");
    eprintln!("  ai-ready [--repo <path>]    AI-readiness scorecard");
    eprintln!("  autofix <path> [--dry-run|--apply|--pr]");
    eprintln!("                              safe automated fixes (see `autofix --help`)");
    eprintln!();
    // Phase 5 emptied the Click tree: no command delegates any more.
    // The delegation path itself retires with `src/` in Phase 6.
    eprintln!("No subcommand delegates to Python; unknown ones are errors.");
}

// ── explore ─────────────────────────────────────────────────────────────────
//
// In-process engine call via the shared front end. No Python involvement:
// the Python explore orchestrator was deleted 2026-05-08, so the old
// "fall back to Python" tail was already a dead path. The only recoverable
// condition is "daemon not running", which the router now fixes itself.

fn run_explore(args: &[String]) -> ExitCode {
    use aethyme_engine::explore_cli::{ExploreCliOutcome, run};
    match run(args) {
        ExploreCliOutcome::Done => ExitCode::SUCCESS,
        ExploreCliOutcome::BadUsage(msg) => {
            eprintln!("{msg}");
            ExitCode::from(2)
        }
        ExploreCliOutcome::Failed(msg) => {
            eprintln!("{msg}");
            ExitCode::from(1)
        }
        ExploreCliOutcome::DaemonNotRunning { repo } => {
            eprintln!(
                "explore: engine daemon not running for {} — starting it \
                 (first map build can take a minute on large repos)…",
                repo.display()
            );
            if let Err(msg) = start_engine_daemon_and_wait(&repo) {
                eprintln!("explore: {msg}");
                return ExitCode::from(1);
            }
            match run(args) {
                ExploreCliOutcome::Done => ExitCode::SUCCESS,
                ExploreCliOutcome::DaemonNotRunning { .. } => {
                    eprintln!(
                        "explore: engine daemon still not reachable after start; \
                         check {}",
                        aethyme_engine::daemon::logfile_path_for(&repo).display()
                    );
                    ExitCode::from(1)
                }
                ExploreCliOutcome::BadUsage(msg) => {
                    eprintln!("{msg}");
                    ExitCode::from(2)
                }
                ExploreCliOutcome::Failed(msg) => {
                    eprintln!("{msg}");
                    ExitCode::from(1)
                }
            }
        }
    }
}

fn run_verify_targets(args: &[String]) -> ExitCode {
    use aethyme_engine::verify_targets_cli::{VerifyTargetsCliOutcome, run};
    match run(args) {
        VerifyTargetsCliOutcome::Done => ExitCode::SUCCESS,
        VerifyTargetsCliOutcome::BadUsage(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
        VerifyTargetsCliOutcome::Failed(message) => {
            eprintln!("{message}");
            ExitCode::from(1)
        }
    }
}

fn start_engine_daemon_and_wait(repo: &Path) -> Result<(), String> {
    use aethyme_engine::daemon::{self, ReadyOutcome, StartOutcome};
    let serve_exe = engine_cli_binary_path();
    let opts = daemon::StartOptions::default();
    let mut spawned = match daemon::start_detached(repo, &serve_exe, &opts)? {
        StartOutcome::Spawned(child) => Some(child),
        // Already running but the socket refused just now — likely mid
        // map-build. Don't watch a process we didn't spawn.
        StartOutcome::AlreadyRunning(_) => None,
    };
    match daemon::wait_until_ready(repo, spawned.as_mut(), DAEMON_READY_TIMEOUT) {
        ReadyOutcome::Ready => Ok(()),
        ReadyOutcome::ProcessExited => Err(format!(
            "engine daemon exited during startup (is the repo indexed? \
             run `aethyme-graph-index` + `aethyme-engine-cli index`). Log tail:\n{}",
            daemon::log_tail(repo, 5)
        )),
        ReadyOutcome::TimedOut => Err(format!(
            "engine daemon did not become ready within {}s; log: {}",
            DAEMON_READY_TIMEOUT.as_secs(),
            daemon::logfile_path_for(repo).display()
        )),
    }
}

/// Locate the `aethyme-engine-cli` binary, used only as the detached
/// daemon-serve process. It's built into the same directory as this
/// binary (both by `cargo build` and by `cargo install --path`), so a
/// sibling lookup covers both layouts; PATH is the fallback.
fn engine_cli_binary_path() -> PathBuf {
    if let Ok(mut exe) = env::current_exe() {
        exe.pop();
        let candidate = exe.join("aethyme-engine-cli");
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from("aethyme-engine-cli")
}

// ── aethyme root — where the Python package lives ───────────────────────────
//
// Delegated commands (`enhance`, `repo`, `intents`, …) are implemented by
// the Python package in the Aethyme source checkout. Resolution order:
//   1. $AETHYME_ROOT (explicit override, highest priority)
//   2. pointer file: $XDG_CONFIG_HOME/aethyme/root (default ~/.config/aethyme/root)
//   3. upward walk from the current directory (covers working inside the
//      Aethyme repo or any of its worktrees with zero configuration)

fn config_pointer_path() -> Option<PathBuf> {
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("aethyme").join("root"))
}

/// A directory qualifies as an Aethyme Python root when it contains the
/// package sources this router delegates to.
fn is_aethyme_root(dir: &Path) -> bool {
    dir.join("src").join("cli.py").is_file() && dir.join("pyproject.toml").is_file()
}

fn resolve_aethyme_root() -> Option<(PathBuf, &'static str)> {
    if let Ok(v) = env::var("AETHYME_ROOT")
        && !v.is_empty()
    {
        return Some((PathBuf::from(v), "AETHYME_ROOT env"));
    }
    if let Some(pointer) = config_pointer_path()
        && let Ok(text) = std::fs::read_to_string(&pointer)
    {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            let path = PathBuf::from(trimmed);
            if is_aethyme_root(&path) {
                return Some((path, "pointer file"));
            }
        }
    }
    // Upward walk: <dir>/packages/aethyme (monorepo root or any worktree)
    // or <dir> itself (running from inside the package).
    let mut dir = env::current_dir().ok()?;
    loop {
        if is_aethyme_root(&dir) {
            return Some((dir, "upward walk"));
        }
        let candidate = dir.join("packages").join("aethyme");
        if is_aethyme_root(&candidate) {
            return Some((candidate, "upward walk"));
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn run_root_subcommand(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("show") | None => match resolve_aethyme_root() {
            Some((path, source)) => {
                println!("{} (via {source})", path.display());
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("aethyme root: not resolved. Fix one of:");
                print_root_guidance();
                ExitCode::from(1)
            }
        },
        Some("set") => {
            let Some(raw) = args.get(1) else {
                eprintln!("usage: aethyme root set <path-to-aethyme-package>");
                return ExitCode::from(2);
            };
            let path = match std::fs::canonicalize(raw) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("aethyme root set: {raw}: {e}");
                    return ExitCode::from(1);
                }
            };
            // Accept either the package dir or the monorepo root.
            let target = if is_aethyme_root(&path) {
                path
            } else {
                let nested = path.join("packages").join("aethyme");
                if is_aethyme_root(&nested) {
                    nested
                } else {
                    eprintln!(
                        "aethyme root set: {} does not contain src/cli.py + pyproject.toml \
                         (nor packages/aethyme/)",
                        path.display()
                    );
                    return ExitCode::from(1);
                }
            };
            let Some(pointer) = config_pointer_path() else {
                eprintln!("aethyme root set: cannot resolve a config directory (no HOME)");
                return ExitCode::from(1);
            };
            if let Some(parent) = pointer.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                eprintln!("aethyme root set: create {}: {e}", parent.display());
                return ExitCode::from(1);
            }
            if let Err(e) = std::fs::write(&pointer, format!("{}\n", target.display())) {
                eprintln!("aethyme root set: write {}: {e}", pointer.display());
                return ExitCode::from(1);
            }
            println!(
                "aethyme root -> {} ({})",
                target.display(),
                pointer.display()
            );
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("aethyme root: unknown action '{other}' (use show|set)");
            ExitCode::from(2)
        }
    }
}

fn print_root_guidance() {
    eprintln!("  - run from inside an Aethyme checkout (auto-discovered), or");
    eprintln!("  - `aethyme root set /path/to/Aethyme` (writes ~/.config/aethyme/root), or");
    eprintln!("  - export AETHYME_ROOT=/path/to/Aethyme/packages/aethyme");
}

// ── delegation to Python ────────────────────────────────────────────────────

fn delegate_to_python(subcommand: &str, args: &[String]) -> ExitCode {
    let Some((aethyme_root, _source)) = resolve_aethyme_root() else {
        eprintln!(
            "aethyme: cannot locate the Aethyme Python package for the \
             '{subcommand}' command."
        );
        print_root_guidance();
        return ExitCode::from(2);
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
