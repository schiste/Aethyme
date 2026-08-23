//! CLI for `aethyme broker ...` — a thin client of [`crate::Broker`].
//!
//! Owned by the broker crate (not the router binary) so the command
//! surface and the library evolve together; the `aethyme` router just
//! dispatches here. Contract: no logic beyond argument parsing and
//! rendering, and every command has a `--json` form whose shape comes
//! from the library's serializable types (#32).

use std::path::PathBuf;

use crate::broker::Broker;

const USAGE: &str = "\
aethyme broker — coordinate concurrent AI agent sessions on this repository

Usage:
  aethyme init [--json]                (also: aethyme broker init)
      Guided setup: certify (read-only), then scaffold (deterministic,
      only-if-missing), then gates draft (adaptive, only when no
      gates.toml exists) — the three commands below in sequence, then a
      summary of what already existed vs what was created. Idempotent:
      a second run on the same repo changes nothing and says so.
  aethyme certify [--json]             (also: aethyme broker certify)
      The certification method: deterministic, strictly read-only checks
      (git version, repo, configs valid, gitignore contract, protocol,
      db integrity). Exits non-zero on failures — CI/cron-able as the
      recurring inspection. Never writes anything.
  aethyme broker scaffold [--json]
      Deterministic setup: ONLY what the broker needs, with content that
      is identical for every repo (config.toml skeleton, .gitignore
      block, broker database). Never overwrites. Certify + scaffold are
      the 'always exactly the same' pair — one reads, one writes.
  aethyme broker gates draft [--json]
      Adaptive (NOT scaffolding): sniff this repo's manifests and draft
      a gates.toml. Output depends on the repo — review it, then run
      certify.
  aethyme broker adopt [<path>] [--task <text>] [--reuse|--replace-stale] [--json]
      Register an existing worktree (attach-first). Defaults to the
      current directory. If the worktree already has a session:
      --reuse points it at a follow-up task (fresh baseline);
      --replace-stale closes it (state only) and registers fresh;
      neither flag = error listing your options.
  aethyme broker close --session <id> [--json]
      Low-level state-only close. Never touches the worktree and does
      not check whether commits were submitted. Prefer finish for normal
      lifecycle use.
  aethyme broker finish --session <id> [--json]
      Higher-level lifecycle close: closes only when the session has no
      dirty WIP and no committed work waiting for submit/promotion. If it
      is not safe, prints the next command; suggests cleanup only when
      cleanup would pass without --force.
  aethyme broker start --task <text> [--json]
      Create a broker-managed worktree + branch and register a session,
      but do not spawn a process. Prefer this over adopting the main
      checkout for agent work; it isolates the git index and worktree.
  aethyme broker start-agent --task <text> --cmd <command> [--json]
      Create a worktree + branch and spawn <command> in it (sh -c),
      logging to .aethyme/logs/.
  aethyme broker agents [--json]
      List live sessions with activity-derived liveness, refreshing
      diff-derived leases and warning on overlapping edits.
  aethyme broker leases [--json]
      Refresh and list active leases plus current overlaps.
  aethyme broker leases claim <path> --session <id> [--ttl <seconds>] [--json]
      Explicitly claim a path (end it with / for a directory claim).
  aethyme broker leases release <path> --session <id> [--json]
      Release an explicit claim.
  aethyme broker exec --session <id> -- <command> [--json]
      Run a command in the session worktree, then fail if it leaves new
      dirty paths outside explicit leases or in adoption-time foreign
      files. Exports AETHYME_TEST_DB_SUFFIX=s<id>-exec.
  aethyme broker git --session <id> [--repo <owner/name>] [--scope <scope>] [--effect <read|write|destructive>] [--reason <text>] [--destructive] [--json] -- <git-args>
      Run Git through the durable operation coordinator. Remote Git commands
      require an exact --repo. Repository writes are serialized, journaled,
      and fail closed after a crash with an unknown remote outcome.
  aethyme broker gh --session <id> --repo <owner/name> [--scope <scope>] [--effect <read|write|destructive>] [--reason <text>] [--destructive] [--json] -- <gh-args>
      Run GitHub CLI through the same repository coordinator. The broker sets
      GH_REPO from the exact target and never persists command output or
      secret-bearing argument values.
  aethyme broker operations [--json]
      List the durable coordinated-operation journal.
  aethyme broker operations reconcile --operation <id> --outcome <succeeded|failed> --reason <text> [--json]
      Resolve a crash-ambiguous operation after independently inspecting the
      remote state. Overlapping writes remain blocked until reconciliation.
  aethyme broker gates validate [--json]
      Parse and validate .aethyme/gates.toml.
  aethyme broker gates affected --session <id> [--json]
      Show which gates the session's diff selects and why.
  aethyme broker gates semantic --session <id> [--json]
      Advisory semantic gate-selection report: shows enforced path-triggered
      gates plus caller-edge suggestion status. Never changes what submit,
      CI, or gates run execute.
  aethyme broker gates run --session <id> [--json]
      Run affected gates cheap-first with tree-hash caching; cancels this
      session's obsolete in-flight runs; stops at first failure.
  aethyme broker gates run --all [--json]
      Run EVERY gate in cost order against the current worktree — no
      diff selection, no session. Same runner, streaming, and tree-hash
      result cache as session runs; stops at first failure and exits
      non-zero if any gate does not pass. The CI entrypoint: gates.toml
      is the single definition of verified for CI and broker alike.
  aethyme broker hooks install [--json]
      Explicitly install the two managed git hooks into the shared
      <git-common-dir>/hooks (all worktrees see them): pre-commit runs
      the cost<=1 gates whose triggers match the staged files (failure
      blocks the commit); post-commit warns when the new commit touches
      files another live session is editing (informational — never
      blocks). Refuses to touch a hook file it does not own (no aethyme
      marker); with the marker, only the marker block is replaced. The
      hook shims embed this binary's absolute path.
  aethyme broker hooks uninstall [--json]
      Remove the aethyme marker blocks, deleting a hook file only when
      nothing but the shim remained. User content is preserved.
  aethyme broker hooks status [--json]
      Report installed/absent/foreign per managed hook.
      (hooks pre-commit / hooks post-commit are the internal entry
      points the installed shims call — not for direct use.)
  aethyme broker pr check [--target <branch>] [--pr <number>] [--agent <name>] [--dispatch] [--cmd <command>] [--json]
      Inspect the open PR for the current branch targeting <branch>
      (default: production). A thumbs-up marker in the PR body means all
      good and skips activity checks. A looking-eyes marker or no marker
      checks comments, reviews, and status checks; new actionable
      activity prepares a Push2prod prompt. With --dispatch, the broker
      attaches that prompt to an existing matching session when possible
      or spawns a Codex agent command.
  aethyme broker submit --session <id> [--json]
      Submit the session's head commit: simulate the merge onto the local
      integration branch, run affected gates on the merged tree, and
      promote when verified (default; set [promote] mode = 'manual' to
      hold verified entries for explicit promote). Conflicts reject
      before any gate runs and write instructions to
      <worktree>/.aethyme/broker-action-required.md. V1 submits the
      whole session head only; --path/--commit scoping is intentionally
      out of scope while worktree identity is the coordination unit.
  aethyme broker repair --session <id> [--json]
      One-command recovery for a blocked session: apply the documented
      local rebase path for the latest submit conflict, or rebase onto
      promoted integration work when status reports that conflict surface.
      Then refresh leases and show affected gates. Never submits or
      promotes; run submit when the report is clean.
  aethyme broker queue [--json]
      Show the merge queue.
  aethyme broker promote --entry <id> [--json]
      Manual-mode only: advance the local integration branch to a verified
      entry's merge commit; other in-flight entries are re-simulated.
      Never pushes.
  aethyme broker ship plan --entry <id> [--json]
      Read-only publication plan for a promoted entry: resolve the exact
      integration tip, local and remote default-branch SHAs, freshness,
      proposed push, and whether synchronizing local main is currently safe.
  aethyme broker integration status [--json]
      Focused promoted-but-unmerged view: the local integration branch as
      a pending layer above main, with promoted entries, files changed,
      live sessions conflicting with that layer, and the next action.
  aethyme broker integration wait-stable [--seconds <n>] [--json]
      Sample integration, wait for a quiet window (default: 30s), then
      sample again. Fails if integration moved, printing the old and new
      tips so long checks are not mistaken for current-tip proof.
  aethyme broker integration reconcile --upstream <ref> [--resolution-file <path>] [--dry-run|--apply] [--json]
      Compare already-fetched upstream with local main and promoted queue
      state. Dry-run is the default. --apply marks externally landed work,
      preserves pending promotions, and rebuilds integration. A resolution
      file may attest specific promoted entries as superseded upstream.
  aethyme broker status [--json]
      The whole picture: agents, overlaps, promoted conflicts, merge
      queue, integration head.
  aethyme broker events [--since <id>] [--kind <prefix>] [--follow] [--json]
      Show the append-only event log (see docs/events-contract.md).
      --kind filters by prefix (e.g. merge. or lease.overlap); --follow
      polls for new events and survives transient read errors.
  aethyme broker events prune --keep-days <n> [--json]
      Retention: delete events older than <n> days. Event ids stay
      strictly increasing, so existing --since cursors remain valid.
  aethyme broker metrics [--json]
      Cost/benefit accounting from safe local telemetry: broker command
      latency (names + numbers only, never task text or paths), gate
      executions vs cache hits with time saved, conflicts caught before
      any gate ran, overlaps warned.
  aethyme broker doctor [--fix-version] [--json]
      Health checks: database integrity, sessions whose worktree is
      gone, orphaned gate pidfiles, and stale local CLI builds when run
      inside the Aethyme source checkout. --fix-version is explicit and
      source-checkout-only: when the running CLI is behind integration,
      reinstall it from a temporary integration worktree.
  aethyme broker quick-test [--chau7] [--with-gate] [--json]
      Disposable first-run smoke: creates a temporary git repo, runs init,
      adopt, commit, submit, verifies promotion, and removes the repo.
      --chau7 requires a Chau7 runtime marker; outside Chau7 it reports
      that this test is designed to run in Chau7 and skips the smoke.
      --with-gate installs a passing fixture gate and then proves a
      failing variant is rejected without promotion.
  aethyme broker verify-loop [--json]   (alias: e2e)
      End-to-end broker verification for operators: snapshot integration,
      run quick-test, run doctor, run broker source tests when this is an
      Aethyme source checkout, then fail if integration moved during the
      run so the result cannot be mistaken for current-tip proof.
  aethyme broker cleanup <session-id> [--force] [--json]
      Remove a session's worktree. Refuses on uncommitted changes or
      unmerged commits unless --force. Usually run only after finish says
      cleanup is safe.
  aethyme broker check-contract [--base <ref>] [--pr-body <file>]
      Cross-process contract gate: refuse a diff that removes symbols
      listed in the consumers registry unless the PR body or commit
      messages declare a contract decision. Run by CI and by the
      `cross-process-contract` gate. Exit 1 = undeclared contract change.

