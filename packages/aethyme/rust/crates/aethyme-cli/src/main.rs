//! Aethyme — top-level CLI binary. Entry point for agents using the
//! `aethyme` command.
//!
//! Single-entrypoint design (issue #31, 2026-07-14):
//!   1. `explore` runs **in-process** via the engine library (shared front
//!      end in `aethyme_engine::explore_cli`). When the engine daemon isn't
//!      running for the target repo, the router starts one (detached, via
//!      the sibling `aethyme-engine-cli` serve binary), waits for the
//!      socket, and retries — `aethyme explore` just works.
//!   2. Everything else is native too: `broker`/`certify`/`init`
//!      (aethyme-broker), `enhance`/`repo` UX (aethyme-enhance),
//!      `ai-ready`/`autofix` (aethyme-quality), and the engine groups.
//!      There is no delegation path: unknown subcommands are errors.
//!
//! The python-retirement finished here (Phase 6, 2026-08-01). The router
//! once shelled out to `python -m src.cli` for unflipped command groups
//! and had to *find* that package; `src/` is deleted, so both the
//! delegation and the package-finding are gone.
//!
//! The repo path for explore is found by:
//!   1. `--repo <path>` flag (explicit)
//!   2. `$AETHYME_REPO` env var
//!   3. current directory

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

mod repository_deploy;
mod repository_enrollment;
mod repository_upgrade;

/// Upper bound for waiting on a freshly-spawned engine daemon. The socket
/// binds only after the initial map build (~70s on a 12K-file repo), so
/// this is generous rather than snappy on purpose.
const DAEMON_READY_TIMEOUT: Duration = Duration::from_secs(240);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedCommand<'a> {
    name: &'a str,
    args: &'a [String],
    compatibility_capability: Option<repository_upgrade::CommandCapability>,
    invocation_surface: Option<repository_upgrade::InvocationSurface>,
}

impl<'a> ParsedCommand<'a> {
    fn parse(args: &'a [String]) -> Option<Self> {
        let (name, tail) = args.split_first()?;
        let (compatibility_capability, invocation_surface) = match name.as_str() {
            "broker" => (
                Some(broker_command_capability(tail)),
                Some(broker_invocation_surface(tail)),
            ),
            "upgrade" => (
                Some(repository_upgrade::CommandCapability::Upgrade),
                Some(repository_upgrade::InvocationSurface::UpgradeCommand),
            ),
            // I1 preserves the previous enforcement boundary: repository
            // compatibility gates broker commands, while other top-level
            // commands retain their existing behavior.
            _ => (None, None),
        };
        Some(Self {
            name,
            args: tail,
            compatibility_capability,
            invocation_surface,
        })
    }
}

fn broker_invocation_surface(args: &[String]) -> repository_upgrade::InvocationSurface {
    use repository_upgrade::InvocationSurface;

    match (
        args.first().map(String::as_str),
        args.get(1).map(String::as_str),
    ) {
        (Some("hooks"), Some("pre-commit" | "post-commit")) => InvocationSurface::Hook,
        (Some("git" | "gh" | "operations"), _) => InvocationSurface::CoordinatedOperation,
        _ => InvocationSurface::BrokerCommand,
    }
}

fn broker_command_capability(args: &[String]) -> repository_upgrade::CommandCapability {
    use repository_upgrade::CommandCapability;

    let subcommand = args.first().map(String::as_str);
    let nested = args.get(1).map(String::as_str);
    match (subcommand, nested) {
        (Some("start" | "start-agent"), _) => CommandCapability::NewSession,
        (Some("adopt"), _) => CommandCapability::NewSession,
        (Some("submit" | "exec" | "repair"), _) => CommandCapability::SessionContinuation,
        (Some("hooks"), Some("pre-commit")) => CommandCapability::ManagedPreCommit,
        (Some("hooks"), Some("post-commit")) => CommandCapability::SessionContinuation,
        (Some("leases"), Some("claim" | "release")) => CommandCapability::SessionContinuation,
        (Some("gates"), Some("run" | "pre-push" | "affected" | "semantic")) => {
            CommandCapability::SessionContinuation
        }
        (Some("close" | "finish" | "git" | "gh" | "cleanup"), _) => {
            CommandCapability::RecoveryWrite
        }
        (Some("checkpoint"), Some("apply")) => CommandCapability::RecoveryWrite,
        (Some("report"), Some("file")) => CommandCapability::RecoveryWrite,
        (Some("operations" | "resources"), Some("reconcile"))
        | (Some("advisories"), Some("ack")) => CommandCapability::RecoveryWrite,
        (Some("external-events"), Some("ingest" | "reconcile")) => {
            CommandCapability::SharedMutation
        }
        (Some("review"), Some("register" | "request" | "unlock")) => {
            CommandCapability::SharedMutation
        }
        (Some("integration"), Some("reconcile")) if args.iter().any(|arg| arg == "--apply") => {
            CommandCapability::RecoveryWrite
        }
        (Some("ship"), Some("plan"))
        | (Some("checkpoint"), Some("plan"))
        | (Some("integration"), Some("status" | "reconcile"))
        | (Some("leases"), _)
        | (Some("resources"), Some("plan" | "list"))
        | (Some("gates"), Some("validate"))
        | (Some("report"), Some("capture" | "list" | "show" | "render"))
        | (Some("hooks"), Some("status"))
        | (Some("operations"), _)
        | (Some("advisories"), Some("list" | "show"))
        | (Some("external-events"), Some("list" | "show"))
        | (Some("review"), Some("show"))
        | (Some("handoff" | "queue" | "status" | "agents" | "metrics" | "certify"), _)
        | (Some("events"), _)
            if nested != Some("prune") =>
        {
            CommandCapability::DiagnosticRead
        }
        _ => CommandCapability::SharedMutation,
    }
}