Overlaps warn — they never block (v0 policy).
";

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Entry point for the router. Returns a process exit code.
pub fn run(args: &[String]) -> u8 {
    // Dispatched before the shared parser: the contract check is a CI/gate
    // entry point with its own flags (`--base`, `--pr-body`) and its own
    // exit-code contract (2 = bad invocation), and it deliberately records
    // no command metric — it runs on every submit and would swamp the
    // ledger with noise.
    if args.first().map(String::as_str) == Some("check-contract") {
        return crate::contract_check::run(&args[1..]);
    }
    let started = std::time::Instant::now();
    let code = match run_inner(args) {
        Ok(()) => 0,
        Err(UsageError::Help) => {
            eprint!("{USAGE}");
            2
        }
        Err(UsageError::Message(message)) => {
            eprintln!("Error: {message}");
            1
        }
    };
    record_command_metric(args, code, started.elapsed().as_millis() as i64);
    code
}

/// Safe-by-construction command telemetry: the label is built ONLY from
/// an allowlist of known subcommand words, so positional values (paths,
/// session ids, task text) can never leak into the metrics file. Best
/// effort — any failure is silently ignored.
fn record_command_metric(args: &[String], exit: u8, duration_ms: i64) {
    if !command_records_metric(args) {
        return;
    }
    const KNOWN: &[&str] = &[
        "adopt",
        "start",
        "start-agent",
        "exec",
        "git",
        "gh",
        "operations",
        "reconcile",
        "agents",
        "leases",
        "claim",
        "release",
        "gates",
        "draft",
        "validate",
        "affected",
        "semantic",
        "run",
        "hooks",
        "install",
        "uninstall",
        "pre-commit",
        "post-commit",
        "pr",
        "check",
        "submit",
        "repair",
        "queue",
        "promote",
        "ship",
        "plan",
        "integration",
        "status",
        "events",
        "prune",
        "metrics",
        "doctor",
        "quick-test",
        "verify-loop",
        "e2e",
        "finish",
        "cleanup",
        "certify",
        "scaffold",
        "init",
    ];
    let label: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| KNOWN.contains(a))
        .take(2)
        .collect();
    if label.is_empty() {
        return; // nothing recognizable
    }
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    let Ok(repo) = crate::GitRepo::discover(&cwd) else {
        return;
    };
    let Ok(main_root) = repo.main_root() else {
        return;
    };
    let dir = main_root.join(".aethyme/logs");
    let _ = std::fs::create_dir_all(&dir);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let line = format!(
        "{{\"ts\":{ts},\"command\":\"{}\",\"duration_ms\":{duration_ms},\"exit\":{exit}}}\n",
        label.join(".")
    );
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("command-metrics.jsonl"))
    {
        let _ = file.write_all(line.as_bytes());
    }
}

/// Whether this invocation should contribute command-latency telemetry.
/// Report-only commands stay telemetry-free; variants that mutate broker,
/// repository, or installation state remain observable.
fn command_records_metric(args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        Some("certify" | "queue" | "metrics") => false,
        Some("ship") => args.get(1).map(String::as_str) != Some("plan"),
        Some("operations") => args.get(1).map(String::as_str) == Some("reconcile"),
        Some("git" | "gh") => {
            let command = args
                .iter()
                .position(|arg| arg == "--")
                .map(|index| &args[index + 1..])
                .unwrap_or(&[]);
            let effect = if args.first().map(String::as_str) == Some("git") {
                crate::classify_git(command)
            } else {
                crate::classify_gh(command)
            };
            effect != Some(crate::OperationEffect::Read)
        }
        Some("hooks") => args.get(1).map(String::as_str) != Some("status"),
        Some("events") => args.get(1).map(String::as_str) == Some("prune"),
        Some("gates") => !matches!(
            args.get(1).map(String::as_str),
            Some("validate" | "affected" | "semantic")
        ),
        Some("doctor") => args.iter().any(|arg| arg == "--fix-version"),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_string()).collect()
    }

    #[test]
    fn telemetry_classification_tracks_semantic_mutability() {
        for command in [
            args(&["certify"]),
            args(&["hooks", "status"]),
            args(&["queue"]),
            args(&["events"]),
            args(&["events", "--follow"]),
            args(&["metrics"]),
            args(&["gates", "validate"]),
            args(&["gates", "affected", "--session", "7"]),
            args(&["gates", "semantic", "--session", "7"]),
            args(&["doctor"]),
            args(&["operations"]),
            args(&["git", "--session", "7", "--", "status"]),
            args(&[
                "gh",
                "--session",
                "7",
                "--repo",
                "o/r",
                "--",
                "pr",
                "view",
                "1",
            ]),
        ] {
            assert!(
                !super::command_records_metric(&command),
                "read-only reporter should not record telemetry: {command:?}"
            );
        }

        for command in [
            args(&["hooks", "install"]),
            args(&["events", "prune", "--keep-days", "7"]),
            args(&["gates", "run", "--session", "7"]),
            args(&["doctor", "--fix-version"]),
            args(&["status"]),
            args(&["agents"]),
            args(&["leases"]),
            args(&["integration", "status"]),
            args(&["operations", "reconcile", "--operation", "1"]),
            args(&["git", "--session", "7", "--", "push"]),
            args(&[
                "gh",
                "--session",
                "7",
                "--repo",
                "o/r",
                "--",
                "pr",
                "merge",
                "1",
            ]),
        ] {
            assert!(
                super::command_records_metric(&command),
                "stateful command should record telemetry: {command:?}"
            );
        }
    }

    #[test]
    fn parse_accepts_read_only_ship_plan() {
        let parsed = match super::parse(&args(&["ship", "plan", "--entry", "42", "--json"])) {
            Ok(parsed) => parsed,
            Err(_) => panic!("ship plan should parse"),
        };
        assert_eq!(parsed.positional, vec!["ship", "plan"]);
        assert_eq!(parsed.entry, Some(42));
        assert!(parsed.json);
        assert!(!super::command_records_metric(&args(&[
            "ship", "plan", "--entry", "42"
        ])));
    }

    #[test]
    fn init_next_steps_names_quick_test_before_start_and_submit() {
        let message = super::init_next_steps_message();
        let init = message.find("aethyme init").unwrap();
        let quick_test = message.find("aethyme broker quick-test").unwrap();
        let start = message.find("aethyme broker start").unwrap();
        let submit = message
            .find("aethyme broker submit --session <id>")
            .unwrap();
        assert!(init < quick_test);
        assert!(quick_test < start);
        assert!(start < submit);
    }

    #[test]
    fn parse_accepts_explicit_doctor_version_fix() {
        let args = vec!["doctor".to_string(), "--fix-version".to_string()];
        let parsed = match super::parse(&args) {
            Ok(parsed) => parsed,
            Err(_) => panic!("doctor --fix-version should parse"),
        };

        assert_eq!(parsed.positional, vec!["doctor"]);
        assert!(parsed.fix_version);
    }

    #[test]
    fn parse_accepts_quick_test_with_gate() {
        let args = vec!["quick-test".to_string(), "--with-gate".to_string()];
        let parsed = match super::parse(&args) {
            Ok(parsed) => parsed,
            Err(_) => panic!("quick-test --with-gate should parse"),
        };

        assert_eq!(parsed.positional, vec!["quick-test"]);
        assert!(parsed.with_gate);
    }

    #[test]
    fn parse_accepts_integration_wait_stable_seconds() {
        let args = vec![
            "integration".to_string(),
            "wait-stable".to_string(),
            "--seconds".to_string(),
            "30".to_string(),
        ];
        let parsed = match super::parse(&args) {
            Ok(parsed) => parsed,
            Err(_) => panic!("integration wait-stable --seconds should parse"),
        };

        assert_eq!(parsed.positional, vec!["integration", "wait-stable"]);
        assert_eq!(parsed.seconds, Some(30));
    }

    #[test]
    fn parse_accepts_integration_reconcile_options() {
        let args = vec![
            "integration".to_string(),
            "reconcile".to_string(),
            "--upstream".to_string(),
            "origin/main".to_string(),
            "--resolution-file".to_string(),
            "reconciliation.json".to_string(),
            "--apply".to_string(),
        ];
        let parsed = match super::parse(&args) {
            Ok(parsed) => parsed,
            Err(_) => panic!("integration reconcile options should parse"),
        };

        assert_eq!(parsed.upstream.as_deref(), Some("origin/main"));
        assert_eq!(
            parsed.resolution_file.as_deref(),
            Some(std::path::Path::new("reconciliation.json"))
        );
        assert!(parsed.apply);
    }

    #[test]
    fn parse_accepts_guarded_exec_command_after_separator() {
        let args = vec![
            "exec".to_string(),
            "--session".to_string(),
            "7".to_string(),
            "--".to_string(),
            "cargo".to_string(),
            "fmt".to_string(),
            "--check".to_string(),
        ];
        let parsed = match super::parse(&args) {
            Ok(parsed) => parsed,
            Err(_) => panic!("exec with -- separator should parse"),
        };

        assert_eq!(parsed.positional, vec!["exec"]);
        assert_eq!(parsed.session, Some(7));
        assert_eq!(parsed.exec_command, vec!["cargo", "fmt", "--check"]);
    }

    #[test]
    fn parse_accepts_coordinated_github_operation() {
        let args = vec![
            "--session".to_string(),
            "7".to_string(),
            "--repo".to_string(),
            "owner/repo".to_string(),
            "--scope".to_string(),
            "pull-request:42".to_string(),
            "--effect".to_string(),
            "write".to_string(),
            "--reason".to_string(),
            "reviewed release workflow".to_string(),
            "--".to_string(),
            "pr".to_string(),
            "merge".to_string(),
            "42".to_string(),
        ];
        let parsed = match super::parse(&args) {
            Ok(parsed) => parsed,
            Err(_) => panic!("coordinated gh operation should parse"),
        };

        assert_eq!(parsed.session, Some(7));
        assert_eq!(parsed.repository.as_deref(), Some("owner/repo"));
        assert_eq!(parsed.scope.as_deref(), Some("pull-request:42"));
        assert_eq!(parsed.effect.as_deref(), Some("write"));
        assert_eq!(parsed.reason.as_deref(), Some("reviewed release workflow"));
        assert_eq!(parsed.exec_command, vec!["pr", "merge", "42"]);
    }

    #[test]
    fn parse_accepts_pr_check_routing_flags() {
        let args = vec![
            "check".to_string(),
            "--target".to_string(),
            "production".to_string(),
            "--pr".to_string(),
            "42".to_string(),
            "--agent".to_string(),
            "Push2prod".to_string(),
            "--dispatch".to_string(),
            "--cmd".to_string(),
            "codex exec prompt".to_string(),
        ];
        let parsed = match super::parse(&args) {
            Ok(parsed) => parsed,
            Err(_) => panic!("pr check flags should parse"),
        };

        assert_eq!(parsed.positional, vec!["check"]);
        assert_eq!(parsed.target.as_deref(), Some("production"));
        assert_eq!(parsed.pr_number, Some(42));
        assert_eq!(parsed.agent.as_deref(), Some("Push2prod"));
        assert!(parsed.dispatch);
        assert_eq!(parsed.cmd.as_deref(), Some("codex exec prompt"));
    }
}

enum UsageError {
    Help,
    Message(String),
}

impl<E: std::fmt::Display> From<E> for UsageError {
    fn from(err: E) -> Self {
        UsageError::Message(err.to_string())
    }
}

struct Parsed {
    positional: Vec<String>,
    task: Option<String>,
    cmd: Option<String>,
    target: Option<String>,
    repository: Option<String>,
    scope: Option<String>,
    effect: Option<String>,
    outcome: Option<String>,
    reason: Option<String>,
    agent: Option<String>,
    pr_number: Option<i64>,
    session: Option<i64>,
    entry: Option<i64>,
    operation: Option<i64>,
    ttl_seconds: Option<i64>,
    since: Option<i64>,
    kind: Option<String>,
    keep_days: Option<i64>,
    seconds: Option<u64>,
    upstream: Option<String>,
    resolution_file: Option<PathBuf>,
    follow: bool,
    json: bool,
    force: bool,
    check: bool,
    dispatch: bool,
    reuse: bool,
    replace_stale: bool,
    all: bool,
    chau7: bool,
    fix_version: bool,
    with_gate: bool,
    apply: bool,
    dry_run: bool,
    destructive: bool,
    exec_command: Vec<String>,
}

fn parse(args: &[String]) -> Result<Parsed, UsageError> {
    let mut parsed = Parsed {
        positional: Vec::new(),
        task: None,
        cmd: None,
        target: None,
        repository: None,
        scope: None,
        effect: None,
        outcome: None,
        reason: None,
        agent: None,
        pr_number: None,
        session: None,
        entry: None,
        operation: None,
        ttl_seconds: None,
        since: None,
        kind: None,
        keep_days: None,
        seconds: None,
        upstream: None,
        resolution_file: None,
        follow: false,
        json: false,
        force: false,
        check: false,
        dispatch: false,
        reuse: false,
        replace_stale: false,
        all: false,
        chau7: false,
        fix_version: false,
        with_gate: false,
        apply: false,
        dry_run: false,
        destructive: false,
        exec_command: Vec::new(),
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--" => {
                parsed.exec_command = iter.cloned().collect();
                break;
            }
            "--json" => parsed.json = true,
            "--follow" => parsed.follow = true,
            "--since" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--since requires a value".into()))?;
                parsed.since = Some(value.parse().map_err(|_| {
                    UsageError::Message("--since must be an integer event id".into())
                })?);
            }
            "--force" => parsed.force = true,
            "--check" => parsed.check = true,
            "--dispatch" => parsed.dispatch = true,
            "--reuse" => parsed.reuse = true,
            "--all" => parsed.all = true,
            "--chau7" => parsed.chau7 = true,
            "--fix-version" => parsed.fix_version = true,
            "--with-gate" => parsed.with_gate = true,
            "--apply" => parsed.apply = true,
            "--dry-run" => parsed.dry_run = true,
            "--destructive" => parsed.destructive = true,
            "--replace-stale" => parsed.replace_stale = true,
            "--kind" => {
                parsed.kind = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--kind requires a value".into()))?
                        .clone(),
                )
            }
            "--keep-days" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--keep-days requires a value".into()))?;
                parsed.keep_days =
                    Some(value.parse().map_err(|_| {
                        UsageError::Message("--keep-days must be an integer".into())
                    })?);
            }
            "--seconds" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--seconds requires a value".into()))?;
                parsed.seconds = Some(value.parse().map_err(|_| {
                    UsageError::Message("--seconds must be a non-negative integer".into())
                })?);
            }
            "--upstream" => {
                parsed.upstream = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--upstream requires a ref".into()))?
                        .clone(),
                )
            }
            "--resolution-file" => {
                parsed.resolution_file = Some(PathBuf::from(
                    iter.next()
                        .ok_or(UsageError::Message(
                            "--resolution-file requires a path".into(),
                        ))?
                        .clone(),
                ))
            }
            "--task" => {
                parsed.task = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--task requires a value".into()))?
                        .clone(),
                )
            }
            "--cmd" => {
                parsed.cmd = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--cmd requires a value".into()))?
                        .clone(),
                )
            }
            "--target" => {
                parsed.target = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--target requires a value".into()))?
                        .clone(),
                )
            }
            "--repo" => {
                parsed.repository = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--repo requires owner/name".into()))?
                        .clone(),
                )
            }
            "--scope" => {
                parsed.scope = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--scope requires a value".into()))?
                        .clone(),
                )
            }
            "--effect" => {
                parsed.effect = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--effect requires a value".into()))?
                        .clone(),
                )
            }
            "--outcome" => {
                parsed.outcome = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--outcome requires a value".into()))?
                        .clone(),
                )
            }
            "--reason" => {
                parsed.reason = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--reason requires a value".into()))?
                        .clone(),
                )
            }
            "--agent" => {
                parsed.agent = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--agent requires a value".into()))?
                        .clone(),
                )
            }
            "--pr" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--pr requires a value".into()))?;
                parsed.pr_number = Some(value.parse().map_err(|_| {
                    UsageError::Message("--pr must be an integer PR number".into())
                })?);
            }
            "--session" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--session requires a value".into()))?;
                parsed.session = Some(value.parse().map_err(|_| {
                    UsageError::Message("--session must be an integer session id".into())
                })?);
            }
            "--entry" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--entry requires a value".into()))?;
                parsed.entry = Some(value.parse().map_err(|_| {
                    UsageError::Message("--entry must be an integer queue entry id".into())
                })?);
            }
            "--operation" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--operation requires a value".into()))?;
                parsed.operation = Some(value.parse().map_err(|_| {
                    UsageError::Message("--operation must be an integer operation id".into())
                })?);
            }
            "--ttl" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--ttl requires a value".into()))?;
                parsed.ttl_seconds = Some(value.parse().map_err(|_| {
                    UsageError::Message("--ttl must be an integer number of seconds".into())
                })?);
            }
            "-h" | "--help" => return Err(UsageError::Help),
            other if other.starts_with('-') => {
                return Err(UsageError::Message(format!("unknown flag {other}")));
            }
            positional => parsed.positional.push(positional.to_string()),
        }
    }
    Ok(parsed)
}

fn aethyme_gates_load(main_root: &std::path::Path) -> Result<Vec<crate::Gate>, UsageError> {
    Ok(crate::load_gates(main_root)?)
}

fn print_checks(checks: &[crate::init::Check]) {
    for check in checks {
        let tag = match check.status {
            crate::init::CheckStatus::Pass => "pass",
            crate::init::CheckStatus::Created => "created",
            crate::init::CheckStatus::Warn => "warn",
            crate::init::CheckStatus::Fail => "FAIL",
            crate::init::CheckStatus::Skipped => "skip",
        };
        println!("{tag:<8} {:<28} {}", check.id, check.detail);
    }
}

fn duration_label(duration_ms: Option<i64>) -> String {
    duration_ms
        .map(|ms| format!("{ms}ms"))
        .unwrap_or_else(|| "-".into())
}

fn gate_status_label(
    status: crate::GateStatus,
    failure_class: Option<crate::GateFailureClass>,
) -> String {
    match failure_class {
        Some(class) => format!("{}/{}", status.as_str(), class.as_str()),
        None => status.as_str().to_string(),
    }
}

fn render_hook_reports(reports: &[crate::HookReport], json: bool) -> Result<(), UsageError> {
    if json {
        println!("{}", serde_json::to_string_pretty(reports)?);
    } else {
        for report in reports {
            println!(
                "{:<12} {:<10} {}",
                report.hook,
                report.state.as_str(),
                report.path
            );
        }
    }
    Ok(())
}

fn render_pr_check_report(report: &crate::PrCheckReport, json: bool) -> Result<(), UsageError> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }

    match &report.pr {
        Some(pr) => {
            println!(
                "PR #{} -> {}: {}",
                pr.number, report.target_branch, pr.title
            );
            if let Some(url) = &pr.url {
                println!("URL: {url}");
            }
        }
        None => {
            println!("{}", report.decision.summary);
        }
    }
    println!("Marker: {}", report.marker.as_str());
    println!(
        "Activity: {}{}",
        if report.checked_activity {
            "checked"
        } else {
            "skipped"
        },
        if report.checked_activity {
            format!(
                " (new: {}, comments: {}, reviews: {}, failing checks: {})",
                if report.new_activity { "yes" } else { "no" },
                report.comments.len(),
                report.reviews.len(),
                report.failing_checks.len()
            )
        } else {
            String::new()
        }
    );
    println!("Decision: {}", report.decision.summary);
    println!(
        "Dispatch: {}{}",
        report.dispatch.status.as_str(),
        report
            .dispatch
            .session_id
            .map(|id| format!(" (session {id})"))
            .unwrap_or_default()
    );
    if let Some(path) = &report.prompt_path {
        println!("Prompt: {path}");
    }
    for command in &report.next_commands {
        println!("run: {command}");
    }
    Ok(())
}

fn render_quick_test_report(report: &crate::QuickTestReport, json: bool) -> Result<(), UsageError> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    if report.skipped {
        println!("{}", report.message);
        return Ok(());
    }
    println!("{}", report.message);
    if report.chau7.detected {
        println!(
            "Chau7 runtime markers detected: {}",
            report.chau7.markers.join(", ")
        );
    }
    for step in &report.steps {
        println!("{:<8} {:<20} {}", step.status, step.name, step.detail);
    }
    if let Some(gate) = &report.gate_fixture {
        println!("gate fixture: {}", gate.gate_name);
        println!("  passing entry: q{}", gate.passing_entry_id);
        for outcome in &gate.passing_outcomes {
            println!(
                "    {} {}{}",
                outcome.gate,
                outcome.status.as_str(),
                if outcome.cached { " (cached)" } else { "" }
            );
        }
        println!(
            "  failing entry: q{} ({})",
            gate.failing_entry_id,
            gate.failing_entry_status.as_str()
        );
        for outcome in &gate.failing_outcomes {
            println!(
                "    {} {}{}",
                outcome.gate,
                outcome.status.as_str(),
                if outcome.cached { " (cached)" } else { "" }
            );
        }
    }
    println!(
        "temporary repo removed: {}",
        if report.temp_repo_removed {
            "yes"
        } else {
            "no"
        }
    );
    if let Some(head) = &report.integration_head {
        println!("integration head: {}", &head[..12.min(head.len())]);
    }
    Ok(())
}