fn eligible_pinned_session_contract(
    cwd: &Path,
    args: &[String],
) -> Option<aethyme_broker::RepositoryContract> {
    let checkout = aethyme_broker::GitRepo::discover(cwd).ok()?;
    let main_root = checkout.main_root().ok()?;
    if !main_root.join(aethyme_broker::BROKER_DB_RELPATH).is_file() {
        return None;
    }
    let mut broker = aethyme_broker::Broker::open_for_compatibility_backfill(cwd).ok()?;
    let explicit_session = args
        .windows(2)
        .find(|pair| pair[0] == "--session")
        .and_then(|pair| pair[1].parse::<i64>().ok());
    let session = if let Some(session_id) = explicit_session {
        broker.store().session(session_id).ok()?
    } else {
        let worktree = checkout.root().to_string_lossy();
        broker.store().session_for_worktree(&worktree).ok()??
    };
    matches!(
        session.status,
        aethyme_broker::SessionStatus::Active
            | aethyme_broker::SessionStatus::Idle
            | aethyme_broker::SessionStatus::Stale
    )
    .then_some(session.repository_contract)
    .flatten()
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(command) = ParsedCommand::parse(&args) else {
        print_top_level_help();
        return ExitCode::from(2);
    };

    let mut broker_compatibility_mode = aethyme_broker::cli::CompatibilityMode::Normal;
    if let Some(capability) = command.compatibility_capability
        && let Ok(cwd) = env::current_dir()
    {
        let surface = command
            .invocation_surface
            .unwrap_or(repository_upgrade::InvocationSurface::BrokerCommand);
        let initial = repository_upgrade::compatibility_decision(
            &cwd,
            capability,
            repository_upgrade::CompatibilityContext {
                surface,
                ..repository_upgrade::CompatibilityContext::default()
            },
        );
        let pinned_contract = initial
            .as_ref()
            .filter(|decision| {
                matches!(
                    capability,
                    repository_upgrade::CommandCapability::SessionContinuation
                        | repository_upgrade::CommandCapability::ManagedPreCommit
                ) && matches!(
                    decision.repository,
                    repository_upgrade::RepositoryCompatibility::UpgradeRequired
                        | repository_upgrade::RepositoryCompatibility::UpgradeInProgress
                )
            })
            .and_then(|_| eligible_pinned_session_contract(&cwd, command.args));
        let decision = if pinned_contract.is_some() {
            repository_upgrade::compatibility_decision(
                &cwd,
                capability,
                repository_upgrade::CompatibilityContext {
                    session_contract: pinned_contract.as_ref(),
                    surface,
                },
            )
        } else {
            initial
        };
        if let Some(decision) = decision {
            if let Some(message) = decision.refusal_message() {
                if let Some(hook_message) = decision.managed_pre_commit_refusal_message() {
                    eprintln!("{hook_message}");
                } else {
                    eprintln!("Error: {message}");
                }
                return ExitCode::from(1);
            }
            if decision.execution == repository_upgrade::CompatibilityExecution::ReadOnlySnapshot {
                broker_compatibility_mode =
                    aethyme_broker::cli::CompatibilityMode::ReadOnlySnapshot;
            }
        }
    }

    match command.name {
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
        // Reader sibling of verify-targets over the SAME saved
        // answer-json (python-retirement Phase 5.5): the compact
        // decision surface deployed skills used to build with a
        // `.venv/bin/python` heredoc.
        "explore-summary" => run_explore_summary(&args[1..]),
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
                match aethyme_enhance::repo_cli::run(&args[1..]) {
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
        // Broker commands have been native Rust from birth (issue #31).
        "broker" => ExitCode::from(aethyme_broker::cli::run_with_mode(
            command.args,
            broker_compatibility_mode,
        )),
        "update" => ExitCode::from(aethyme_broker::run_update_cli(command.args)),
        "upgrade" => ExitCode::from(repository_upgrade::run(command.args)),
        // Certification — top-level by design (the "airport certification"
        // inspection). Strictly read-only; adaptive setup lives in
        // `broker scaffold`.
        "certify" => ExitCode::from(aethyme_broker::cli::run(&args)),
        // Guided setup — certify + scaffold + gates draft in sequence,
        // idempotent. Top-level like certify: it is the first command a
        // new repo runs.
        "init" => ExitCode::from(aethyme_broker::cli::run(&args)),
        "deploy" => ExitCode::from(repository_deploy::run(&args[1..])),
        // Native since python-retirement Phase 2 (the Python `enhance`
        // group is deleted). deploy/verify answer natively; unknown
        // subcommands (and `--help`) get a native error like the other
        // native groups — there is no Python surface to delegate to.
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
        "enhance" => ExitCode::from(aethyme_enhance::cli::run(&args[1..])),
        other => unknown_subcommand(other),
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
    eprintln!("  explore-summary --from explore.json");
    eprintln!("                              compact decision surface from a saved answer-json");
    eprintln!(
        "  verify-targets --repo <path> --from explore.json [--max-targets 2 --max-lines 80]"
    );
    eprintln!("                              bounded source spans for Explore targets");
    eprintln!();
    eprintln!("Agent broker:");
    eprintln!("  init                        guided setup: certify + scaffold + gates draft");
    eprintln!("  certify                     read-only certification checks for this repo");
    eprintln!("  broker start --task <text>  create an isolated worktree + session");
    eprintln!("  broker submit --session <id> simulate, gate, and promote a session");
    eprintln!("  broker status              show sessions, conflicts, queue, and integration");
    eprintln!("  broker finish --session <id> safely close a completed session");
    eprintln!("  broker leases [claim|plan|release] inspect or manage path ownership");
    eprintln!("  broker git|gh --session <id> coordinate Git and GitHub operations");
    eprintln!("  broker operations          inspect/reconcile the remote-operation journal");
    eprintln!("  broker adopt|start-agent|agents|cleanup   (see `broker --help`)");
    eprintln!("  update check|plan|execute  explicit paired-binary updates; never background");
    eprintln!("  upgrade plan|apply|recover review, apply, or recover repository migrations");
    eprintln!();
    eprintln!("Setup:");
    eprintln!("  deploy [verify|bridge] [--repo <path>]  enroll repository policy");
    eprintln!("  root show|set <path>        developer checkout pointer (legacy compatibility)");
    eprintln!();
    eprintln!("Quality:");
    eprintln!("  ai-ready [--repo <path>]    AI-readiness scorecard");
    eprintln!("  autofix <path> [--dry-run|--apply|--pr]");
    eprintln!("                              safe automated fixes (see `autofix --help`)");
    eprintln!();
    // Phase 6 deleted `src/` and the delegation path with it.
    eprintln!("Every subcommand is native; unknown ones are errors.");
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

fn run_explore_summary(args: &[String]) -> ExitCode {
    use aethyme_enhance::explore_summary_cli::{ExploreSummaryCliOutcome, run};
    match run(args) {
        ExploreSummaryCliOutcome::Done => ExitCode::SUCCESS,
        ExploreSummaryCliOutcome::BadUsage(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
        ExploreSummaryCliOutcome::Failed(message) => {
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

// ── aethyme root — legacy developer checkout pointer ────────────────────────
//
// Repository deployment is self-contained and does not consult this pointer.
// The command remains for developer compatibility and explicit inspection of
// source checkouts configured by older installations.
//
// Resolution order:
//   1. $AETHYME_ROOT (explicit override, highest priority)
//   2. pointer file: $XDG_CONFIG_HOME/aethyme/root (default ~/.config/aethyme/root)
//   3. upward walk from the current directory (covers working inside the
//      Aethyme repo or any of its worktrees with zero configuration)
//
// Existing pointer files remain readable so upgrades do not destroy operator
// configuration.

fn config_pointer_path() -> Option<PathBuf> {
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("aethyme").join("root"))
}

/// A directory qualifies as an Aethyme developer checkout when it holds the
/// Rust workspace and canonical skill sources.
fn is_aethyme_root(dir: &Path) -> bool {
    dir.join("rust").join("Cargo.toml").is_file() && dir.join("skills").join("aethyme").is_dir()
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
                        "aethyme root set: {} does not contain rust/Cargo.toml + skills/aethyme \
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

/// Unknown subcommand. Until python-retirement Phase 6 this fell through
/// to `python -m src.cli <subcommand>`; `src/` is deleted, so an unknown
/// name is simply an error.
fn unknown_subcommand(subcommand: &str) -> ExitCode {
    eprintln!("aethyme: unknown subcommand '{subcommand}'");
    eprintln!("Run `aethyme --help` for the command list.");
    ExitCode::from(2)
}

#[cfg(test)]
mod compatibility_command_tests {
    use super::{
        ParsedCommand,
        repository_upgrade::{CommandCapability, InvocationSurface},
    };

    fn capability(args: &[&str]) -> Option<CommandCapability> {
        let args = args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>();
        ParsedCommand::parse(&args)
            .unwrap()
            .compatibility_capability
    }

    fn surface(args: &[&str]) -> Option<InvocationSurface> {
        let args = args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>();
        ParsedCommand::parse(&args).unwrap().invocation_surface
    }

    #[test]
    fn parsed_commands_cover_every_compatibility_capability() {
        let cases = [
            (&["broker", "status"][..], CommandCapability::DiagnosticRead),
            (
                &["broker", "integration", "reconcile", "--apply"][..],
                CommandCapability::RecoveryWrite,
            ),
            (
                &["broker", "checkpoint", "plan", "--session", "7"][..],
                CommandCapability::DiagnosticRead,
            ),
            (
                &["broker", "checkpoint", "apply", "--session", "7"][..],
                CommandCapability::RecoveryWrite,
            ),
            (
                &["broker", "submit", "--session", "7"][..],
                CommandCapability::SessionContinuation,
            ),
            (
                &["broker", "hooks", "pre-commit"][..],
                CommandCapability::ManagedPreCommit,
            ),
            (
                &["broker", "start", "--task", "work"][..],
                CommandCapability::NewSession,
            ),
            (
                &["broker", "ship", "execute"][..],
                CommandCapability::SharedMutation,
            ),
            (&["upgrade", "plan"][..], CommandCapability::Upgrade),
        ];

        for (args, expected) in cases {
            assert_eq!(
                capability(args),
                Some(expected),
                "unexpected capability for {args:?}"
            );
        }
    }

    #[test]
    fn compatibility_is_scoped_to_the_existing_broker_boundary() {
        assert_eq!(
            capability(&["broker", "adopt", "--reuse"]),
            Some(CommandCapability::NewSession)
        );
        assert_eq!(
            capability(&["broker", "adopt"]),
            Some(CommandCapability::NewSession)
        );
        assert_eq!(capability(&["explore"]), None);
    }

    #[test]
    fn invoking_surface_is_identified_before_compatibility_rendering() {
        for (args, expected) in [
            (
                &["broker", "hooks", "pre-commit"][..],
                InvocationSurface::Hook,
            ),
            (&["broker", "status"][..], InvocationSurface::BrokerCommand),
            (&["upgrade", "plan"][..], InvocationSurface::UpgradeCommand),
            (
                &["broker", "gh", "--session", "7"][..],
                InvocationSurface::CoordinatedOperation,
            ),
            (
                &["broker", "operations"][..],
                InvocationSurface::CoordinatedOperation,
            ),
        ] {
            assert_eq!(
                surface(args),
                Some(expected),
                "unexpected surface: {args:?}"
            );
        }
    }

    #[test]
    fn degraded_repository_lanes_match_command_semantics() {
        let cases = [
            (
                &["broker", "integration", "reconcile", "--dry-run"][..],
                CommandCapability::DiagnosticRead,
            ),
            (
                &["broker", "integration", "reconcile", "--apply"][..],
                CommandCapability::RecoveryWrite,
            ),
            (
                &["broker", "report", "capture"][..],
                CommandCapability::DiagnosticRead,
            ),
            (
                &["broker", "report", "file"][..],
                CommandCapability::RecoveryWrite,
            ),
            (
                &["broker", "git", "--session", "7"][..],
                CommandCapability::RecoveryWrite,
            ),
            (
                &["broker", "finish", "--session", "7"][..],
                CommandCapability::RecoveryWrite,
            ),
            (
                &["broker", "close", "--session", "7"][..],
                CommandCapability::RecoveryWrite,
            ),
            (
                &["broker", "hooks", "pre-commit"][..],
                CommandCapability::ManagedPreCommit,
            ),
            (
                &["broker", "integration", "status"][..],
                CommandCapability::DiagnosticRead,
            ),
            (
                &["broker", "advisories", "list"][..],
                CommandCapability::DiagnosticRead,
            ),
            (
                &["broker", "advisories", "show", "7"][..],
                CommandCapability::DiagnosticRead,
            ),
            (
                &["broker", "advisories", "ack", "7"][..],
                CommandCapability::RecoveryWrite,
            ),
        ];
        for (args, expected) in cases {
            assert_eq!(
                capability(args),
                Some(expected),
                "unexpected lane: {args:?}"
            );
        }
    }
}