fn init_next_steps_message() -> &'static str {
    "First-time flow: install -> `aethyme init` -> `aethyme broker quick-test` -> \
     `aethyme broker start --task \"...\"` -> `aethyme broker submit --session <id>`.\n\
     Next steps: review any drafts above, re-check anytime with `aethyme certify`, \
     then run the disposable smoke before starting real sessions; optionally \
     `aethyme enhance deploy` installs the agent protocol into AGENTS.md/CLAUDE.md."
}

fn print_overlap_warnings(overlaps: &[crate::Overlap]) {
    for overlap in overlaps {
        eprintln!(
            "⚠ overlap: sessions {} and {} are both touching {}",
            overlap.session_a, overlap.session_b, overlap.path
        );
    }
}

fn print_promoted_conflict_warnings(conflicts: &[crate::PromotedConflict]) {
    for conflict in conflicts {
        eprintln!(
            "⚠ promoted conflict: session {} is touching {}; integration already changed {}",
            conflict.session_id, conflict.session_path, conflict.promoted_path
        );
    }
}

fn render_status_advice(advice: &[crate::StatusAdvice]) {
    println!("Next actions:");
    if advice.is_empty() {
        println!("  none");
        return;
    }
    for (index, item) in advice.iter().enumerate() {
        println!(
            "  {}. {:<7} {}",
            index + 1,
            item.severity.as_str().to_uppercase(),
            item.summary
        );
        if !item.evidence.is_empty() {
            println!("     evidence: {}", item.evidence.join("; "));
        }
        for command in &item.commands {
            println!("     run: {command}");
        }
    }
}

fn render_repair_report(report: &crate::RepairReport) {
    println!(
        "Repair session {}: {}",
        report.session_id,
        report.action.as_str()
    );
    println!("  source: {}", report.source.as_str());
    if let Some(base) = &report.base {
        println!("  base: {}", &base[..12.min(base.len())]);
    }
    println!(
        "  leases refreshed: {}",
        if report.leases_refreshed { "yes" } else { "no" }
    );
    if report.affected_gates.is_empty() {
        println!("  affected gates: none");
    } else {
        println!("  affected gates:");
        for gate in &report.affected_gates {
            match &gate.triggered_by {
                Some(path) => println!("    - {} (triggered by {})", gate.gate, path),
                None => println!("    - {} (always runs)", gate.gate),
            }
        }
    }
    println!("  next: {}", report.next_command);
}

fn render_finish_report(report: &crate::FinishReport) {
    println!(
        "Finish session {}: {}",
        report.session_id,
        report.status.as_str()
    );
    println!("  {}", report.summary);
    println!("  worktree: {}", report.worktree_path);
    if let Some(entry_id) = report.latest_queue_entry_id {
        let status = report
            .latest_queue_status
            .map(|status| status.as_str())
            .unwrap_or("unknown");
        println!("  latest queue: qid {entry_id} ({status})");
    }
    if !report.dirty_paths.is_empty() {
        println!("  dirty paths: {}", capped_join(&report.dirty_paths, 8));
    }
    if report.unsubmitted_commits > 0 {
        println!("  unsubmitted commits: {}", report.unsubmitted_commits);
    }
    println!(
        "  cleanup safe: {}",
        if report.cleanup_safe { "yes" } else { "no" }
    );
    for warning in &report.warnings {
        println!("  warning: {warning}");
    }
    if report.next_commands.is_empty() {
        println!("  next: none");
    } else {
        println!("  next:");
        for command in &report.next_commands {
            println!("    run: {command}");
        }
    }
}

fn render_verify_loop_report(report: &crate::VerifyLoopReport) {
    println!(
        "Broker verify-loop: {}",
        if report.ok { "passed" } else { "failed" }
    );
    println!(
        "  integration tested: {} @ {}",
        report.integration_branch,
        short_commit(&report.tested_integration_head)
    );
    println!(
        "  integration current: {} @ {}",
        report.integration_branch,
        short_commit(&report.current_integration_head)
    );
    if report.integration_moved {
        println!(
            "  warning: integration moved during verification; tested old tip {}, current tip {}; rerun needed",
            short_commit(&report.tested_integration_head),
            short_commit(&report.current_integration_head)
        );
    }
    println!("Steps:");
    for step in &report.steps {
        println!(
            "  {:<22} {:<5} {} ({}ms)",
            step.name,
            step.status.as_str(),
            step.detail,
            step.duration_ms
        );
    }
    if let Some(quick) = &report.quick_test
        && let Some(head) = &quick.integration_head
    {
        println!("  quick-test temp integration: {}", short_commit(head));
    }
    if let Some(doctor) = &report.doctor {
        println!(
            "  doctor version: {} — {}",
            doctor.version.status.as_str(),
            doctor.version.message
        );
    }
    if report.source_tests.attempted {
        println!(
            "  source test command: {}",
            report.source_tests.command.join(" ")
        );
        if report.source_tests.status != crate::VerifyLoopStepStatus::Pass {
            for line in report
                .source_tests
                .stderr_tail
                .iter()
                .chain(report.source_tests.stdout_tail.iter())
                .take(8)
            {
                println!("    {line}");
            }
        }
    }
    if report.ok {
        println!("Next: none");
    } else if report.integration_moved {
        println!("Next: rerun `aethyme broker verify-loop` on the current integration tip.");
    } else {
        println!("Next: fix the failed step above, then rerun `aethyme broker verify-loop`.");
    }
}

fn render_semantic_gate_advice(report: &crate::SemanticGateAdvice) {
    println!("Semantic gate selection: advisory only");
    println!("  session: {}", report.session_id);
    println!("  enforced by this command: no");
    if report.changed_files.is_empty() {
        println!("  changed files: none");
    } else {
        println!("  changed files: {}", capped_join(&report.changed_files, 8));
    }
    println!(
        "  semantic source: {} ({})",
        report.semantic.provider,
        report.semantic.status.as_str()
    );
    println!("    {}", report.semantic.reason);

    if report.path_selected_gates.is_empty() {
        println!("  path-selected gates: none");
    } else {
        println!("  path-selected gates:");
        for gate in &report.path_selected_gates {
            match &gate.triggered_by {
                Some(path) => println!("    - {} (triggered by {})", gate.gate, path),
                None => println!("    - {} (always runs)", gate.gate),
            }
        }
    }

    if report.semantic_suggested_gates.is_empty() {
        println!("  semantic suggestions: none");
    } else {
        println!("  semantic suggestions:");
        for gate in &report.semantic_suggested_gates {
            match &gate.triggered_by {
                Some(path) => println!("    - {} (via {})", gate.gate, path),
                None => println!("    - {} ({})", gate.gate, gate.reason),
            }
        }
    }
    println!("  next: {}", report.next_action);
}

fn short_commit(commit: &str) -> &str {
    &commit[..12.min(commit.len())]
}

fn capped_join(values: &[String], limit: usize) -> String {
    let mut shown: Vec<String> = values.iter().take(limit).cloned().collect();
    if values.len() > shown.len() {
        shown.push(format!("and {} more", values.len() - shown.len()));
    }
    shown.join(", ")
}

fn plural<'a>(count: usize, singular: &'a str, plural: &'a str) -> &'a str {
    if count == 1 { singular } else { plural }
}

fn render_integration_status(
    report: &crate::IntegrationStatusView,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }

    println!(
        "Integration: {} @ {}",
        report.branch,
        short_commit(&report.head)
    );
    let main_relation = if report.head == report.main_head {
        "current with integration".to_string()
    } else if report.main_is_ancestor {
        format!(
            "{} {} behind integration",
            report.commits_ahead_main,
            plural(report.commits_ahead_main as usize, "commit", "commits")
        )
    } else {
        "diverged from integration".into()
    };
    println!(
        "Main:        {} ({main_relation})",
        short_commit(&report.main_head)
    );
    if let (Some(upstream_ref), Some(upstream_head)) = (&report.upstream_ref, &report.upstream_head)
    {
        let relation = if report.main_behind_upstream_commits > 0 {
            format!(
                "local main behind by {} {}",
                report.main_behind_upstream_commits,
                plural(
                    report.main_behind_upstream_commits as usize,
                    "commit",
                    "commits"
                )
            )
        } else {
            "no fetched commits ahead of local main".into()
        };
        println!(
            "Upstream:    {} @ {} ({relation})",
            upstream_ref,
            short_commit(upstream_head)
        );
    }
    println!();

    if report.promoted_entries.is_empty() && report.changed_files.is_empty() {
        println!("Pending layer: none");
    } else {
        println!(
            "Pending layer: {} promoted {}, {} {} changed, {} {} ahead of main",
            report.promoted_entries.len(),
            plural(report.promoted_entries.len(), "entry", "entries"),
            report.changed_files.len(),
            plural(report.changed_files.len(), "file", "files"),
            report.commits_ahead_main,
            plural(report.commits_ahead_main as usize, "commit", "commits"),
        );
    }

    if report.promoted_entries.is_empty() {
        println!("Promoted entries: none");
    } else {
        println!("Promoted entries:");
        for entry in report.promoted_entries.iter().take(10) {
            let label = entry
                .task
                .as_deref()
                .or(entry.branch.as_deref())
                .unwrap_or("-");
            println!(
                "  q{} session {} {} -> {}  {}",
                entry.queue_entry_id,
                entry.session_id,
                short_commit(&entry.head_commit),
                short_commit(&entry.merge_commit),
                label
            );
            if !entry.files.is_empty() {
                println!("    files: {}", capped_join(&entry.files, 5));
            }
        }
        if report.promoted_entries.len() > 10 {
            println!(
                "  and {} more promoted {}",
                report.promoted_entries.len() - 10,
                plural(report.promoted_entries.len() - 10, "entry", "entries")
            );
        }
    }

    if report.changed_files.is_empty() {
        println!("Changed files: none");
    } else {
        println!("Changed files:");
        for path in report.changed_files.iter().take(12) {
            println!("  - {path}");
        }
        if report.changed_files.len() > 12 {
            println!(
                "  and {} more {}",
                report.changed_files.len() - 12,
                plural(report.changed_files.len() - 12, "file", "files")
            );
        }
    }

    if report.conflicts.is_empty() {
        println!("Conflicts with pending layer: none");
    } else {
        println!("Conflicts with pending layer:");
        for conflict in report.conflicts.iter().take(12) {
            println!(
                "  session {}: {} (session {}, integration {})",
                conflict.session_id, conflict.path, conflict.session_path, conflict.promoted_path
            );
        }
        if report.conflicts.len() > 12 {
            println!(
                "  and {} more {}",
                report.conflicts.len() - 12,
                plural(report.conflicts.len() - 12, "conflict", "conflicts")
            );
        }
    }

    println!("Next action: {}", report.next_action.summary);
    for command in &report.next_action.commands {
        println!("  run: {command}");
    }
    Ok(())
}

fn render_ship_plan(report: &crate::ShipPlan, json: bool) -> Result<(), UsageError> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    println!(
        "Ship plan q{} (session {})",
        report.queue_entry.id, report.originating_session.id
    );
    println!(
        "Integration: {} @ {}",
        report.integration_ref, report.integration_sha
    );
    println!(
        "Local default:  {} @ {}",
        report.local_default_branch_ref, report.local_default_branch_sha
    );
    println!(
        "Remote default: {}/{} @ {}",
        report.remote, report.remote_default_branch_ref, report.remote_default_branch_sha
    );
    println!("Target: {}", report.target_repository);
    println!("Freshness: {:?}", report.freshness.result);
    println!("Proposed push: {}", report.proposed_push.command.join(" "));
    println!(
        "Local-main synchronization safe now: {}",
        if report.local_main_sync_safe {
            "yes"
        } else {
            "no"
        }
    );
    println!(
        "Confirm with: aethyme broker ship execute --entry {} --confirm {}",
        report.queue_entry.id, report.integration_sha
    );
    Ok(())
}

fn render_integration_stability(
    report: &crate::IntegrationStabilityReport,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }

    println!(
        "Integration: {} {} -> {}",
        report.branch,
        short_commit(&report.start_head),
        short_commit(&report.end_head)
    );
    println!(
        "Window:      {}s (observed {}ms)",
        report.requested_seconds, report.observed_ms
    );
    println!(
        "Result:      {}",
        if report.stable { "stable" } else { "moved" }
    );
    println!("{}", report.message);
    if report.live_sessions.is_empty() {
        println!("Live sessions: none");
    } else {
        println!("Live sessions:");
        for session in report.live_sessions.iter().take(10) {
            println!(
                "  session {} {} {} {}",
                session.id,
                session.status.as_str(),
                session.branch,
                session.task.as_deref().unwrap_or("-")
            );
        }
        if report.live_sessions.len() > 10 {
            println!(
                "  and {} more {}",
                report.live_sessions.len() - 10,
                plural(report.live_sessions.len() - 10, "session", "sessions")
            );
        }
    }
    for command in &report.commands {
        println!("run: {command}");
    }
    Ok(())
}

fn render_integration_reconcile(
    report: &crate::IntegrationReconcileReport,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }

    println!("Local main:  {}", short_commit(&report.local_main));
    println!(
        "Upstream:    {} @ {}",
        report.upstream_ref,
        short_commit(&report.upstream_head)
    );
    println!(
        "Integration: {} -> {}",
        short_commit(&report.old_integration),
        short_commit(&report.new_integration)
    );
    if let Some(path) = &report.resolution_file {
        println!("Resolution:  {path}");
    }
    println!(
        "Result:      {}",
        if report.applied {
            "applied"
        } else if report.safe {
            "safe dry-run"
        } else {
            "blocked"
        }
    );
    for entry in &report.entries {
        println!(
            "  q{} session {}: {} — {}",
            entry.queue_entry_id,
            entry.session_id,
            entry.classification.as_str(),
            entry.evidence
        );
        if !entry.conflicts.is_empty() {
            println!("    conflicts: {}", capped_join(&entry.conflicts, 5));
        }
    }
    for warning in &report.warnings {
        println!("Warning: {warning}");
    }
    println!("Next action: {}", report.next_action);
    Ok(())
}

fn open_broker() -> Result<Broker, UsageError> {
    let cwd = std::env::current_dir()
        .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
    Ok(Broker::open(&cwd)?)
}

fn parse_operation_effect(
    value: Option<&str>,
) -> Result<Option<crate::OperationEffect>, UsageError> {
    value
        .map(|value| {
            crate::OperationEffect::parse(value).map_err(|_| {
                UsageError::Message("--effect must be read, write, or destructive".into())
            })
        })
        .transpose()
}

fn render_coordinated_operation(
    report: &crate::CoordinatedOperationReport,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
    } else {
        if !report.stdout.is_empty() {
            print!("{}", report.stdout);
            if !report.stdout.ends_with('\n') {
                println!();
            }
        }
        if !report.stderr.is_empty() {
            eprint!("{}", report.stderr);
            if !report.stderr.ends_with('\n') {
                eprintln!();
            }
        }
        println!(
            "operation {}: {} {} on {} ({})",
            report.operation.id,
            report.operation.provider.as_str(),
            report.operation.status.as_str(),
            report.operation.repository,
            report.classification,
        );
    }
    Ok(())
}

fn run_inner(args: &[String]) -> Result<(), UsageError> {
    let Some(subcommand) = args.first() else {
        return Err(UsageError::Help);
    };
    let parsed = parse(&args[1..])?;

    match subcommand.as_str() {
        "adopt" => {
            let mut broker = open_broker()?;
            let path = parsed.positional.first().map(PathBuf::from).unwrap_or(
                std::env::current_dir().map_err(|e| UsageError::Message(e.to_string()))?,
            );
            let mode = match (parsed.reuse, parsed.replace_stale) {
                (true, true) => {
                    return Err(UsageError::Message(
                        "--reuse and --replace-stale are mutually exclusive".into(),
                    ));
                }
                (true, false) => crate::AdoptMode::Reuse,
                (false, true) => crate::AdoptMode::ReplaceStale,
                (false, false) => crate::AdoptMode::New,
            };
            let session = broker.adopt_with(&path, parsed.task.as_deref(), mode)?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&session)?);
            } else {
                let verb = if parsed.reuse { "Reusing" } else { "Adopted" };
                println!(
                    "{verb} session {} — worktree {} on branch {}",
                    session.id, session.worktree_path, session.branch
                );
                if std::path::Path::new(&session.worktree_path) == broker.main_root() {
                    println!(
                        "note: main-checkout session — verification is advisory here \
                         (commits land on main before gates run); use a worktree \
                         session for enforced verification."
                    );
                }
            }
        }
        "start" => {
            let task = parsed
                .task
                .ok_or(UsageError::Message("start requires --task".into()))?;
            let mut broker = open_broker()?;
            let session = broker.start_worktree(&task)?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&session)?);
            } else {
                println!(
                    "Started session {} — worktree {} on branch {}",
                    session.id, session.worktree_path, session.branch
                );
                println!("Next: cd {}", session.worktree_path);
            }
        }
        "start-agent" => {
            let task = parsed
                .task
                .ok_or(UsageError::Message("start-agent requires --task".into()))?;
            let cmd = parsed
                .cmd
                .ok_or(UsageError::Message("start-agent requires --cmd".into()))?;
            let mut broker = open_broker()?;
            let session = broker.start_agent(&task, &cmd)?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&session)?);
            } else {
                println!(
                    "Started session {} (pid {}) — worktree {} on branch {}\nLog: {}",
                    session.id,
                    session.pid.unwrap_or(-1),
                    session.worktree_path,
                    session.branch,
                    session.log_path.as_deref().unwrap_or("-"),
                );
            }
        }
        "agents" => {
            let mut broker = open_broker()?;
            let overlaps = broker.refresh_leases()?;
            let views = broker.agents(now_ms())?;
            if parsed.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "agents": views,
                        "overlaps": overlaps,
                    }))?
                );
            } else if views.is_empty() {
                println!("No live sessions. Start one with `aethyme broker start --task \"...\"`.");
            } else {
                println!(
                    "{:<4} {:<8} {:<8} {:<24} TASK",
                    "ID", "STATUS", "ORIGIN", "BRANCH"
                );
                for view in views {
                    println!(
                        "{:<4} {:<8} {:<8} {:<24} {}",
                        view.session.id,
                        view.derived_status.as_str(),
                        view.session.origin.as_str(),
                        view.session.branch,
                        view.session.task.as_deref().unwrap_or("-"),
                    );
                }
                print_overlap_warnings(&overlaps);
            }
        }
        "leases" => {
            let mut broker = open_broker()?;
            match parsed.positional.first().map(String::as_str) {
                None => {
                    let overlaps = broker.refresh_leases()?;
                    let leases = broker.store().active_leases()?;
                    if parsed.json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "leases": leases,
                                "overlaps": overlaps,
                            }))?
                        );
                    } else if leases.is_empty() {
                        println!("No active leases.");
                    } else {
                        println!("{:<4} {:<9} PATH", "SID", "KIND");
                        for lease in leases {
                            println!(
                                "{:<4} {:<9} {}",
                                lease.session_id,
                                lease.kind.as_str(),
                                lease.path
                            );
                        }
                        print_overlap_warnings(&overlaps);
                    }
                }
                Some("claim") => {
                    let path = parsed
                        .positional
                        .get(1)
                        .ok_or(UsageError::Message("claim requires a path".into()))?;
                    let session = parsed
                        .session
                        .ok_or(UsageError::Message("claim requires --session <id>".into()))?;
                    let report =
                        broker.claim_lease(session, path, parsed.ttl_seconds.map(|s| s * 1000))?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        println!("Session {session} claimed {path}.");
                    }
                }
                Some("release") => {
                    let path = parsed
                        .positional
                        .get(1)
                        .ok_or(UsageError::Message("release requires a path".into()))?;
                    let session = parsed.session.ok_or(UsageError::Message(
                        "release requires --session <id>".into(),
                    ))?;
                    broker.store().release_lease(session, path)?;
                    if parsed.json {
                        println!("{{\"released\":{}}}", serde_json::to_string(path)?);
                    } else {
                        println!("Session {session} released {path}.");
                    }
                }
                Some(other) => {
                    return Err(UsageError::Message(format!(
                        "unknown leases action {other:?} — expected claim or release"
                    )));
                }
            }
        }
        "exec" => {
            let session = parsed
                .session
                .ok_or(UsageError::Message("exec requires --session <id>".into()))?;
            let mut broker = open_broker()?;
            let report = broker.guarded_exec(session, &parsed.exec_command)?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "exec session {}: command {}{}",
                    session,
                    if report.command_success {
                        "passed"
                    } else {
                        "failed"
                    },
                    report
                        .exit_code
                        .map(|code| format!(" ({code})"))
                        .unwrap_or_default()
                );
                if report.touched_paths.is_empty() {
                    println!("  touched paths: none");
                } else {
                    println!("  touched paths: {}", capped_join(&report.touched_paths, 8));
                }
                if !report.outside_lease_paths.is_empty() {
                    println!(
                        "  outside explicit leases: {}",
                        capped_join(&report.outside_lease_paths, 8)
                    );
                }
                if !report.foreign_paths.is_empty() {
                    println!(
                        "  adoption-time foreign paths: {}",
                        capped_join(&report.foreign_paths, 8)
                    );
                }
            }
            if !report.ok {
                return Err(UsageError::Message(
                    "guarded exec failed ownership or command checks".into(),
                ));
            }
        }
        "git" | "gh" => {
            let session = parsed.session.ok_or(UsageError::Message(format!(
                "{subcommand} requires --session <id>"
            )))?;
            let provider = if subcommand == "git" {
                crate::OperationProvider::Git
            } else {
                crate::OperationProvider::Github
            };
            let request = crate::CoordinatedCommand {
                session_id: session,
                provider,
                repository: parsed.repository,
                scope: parsed.scope,
                declared_effect: parse_operation_effect(parsed.effect.as_deref())?,
                destructive_confirmed: parsed.destructive,
                authorization_reason: parsed.reason,
                args: parsed.exec_command,
            };
            let mut broker = open_broker()?;
            let report = broker.run_coordinated_operation(request)?;
            render_coordinated_operation(&report, parsed.json)?;
            if !report.ok() {
                return Err(UsageError::Message(format!(
                    "coordinated {subcommand} operation {} failed",
                    report.operation.id
                )));
            }
        }
        "operations" => {
            let mut broker = open_broker()?;
            match parsed.positional.first().map(String::as_str) {
                None => {
                    let operations = broker.store().coordinated_operations()?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&operations)?);
                    } else if operations.is_empty() {
                        println!("No coordinated operations recorded.");
                    } else {
                        println!(
                            "{:<5} {:<8} {:<12} {:<22} SCOPE",
                            "ID", "TOOL", "STATUS", "REPOSITORY"
                        );
                        for operation in operations {
                            println!(
                                "{:<5} {:<8} {:<12} {:<22} {}",
                                operation.id,
                                operation.provider.as_str(),
                                operation.status.as_str(),
                                operation.repository,
                                operation.scope,
                            );
                        }
                    }
                }
                Some("reconcile") => {
                    let operation = parsed.operation.ok_or(UsageError::Message(
                        "operations reconcile requires --operation <id>".into(),
                    ))?;
                    let outcome = parsed.outcome.as_deref().ok_or(UsageError::Message(
                        "operations reconcile requires --outcome succeeded|failed".into(),
                    ))?;
                    let succeeded = match outcome {
                        "succeeded" => true,
                        "failed" => false,
                        _ => {
                            return Err(UsageError::Message(
                                "--outcome must be succeeded or failed".into(),
                            ));
                        }
                    };
                    let reason = parsed.reason.as_deref().ok_or(UsageError::Message(
                        "operations reconcile requires --reason <text>".into(),
                    ))?;
                    let report =
                        broker.reconcile_coordinated_operation(operation, succeeded, reason)?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        println!(
                            "operation {} reconciled as {}: {}",
                            report.operation.id,
                            report.operation.status.as_str(),
                            report.reason,
                        );
                    }
                }
                Some(other) => {
                    return Err(UsageError::Message(format!(
                        "unknown operations action {other:?} — expected reconcile"
                    )));
                }
            }
        }
        "gates" => {
            let action =
                parsed
                    .positional
                    .first()
                    .map(String::as_str)
                    .ok_or(UsageError::Message(
                        "gates requires an action: draft, validate, affected, semantic, or run"
                            .into(),
                    ))?;
            match action {
                "draft" => {
                    let cwd = std::env::current_dir()
                        .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
                    let report = crate::init::draft_gates(&cwd)?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        for check in &report.checks {
                            println!(
                                "{:<8} {}",
                                format!("{:?}", check.status).to_lowercase(),
                                check.detail
                            );
                        }
                    }
                }
                "validate" => {
                    let broker = open_broker()?;
                    let gates = aethyme_gates_load(broker.main_root())?;
                    if parsed.json {
                        let summary: Vec<_> = gates
                            .iter()
                            .map(|g| {
                                serde_json::json!({
                                    "name": g.name, "command": g.command,
                                    "cost": g.cost, "triggers": g.triggers,
                                    "cache": g.cache,
                                })
                            })
                            .collect();
                        println!("{}", serde_json::to_string_pretty(&summary)?);
                    } else {
                        println!("gates.toml OK — {} gate(s), cheap-first:", gates.len());
                        for gate in gates {
                            println!(
                                "  [{}] {} — {} (triggers: {}{})",
                                gate.cost,
                                gate.name,
                                gate.command,
                                if gate.triggers.is_empty() {
                                    "always".to_string()
                                } else {
                                    gate.triggers.join(", ")
                                },
                                if gate.cache { "" } else { "; cache: off" }
                            );
                        }
                    }
                }
                "affected" => {
                    let session = parsed.session.ok_or(UsageError::Message(
                        "gates affected requires --session <id>".into(),
                    ))?;
                    let mut broker = open_broker()?;
                    let selections = broker.affected_gates(session)?;
                    if parsed.json {
                        let out: Vec<_> = selections
                            .iter()
                            .map(|(gate, why)| {
                                serde_json::json!({"gate": gate, "triggered_by": why})
                            })
                            .collect();
                        println!("{}", serde_json::to_string_pretty(&out)?);
                    } else if selections.is_empty() {
                        println!("No gates affected by this session's diff.");
                    } else {
                        for (gate, why) in selections {
                            match why {
                                Some(path) => println!("{gate}  (triggered by {path})"),
                                None => println!("{gate}  (always runs)"),
                            }
                        }
                    }
                }
                "semantic" => {
                    let session = parsed.session.ok_or(UsageError::Message(
                        "gates semantic requires --session <id>".into(),
                    ))?;
                    let mut broker = open_broker()?;
                    let report = broker.semantic_gate_advice(session)?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        render_semantic_gate_advice(&report);
                    }
                }
                "run" if parsed.all => {
                    if parsed.session.is_some() {
                        return Err(UsageError::Message(
                            "gates run takes --session <id> or --all, not both".into(),
                        ));
                    }
                    let cwd = std::env::current_dir()
                        .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
                    let mut broker = open_broker()?;
                    let outcomes = broker.run_all_gates(&cwd)?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&outcomes)?);
                    } else {
                        for outcome in &outcomes {
                            println!(
                                "{:<20} {:<10} {}{}",
                                outcome.gate,
                                gate_status_label(outcome.status, outcome.failure_class),
                                if outcome.cached { "(cached) " } else { "" },
                                outcome
                                    .duration_ms
                                    .map(|ms| format!("{ms}ms"))
                                    .unwrap_or_default(),
                            );
                        }
                    }
                    // Unlike --session runs, --all is the CI entrypoint:
                    // the exit code must be conclusive in --json mode too.
                    if outcomes
                        .iter()
                        .any(|outcome| outcome.status != crate::GateStatus::Pass)
                    {
                        return Err(UsageError::Message("one or more gates did not pass".into()));
                    }
                }
                "run" => {
                    let session = parsed.session.ok_or(UsageError::Message(
                        "gates run requires --session <id> (or --all)".into(),
                    ))?;
                    let mut broker = open_broker()?;
                    let outcomes = broker.run_gates(session)?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&outcomes)?);
                    } else if outcomes.is_empty() {
                        println!("No gates affected — nothing to run.");
                    } else {
                        let mut failed = false;
                        for outcome in &outcomes {
                            println!(
                                "{:<20} {:<10} {}{}",
                                outcome.gate,
                                gate_status_label(outcome.status, outcome.failure_class),
                                if outcome.cached { "(cached) " } else { "" },
                                outcome
                                    .duration_ms
                                    .map(|ms| format!("{ms}ms"))
                                    .unwrap_or_default(),
                            );
                            failed |= outcome.status.as_str() != "pass";
                        }
                        if failed {
                            return Err(UsageError::Message(
                                "one or more gates did not pass".into(),
                            ));
                        }
                    }
                }
                other => {
                    return Err(UsageError::Message(format!(
                        "unknown gates action {other:?} — expected draft, validate, affected, semantic, or run"
                    )));
                }
            }
        }
        "pr" => {
            let action = parsed
                .positional
                .first()
                .map(String::as_str)
                .ok_or(UsageError::Message("pr requires an action: check".into()))?;
            match action {
                "check" => {
                    let mut broker = open_broker()?;
                    let report = broker.pr_check(crate::PrCheckOptions {
                        target_branch: parsed.target.unwrap_or_else(|| "production".into()),
                        pr_number: parsed.pr_number,
                        agent_name: parsed.agent.unwrap_or_else(|| "Push2prod".into()),
                        dispatch: parsed.dispatch,
                        agent_command: parsed.cmd,
                        now_ms: now_ms(),
                    })?;
                    render_pr_check_report(&report, parsed.json)?;
                }
                other => {
                    return Err(UsageError::Message(format!(
                        "unknown pr action {other:?} — expected check"
                    )));
                }
            }
        }
        "hooks" => {
            let action =
                parsed
                    .positional
                    .first()
                    .map(String::as_str)
                    .ok_or(UsageError::Message(
                        "hooks requires an action: install, uninstall, or status".into(),
                    ))?;
            // Hook management needs only the git repo — never the broker
            // db, so `hooks install` on a fresh clone creates no state.
            let cwd = std::env::current_dir()
                .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
            match action {
                "install" => {
                    let repo = crate::GitRepo::discover(&cwd)?;
                    let binary = std::env::current_exe().map_err(|err| {
                        UsageError::Message(format!("cannot resolve the aethyme binary: {err}"))
                    })?;
                    let reports = crate::hooks::install(&repo, &binary)?;
                    render_hook_reports(&reports, parsed.json)?;
                    if !parsed.json {
                        println!(
                            "Hooks are shared by every worktree. Uninstall any time with \
                             `aethyme broker hooks uninstall`."
                        );
                    }
                }
                "uninstall" => {
                    let repo = crate::GitRepo::discover(&cwd)?;
                    let reports = crate::hooks::uninstall(&repo)?;
                    render_hook_reports(&reports, parsed.json)?;
                }
                "status" => {
                    let repo = crate::GitRepo::discover(&cwd)?;
                    let reports = crate::hooks::status(&repo)?;
                    render_hook_reports(&reports, parsed.json)?;
                }
                // Internal entry points the installed shims call.
                "pre-commit" => crate::hooks::run_pre_commit(&cwd)?,
                "post-commit" => crate::hooks::run_post_commit(&cwd),
                other => {
                    return Err(UsageError::Message(format!(
                        "unknown hooks action {other:?} — expected install, uninstall, or status"
                    )));
                }
            }
        }
        "submit" => {
            let session = parsed
                .session
                .ok_or(UsageError::Message("submit requires --session <id>".into()))?;
            let mut broker = open_broker()?;
            // Preflight (dogfood feedback 2026-07-14): show exactly what
            // will be submitted before anything runs — and warn about
            // uncommitted work, which never integrates.
            if !parsed.json
                && let Ok(info) = broker.store().session(session)
                && let Ok(checkout) =
                    crate::GitRepo::discover(std::path::Path::new(&info.worktree_path))
            {
                let head = checkout.head_commit().unwrap_or_default();
                println!(
                    "Submitting session {session} — HEAD {}",
                    &head[..12.min(head.len())]
                );
                let base = broker
                    .session_change_base(&checkout)
                    .or_else(|| info.diff_base.clone());
                if let Some(base) = base.as_deref()
                    && let Ok(commits) = checkout.commit_summaries(base, "HEAD", 10)
                {
                    if commits.is_empty() {
                        println!(
                            "  no commits since the session baseline — \
                             nothing new to integrate"
                        );
                    }
                    for line in &commits {
                        println!("  {line}");
                    }
                }
                if let Ok(dirty) = checkout.dirty_paths()
                    && !dirty.is_empty()
                {
                    println!(
                        "  ⚠ {} uncommitted change(s) NOT included \
                         (only committed work integrates), e.g. {}",
                        dirty.len(),
                        dirty.first().map(String::as_str).unwrap_or("")
                    );
                }
            }
            let outcome = broker.submit(session)?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&outcome)?);
            } else if !outcome.conflicts.is_empty() {
                eprintln!("✗ conflict — rejected before any gate ran. Conflicting files:");
                for file in &outcome.conflicts {
                    eprintln!("  - {file}");
                }
                eprintln!(
                    "Instructions written to the session worktree at {}",
                    crate::ACTION_REQUIRED_RELPATH
                );
                eprintln!(
                    "Quick start: git fetch . {base} && git rebase {base}   (then resubmit)",
                    base = outcome.entry.base_commit
                );
                return Err(UsageError::Message("submission conflicted".into()));
            } else {
                let gate_wall_ms: i64 = outcome
                    .gate_outcomes
                    .iter()
                    .filter(|gate| !gate.cached)
                    .filter_map(|gate| gate.duration_ms)
                    .sum();
                for gate in &outcome.gate_outcomes {
                    if gate.cached {
                        println!(
                            "gate {:<20} {} (cached, saved {})",
                            gate.gate,
                            gate_status_label(gate.status, gate.failure_class),
                            duration_label(gate.duration_ms)
                        );
                    } else {
                        println!(
                            "gate {:<20} {} in {}",
                            gate.gate,
                            gate_status_label(gate.status, gate.failure_class),
                            duration_label(gate.duration_ms)
                        );
                    }
                }
                println!("gate wall time: {}ms", gate_wall_ms);
                println!(
                    "entry {} → {}{}",
                    outcome.entry.id,
                    outcome.entry.status.as_str(),
                    if outcome.promoted {
                        " (auto-promoted)"
                    } else {
                        ""
                    }
                );
                if outcome.entry.status.as_str() == "rejected" {
                    if let Ok(info) = broker.store().session(outcome.entry.session_id)
                        && std::path::Path::new(&info.worktree_path) == broker.main_root()
                    {
                        eprintln!(
                            "note: this work is already on main (main-checkout session) — \
                             the broker cannot hold it back. Fix forward on main and resubmit."
                        );
                    }
                    return Err(UsageError::Message(
                        "gates failed on the merged tree".into(),
                    ));
                }
                // "What now?" — the next expected human action was
                // implicit (dogfood feedback 2026-07-14).
                if outcome.promoted {
                    let integration = broker
                        .integration_head()
                        .map(|(_, commit)| commit[..12.min(commit.len())].to_string())
                        .unwrap_or_else(|_| "?".into());
                    println!(
                        "What now: aethyme/integration is at {integration} and contains this work. \
                         Your checkout and branches are untouched — keep working, or start \
                         a follow-up with `aethyme broker adopt --reuse --task \"...\"`, or \
                         finish safely with `aethyme broker finish --session {}`.",
                        outcome.entry.session_id,
                    );
                } else {
                    println!(
                        "What now: entry {} is verified but not promoted (manual mode). \
                         Promote with `aethyme broker promote --entry {}`.",
                        outcome.entry.id, outcome.entry.id,
                    );
                }
            }
        }
        "repair" => {
            let session = parsed
                .session
                .ok_or(UsageError::Message("repair requires --session <id>".into()))?;
            let mut broker = open_broker()?;
            let report = broker.repair(session)?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                render_repair_report(&report);
            }
        }
        "queue" => {
            let mut broker = open_broker()?;
            let entries = broker.store().merge_queue()?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else if entries.is_empty() {
                println!("Merge queue is empty.");
            } else {
                println!("{:<4} {:<4} {:<11} HEAD", "ID", "SID", "STATUS");
                for entry in entries {
                    println!(
                        "{:<4} {:<4} {:<11} {}",
                        entry.id,
                        entry.session_id,
                        entry.status.as_str(),
                        &entry.head_commit[..12.min(entry.head_commit.len())]
                    );
                }
            }
        }
        "promote" => {
            let entry = parsed
                .entry
                .ok_or(UsageError::Message("promote requires --entry <id>".into()))?;
            let mut broker = open_broker()?;
            broker.promote(entry)?;
            if parsed.json {
                println!("{{\"promoted\":{entry}}}");
            } else {
                println!("Promoted entry {entry} to the local integration branch.");
            }
        }
        "ship" => {
            let action = parsed
                .positional
                .first()
                .map(String::as_str)
                .ok_or(UsageError::Message("ship requires an action: plan".into()))?;
            match action {
                "plan" => {
                    let entry = parsed.entry.ok_or(UsageError::Message(
                        "ship plan requires --entry <id>".into(),
                    ))?;
                    let mut broker = open_broker()?;
                    let report = broker.ship_plan(entry)?;
                    render_ship_plan(&report, parsed.json)?;
                }
                other => {
                    return Err(UsageError::Message(format!(
                        "unknown ship action {other:?} — expected plan"
                    )));
                }
            }
        }
        "integration" => {
            let action =
                parsed
                    .positional
                    .first()
                    .map(String::as_str)
                    .ok_or(UsageError::Message(
                        "integration requires an action: status or wait-stable".into(),
                    ))?;
            match action {
                "status" => {
                    let mut broker = open_broker()?;
                    let report = broker.integration_status(now_ms())?;
                    render_integration_status(&report, parsed.json)?;
                }
                "wait-stable" => {
                    let seconds = parsed.seconds.unwrap_or(30);
                    let mut broker = open_broker()?;
                    let report = broker.wait_integration_stable(seconds)?;
                    render_integration_stability(&report, parsed.json)?;
                    if !report.stable {
                        return Err(UsageError::Message(
                            "integration moved during wait-stable window".into(),
                        ));
                    }
                }
                "reconcile" => {
                    if parsed.apply && parsed.dry_run {
                        return Err(UsageError::Message(
                            "choose either --dry-run or --apply, not both".into(),
                        ));
                    }
                    let upstream = parsed.upstream.clone().ok_or(UsageError::Message(
                        "integration reconcile requires --upstream <ref>".into(),
                    ))?;
                    let mut broker = open_broker()?;
                    let report =
                        broker.reconcile_integration(crate::IntegrationReconcileOptions {
                            upstream,
                            apply: parsed.apply,
                            resolution_file: parsed.resolution_file.clone(),
                        })?;
                    render_integration_reconcile(&report, parsed.json)?;
                    if !report.safe {
                        return Err(UsageError::Message(
                            "integration reconciliation is ambiguous or conflicting; no state changed"
                                .into(),
                        ));
                    }
                }
                other => {
                    return Err(UsageError::Message(format!(
                        "unknown integration action {other:?} — expected status, wait-stable, or reconcile"
                    )));
                }
            }
        }
        "status" => {
            let mut broker = open_broker()?;
            let status = broker.status(now_ms())?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!(
                    "Integration: {} @ {}",
                    status.integration_branch,
                    &status.integration_head[..12.min(status.integration_head.len())]
                );
                println!("Local main:  {}", short_commit(&status.main_head));
                if let (Some(upstream_ref), Some(upstream_head)) =
                    (&status.upstream_ref, &status.upstream_head)
                {
                    println!(
                        "Upstream:    {} @ {} ({} commits ahead of local main)",
                        upstream_ref,
                        short_commit(upstream_head),
                        status.main_behind_upstream_commits
                    );
                }
                println!("Summary: {}", status.summary.message);
                println!();
                render_status_advice(&status.advice);
                println!();
                if status.agents.is_empty() {
                    println!("No live sessions.");
                } else {
                    println!(
                        "{:<4} {:<8} {:<8} {:<24} TASK",
                        "ID", "STATUS", "ORIGIN", "BRANCH"
                    );
                    for view in &status.agents {
                        println!(
                            "{:<4} {:<8} {:<8} {:<24} {}",
                            view.session.id,
                            view.derived_status.as_str(),
                            view.session.origin.as_str(),
                            view.session.branch,
                            view.session.task.as_deref().unwrap_or("-"),
                        );
                    }
                }
                if !status.queue.is_empty() {
                    println!();
                    println!("{:<4} {:<4} {:<11} HEAD", "QID", "SID", "QSTATUS");
                    for entry in &status.queue {
                        println!(
                            "{:<4} {:<4} {:<11} {}",
                            entry.id,
                            entry.session_id,
                            entry.status.as_str(),
                            &entry.head_commit[..12.min(entry.head_commit.len())]
                        );
                    }
                }
                print_overlap_warnings(&status.overlaps);
                print_promoted_conflict_warnings(&status.promoted_conflicts);
            }
        }
        "events" => {
            if parsed.positional.first().map(String::as_str) == Some("prune") {
                let keep_days = parsed.keep_days.ok_or(UsageError::Message(
                    "events prune requires --keep-days <n>".into(),
                ))?;
                let mut broker = open_broker()?;
                let cutoff = now_ms() - keep_days * 24 * 60 * 60 * 1000;
                let removed = broker.store().prune_events_before(cutoff)?;
                if parsed.json {
                    println!("{{\"pruned\":{removed}}}");
                } else {
                    println!("Pruned {removed} event(s) older than {keep_days} day(s).");
                }
                return Ok(());
            }
            let mut broker = open_broker()?;
            let mut cursor = parsed.since.unwrap_or(0);
            // --follow survives transient read errors (e.g. a checkpoint
            // or a busy writer) with bounded retries instead of dying.
            let mut consecutive_errors = 0u32;
            loop {
                let events =
                    match broker
                        .store()
                        .events_after_filtered(cursor, 1000, parsed.kind.as_deref())
                    {
                        Ok(events) => {
                            consecutive_errors = 0;
                            events
                        }
                        Err(err) if parsed.follow && consecutive_errors < 5 => {
                            consecutive_errors += 1;
                            eprintln!("events: transient read error ({err}); retrying");
                            std::thread::sleep(std::time::Duration::from_millis(700));
                            continue;
                        }
                        Err(err) => return Err(err.into()),
                    };
                for event in &events {
                    cursor = event.id;
                    if parsed.json {
                        println!("{}", serde_json::to_string(event)?);
                    } else {
                        println!(
                            "{:<6} {} {:<28} sid={} {}",
                            event.id,
                            event.ts,
                            event.kind,
                            event
                                .session_id
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| "-".into()),
                            event.payload_json.as_deref().unwrap_or(""),
                        );
                    }
                }
                if !parsed.follow {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(700));
            }
        }
        "metrics" => {
            let mut broker = open_broker()?;
            // Gate executions (pass/fail) vs cache hits with saved time.
            let executed = broker.store().gate_execution_totals()?;
            let cached = broker
                .store()
                .events_after_filtered(0, i64::MAX, Some("gate.cached"))?;
            let saved_ms: i64 = cached
                .iter()
                .filter_map(|e| e.payload_json.as_deref())
                .filter_map(|p| serde_json::from_str::<serde_json::Value>(p).ok())
                .filter_map(|v| v.get("saved_ms").and_then(|s| s.as_i64()))
                .sum();
            let conflicts = broker
                .store()
                .events_after_filtered(0, i64::MAX, Some("merge.conflict"))?
                .len();
            let overlaps = broker
                .store()
                .events_after_filtered(0, i64::MAX, Some("lease.overlap"))?
                .len();

            // Command latency from the safe telemetry file.
            let mut commands: std::collections::BTreeMap<String, (i64, i64)> =
                std::collections::BTreeMap::new();
            let metrics_path = broker
                .main_root()
                .join(".aethyme/logs/command-metrics.jsonl");
            if let Ok(text) = std::fs::read_to_string(&metrics_path) {
                for line in text.lines() {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                        let name = v
                            .get("command")
                            .and_then(|c| c.as_str())
                            .unwrap_or("?")
                            .to_string();
                        let ms = v.get("duration_ms").and_then(|d| d.as_i64()).unwrap_or(0);
                        let entry = commands.entry(name).or_insert((0, 0));
                        entry.0 += 1;
                        entry.1 += ms;
                    }
                }
            }

            if parsed.json {
                let out = serde_json::json!({
                    "gates_executed": executed.iter().map(|(g, n, ms)| serde_json::json!({
                        "gate": g, "runs": n, "total_ms": ms,
                    })).collect::<Vec<_>>(),
                    "gate_cache_hits": cached.len(),
                    "gate_time_saved_ms": saved_ms,
                    "conflicts_caught_pre_gate": conflicts,
                    "overlaps_warned": overlaps,
                    "commands": commands.iter().map(|(name, (count, ms))| serde_json::json!({
                        "command": name, "count": count, "total_ms": ms,
                    })).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                println!("Gate executions:");
                for (gate, runs, ms) in &executed {
                    println!("  {gate:<20} {runs} run(s), {ms}ms total");
                }
                println!(
                    "Cache hits: {} (≈{}s of checks skipped)",
                    cached.len(),
                    saved_ms / 1000
                );
                println!("Conflicts caught before any gate ran: {conflicts}");
                println!("Overlap warnings: {overlaps}");
                println!("Broker command overhead:");
                for (name, (count, ms)) in &commands {
                    println!(
                        "  {name:<20} {count} call(s), {ms}ms total, {}ms avg",
                        ms / count.max(&1)
                    );
                }
            }
        }
        "doctor" => {
            let mut broker = open_broker()?;
            let report = if parsed.fix_version {
                broker.doctor_with_version_fix()?
            } else {
                broker.doctor()?
            };
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("integrity: {}", report.integrity);
                println!(
                    "version: {} — {}",
                    report.version.status.as_str(),
                    report.version.message
                );
                if let Some(describe) = &report.version.binary.describe {
                    println!(
                        "  binary: aethyme {} ({describe})",
                        report.version.binary.version
                    );
                } else {
                    println!("  binary: aethyme {}", report.version.binary.version);
                }
                if let Some(path) = &report.version.binary.path {
                    println!("  path: {path}");
                }
                if report.version.repo_is_aethyme_source {
                    let integration = report
                        .version
                        .integration_describe
                        .as_deref()
                        .or(report.version.integration_head.as_deref())
                        .unwrap_or("unknown");
                    println!(
                        "  integration: {} {integration}",
                        report.version.integration_branch
                    );
                }
                if let Some(movement) = &report.integration_movement {
                    println!("integration movement: {}", movement.message);
                    println!(
                        "  head: {} @ {}",
                        movement.branch,
                        short_commit(&movement.head)
                    );
                    for session in movement.live_sessions.iter().take(5) {
                        println!(
                            "  live session {} {} {}",
                            session.id,
                            session.status.as_str(),
                            session.branch
                        );
                    }
                    if movement.live_sessions.len() > 5 {
                        println!(
                            "  and {} more live {}",
                            movement.live_sessions.len() - 5,
                            plural(movement.live_sessions.len() - 5, "session", "sessions")
                        );
                    }
                    for command in &movement.commands {
                        println!("  run: {command}");
                    }
                }
                if let Some(repair) = &report.version_repair {
                    println!(
                        "version repair: {} — {}",
                        repair.status.as_str(),
                        repair.message
                    );
                    if repair.attempted {
                        println!("  command: {}", repair.command.join(" "));
                        println!("  duration: {}ms", repair.duration_ms);
                        if let Some(code) = repair.exit_code {
                            println!("  exit: {code}");
                        }
                    }
                    if !repair.stdout_tail.is_empty() {
                        println!("  stdout tail:");
                        for line in &repair.stdout_tail {
                            println!("    {line}");
                        }
                    }
                    if !repair.stderr_tail.is_empty() {
                        println!("  stderr tail:");
                        for line in &repair.stderr_tail {
                            println!("    {line}");
                        }
                    }
                }
                if report.missing_worktrees.is_empty() {
                    println!("worktrees: all live session worktrees exist");
                } else {
                    for id in &report.missing_worktrees {
                        println!("worktrees: session {id} worktree is missing (adopt gone stale?)");
                    }
                }
                if report.orphaned_pidfiles.is_empty() {
                    println!("gate runs: no orphaned pidfiles");
                } else {
                    for name in &report.orphaned_pidfiles {
                        println!("gate runs: orphaned pidfile removed: {name}");
                    }
                }
                if report.healthy() {
                    println!("doctor: healthy");
                } else {
                    return Err(UsageError::Message("doctor found problems".into()));
                }
            }
        }
        "quick-test" => {
            let mode = if parsed.chau7 {
                crate::QuickTestMode::Chau7
            } else {
                crate::QuickTestMode::Generic
            };
            let report = crate::run_broker_quick_test_with_options(
                mode,
                crate::QuickTestOptions {
                    with_gate: parsed.with_gate,
                },
            )?;
            render_quick_test_report(&report, parsed.json)?;
        }
        "verify-loop" | "e2e" => {
            let mut broker = open_broker()?;
            let cwd = std::env::current_dir()
                .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
            let report = broker.verify_loop_from(&cwd)?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                render_verify_loop_report(&report);
                if !report.ok {
                    return Err(UsageError::Message("broker verify-loop failed".into()));
                }
            }
        }
        "init" => {
            let cwd = std::env::current_dir()
                .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
            let report = crate::init::guided_init(&cwd)?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("Phase 1/3 — certify (read-only):");
                print_checks(&report.certify.checks);
                let Some(scaffold) = &report.scaffold else {
                    println!();
                    return Err(UsageError::Message(
                        "certification failed — fix the FAIL items above, then re-run \
                         `aethyme init` (nothing was written)"
                            .into(),
                    ));
                };
                println!();
                println!("Phase 2/3 — scaffold (deterministic, only-if-missing):");
                print_checks(&scaffold.checks);
                println!();
                println!("Phase 3/3 — gates draft (adaptive):");
                match &report.gates {
                    Some(gates) => print_checks(&gates.checks),
                    None => println!(
                        "{:<8} {:<28} .aethyme/gates.toml already present — drafting skipped",
                        "skip", "gates.draft"
                    ),
                }
                println!();
                let write_checks: Vec<&crate::init::Check> = scaffold
                    .checks
                    .iter()
                    .chain(report.gates.iter().flat_map(|g| g.checks.iter()))
                    .collect();
                let existing: Vec<&str> = write_checks
                    .iter()
                    .filter(|c| c.status == crate::init::CheckStatus::Pass)
                    .map(|c| c.id)
                    .collect();
                if !existing.is_empty() {
                    println!("Already existed (untouched): {}", existing.join(", "));
                }
                if report.changed {
                    println!("Created this run:");
                    for check in write_checks
                        .iter()
                        .filter(|c| c.status == crate::init::CheckStatus::Created)
                    {
                        println!("  - {} — {}", check.id, check.detail);
                    }
                } else {
                    println!(
                        "Nothing created — this repository was already set up \
                         (init is idempotent)."
                    );
                }
                println!("{}", init_next_steps_message());
            }
            if !report.certified() {
                return Err(UsageError::Message("initialization failed".into()));
            }
        }
        "certify" | "scaffold" => {
            let cwd = std::env::current_dir()
                .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
            let report = if subcommand == "certify" {
                crate::init::certify(&cwd)?
            } else {
                crate::init::scaffold(&cwd)?
            };
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_checks(&report.checks);
                println!();
                if report.certified() {
                    if subcommand == "certify" {
                        println!("Certified (read-only — nothing written).");
                    } else {
                        println!(
                            "Scaffolding done — review the drafts, then run `aethyme certify`."
                        );
                    }
                } else {
                    return Err(UsageError::Message("FAIL items above must be fixed".into()));
                }
            }
            if !report.certified() {
                return Err(UsageError::Message("certification failed".into()));
            }
        }
        "finish" => {
            let session = parsed
                .session
                .ok_or(UsageError::Message("finish requires --session <id>".into()))?;
            let mut broker = open_broker()?;
            let report = broker.finish(session)?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                render_finish_report(&report);
                if report.status == crate::FinishStatus::Blocked {
                    return Err(UsageError::Message("session is not ready to finish".into()));
                }
            }
        }
        "close" => {
            let session = parsed
                .session
                .ok_or(UsageError::Message("close requires --session <id>".into()))?;
            let mut broker = open_broker()?;
            broker.close(session)?;
            if parsed.json {
                println!("{}", serde_json::json!({ "closed": session }));
            } else {
                println!(
                    "Session {session} closed (state only — worktree untouched). \
                     Next task on the same worktree: `aethyme broker adopt --task \"...\"`."
                );
            }
        }
        "cleanup" => {
            let id: i64 = parsed
                .positional
                .first()
                .ok_or(UsageError::Message("cleanup requires a session id".into()))?
                .parse()
                .map_err(|_| UsageError::Message("session id must be an integer".into()))?;
            let mut broker = open_broker()?;
            broker.cleanup(id, parsed.force)?;
            if parsed.json {
                println!("{{\"cleaned\":{id}}}");
            } else {
                println!("Cleaned session {id}.");
            }
        }
        "-h" | "--help" => return Err(UsageError::Help),
        other => {
            return Err(UsageError::Message(format!(
                "unknown broker subcommand {other:?} — see `aethyme broker --help`"
            )));
        }
    }
    Ok(())
}
