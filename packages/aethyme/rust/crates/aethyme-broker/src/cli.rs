//! CLI for `aethyme broker ...` — a thin client of [`crate::Broker`].
//!
//! Owned by the broker crate (not the router binary) so the command
//! surface and the library evolve together; the `aethyme` router just
//! dispatches here. Contract: no logic beyond argument parsing and
//! rendering, and every command has a `--json` form whose shape comes
//! from the library's serializable types (#32).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::broker::Broker;

const RESOURCES_RECONCILE_USAGE: &str =
    "usage: aethyme broker resources reconcile <lease-id> --confirm <generation> [--json]";
const OPERATIONS_RECONCILE_USAGE: &str = "usage: aethyme broker operations reconcile \
     --operation <id> --outcome <succeeded|failed> --reason <text> [--json]";
const OPERATIONS_SHOW_USAGE: &str = "usage: aethyme broker operations show <id> [--json]";
const ADVISORIES_SHOW_USAGE: &str = "usage: aethyme broker advisories show <id> [--json]";
const ADVISORIES_ACK_USAGE: &str = "usage: aethyme broker advisories ack <id> [--json]";
const INTEGRATION_RECONCILE_USAGE: &str = "usage: aethyme broker integration reconcile \
     --upstream <ref> [--resolution-file <path>] [--write-resolution-template <path>] \
     [--dry-run | --apply --confirm <sha256>] [--json]";

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
  aethyme broker adopt [<path>] [--task <text>] [--path <repo-path>]... [--reuse [--sync-integration]|--replace-stale] [--json]
      Register an existing worktree (attach-first). Defaults to the
      current directory. If the worktree already has a session:
      --reuse points it at a follow-up task with a fresh baseline and
      reports its relation to the current integration tip;
      --sync-integration requires --reuse and first fast-forwards a clean
      session worktree to the exact integration tip;
      --replace-stale closes it (state only) and registers fresh;
      neither flag = error listing your options. Every --path is validated
      and claimed explicitly in the same transaction as create/reuse.
  aethyme broker close --session <id> [--json]
      Low-level state-only close. Never touches the worktree and does
      not check whether commits were submitted. Prefer finish for normal
      lifecycle use.
  aethyme broker finish --session <id> [--keep-worktree] [--json]
      Higher-level lifecycle close: closes only when the session has no
      dirty WIP and no committed work waiting for submit/promotion. If it
      is not safe, prints the next command; suggests cleanup only when
      cleanup would pass without --force. Successful closure atomically
      persists a redacted session.finished handoff with delivery, pending
      work, leases, last-gate provenance, and the recommended next action.
  aethyme broker handoff (--session <id> | --worktree <path>) [--json]
      Read the latest persisted session.finished handoff for one session,
      or the newest completed session registered to a worktree. Does not
      refresh sessions, leases, gates, events, or command telemetry.
  aethyme broker report capture --kind <bug|improvement> --title <text> [--session <id>] [--include-task] [--stdout | --output <filename>] [--json]
      Build an allowlist-only diagnostic snapshot entirely offline. By
      default, atomically writes a new JSON artifact beneath
      .aethyme/reports/ and prints its SHA-256 for review. --output accepts
      a filename (or .aethyme/reports/<filename>) without overwriting;
      --stdout emits the exact report bytes and prints the digest to stderr.
      Task text and coordinated-operation reasons are omitted unless
      --include-task is explicit.
  aethyme broker report list [--json]
      List valid captured reports newest-first with capture time, kind,
      Aethyme version, current SHA-256 digest, and filed/unfiled state.
      Invalid local artifacts are reported separately; no state is changed.
  aethyme broker report show <filename> [--json]
      Inspect one captured report by filename or its repository-relative
      .aethyme/reports/<filename> path. Recomputes the current digest and
      refuses symlinks, path escapes, oversized files, and invalid schemas.
  aethyme broker report render <filename> --form <form.yml> [--output <name>.issue.md] [--json]
      Render a captured report through one repository issue form, entirely
      offline. The form must be a .github/ISSUE_TEMPLATE/*.yml file. Known
      allowlisted report fields become Markdown sections in form order;
      unknown fields remain explicit unfilled sections. Exits non-zero after
      rendering when any required field is still unfilled. --output atomically
      creates an editable .aethyme/reports/*.issue.md review artifact.
  aethyme broker report file <path> --repo <owner/name> --confirm <sha256> [--json]
      File an exact reviewed `report render --output` artifact through the
      coordinated GitHub operation layer. Refuses digest drift and unresolved
      required fields. Successful issue URL/number metadata is journaled and
      recorded locally; ambiguous outcomes require explicit reconciliation and
      are never retried automatically.
  aethyme broker external-events ingest <normalized.json> [--json]
      Ingest one adapter-verified, digest-bound normalized event. The strict
      schema rejects provider payload fields and stores only allowlisted
      repository/PR/commit provenance. No listener or poller is started.
  aethyme broker external-events list [--all] [--json]
      List unresolved events newest-first (or the bounded full history).
  aethyme broker external-events show <id> [--json]
      Inspect one exact redacted event and its resolution state.
  aethyme broker external-events reconcile <id> --outcome <assign|ignore> --reason <text> [--session <id>] [--json]
      Resolve retained ambiguity explicitly. Assignment requires --session;
      unsupported or repository-mismatched events can only be ignored. The
      reason is stored as a SHA-256 digest, never as text.
  aethyme broker start --task <text> [--path <repo-path>]... [--json]
      Create a broker-managed worktree + branch and register a session,
      atomically claiming every reviewed --path, but do not spawn a process.
      Prefer this over adopting the main
      checkout for agent work; it isolates the git index and worktree.
  aethyme broker start-agent --task <text> --cmd <command> [--json]
      Create a worktree + branch and spawn <command> in it (sh -c),
      logging to .aethyme/logs/.
  aethyme broker worktree-root [--json]
      Resolve the scanner-safe external root used by future broker starts.
      Read-only: reports the preferred per-user location, repository key,
      and constrained legacy fallback without creating either directory.
  aethyme broker agents [--json]
      List live sessions with activity-derived liveness, refreshing
      diff-derived leases and warning on overlapping edits.
  aethyme broker leases [--json]
      Refresh and list active leases plus current overlaps.
  aethyme broker leases claim <path> --session <id> [--ttl <seconds>] [--json]
      Explicitly claim a path (end it with / for a directory claim).
  aethyme broker leases plan <paths...> [--session <id>] [--json]
      Read-only preflight for proposed claims. Reports exact and directory
      overlaps, owner liveness and worktree, expiry, valid next actions, and
      whether each claim conflicts.
      Does not create or refresh leases.
  aethyme broker leases export (--session <id> | --entry <id>) [--limit <n>] [--json]
      Export bounded, redacted lease ownership and deterministic routing
      categories from committed [leases.routing] configuration. Includes
      historical released and expired rows for the selected session. Never
      refreshes leases or writes broker state or command telemetry.
  aethyme broker leases release <path> --session <id> [--json]
      Release an explicit claim.
  aethyme broker resources plan <request.json> [--json]
      Read-only host-wide availability estimate for a typed resource bundle.
      Never reserves a port, namespace, capacity slot, or exclusive key.
  aethyme broker resources acquire <request.json> [--wait <duration>] [--grant-out <path>] [--json]
      Atomically reserve the full bundle. --wait bounds contention retries.
      --grant-out atomically creates a mode-0600 private grant and keeps the
      ownership token out of command output.
  aethyme broker resources run <request.json> [--wait <duration>] [--cleanup-command <shell>] [--json] -- <command> ...
      Acquire, expose only public allocations, supervise the process group,
      renew while it runs, execute optional exact cleanup, then release. Lost
      authority or unproven cleanup quarantines the bundle.
  aethyme broker resources renew <grant.json> --ttl <seconds> [--json]
  aethyme broker resources release <grant.json> [--json]
      Renew or release with the exact grant returned by acquire. Tokens are
      read from the file rather than command arguments or broker telemetry.
  aethyme broker resources list [--all] [--json]
      Read-only inventory. Ownership tokens are never included.
  aethyme broker resources reconcile <lease-id> --confirm <generation> [--json]
      Release an expired, quarantined allocation after reviewing host cleanup.
      The generation confirmation fences stale cleanup commands.
  aethyme broker exec --session <id> -- <command> [--json]
      Run a command in the session worktree, then fail if it creates or
      modifies dirty paths outside explicit leases or in adoption-time
      foreign files. Exports AETHYME_TEST_DB_SUFFIX=s<id>-exec.
  aethyme broker git --session <id> [--repo <owner/name>] [--scope <scope>] [--effect <read|write|destructive>] [--reason <text>] [--destructive] [--json] -- <git-args>
      Run Git through the durable operation coordinator. Remote Git commands
      require an exact --repo. Repository writes are serialized, journaled,
      and fail closed after a crash with an unknown remote outcome.
  aethyme broker gh --session <id> --repo <owner/name> [--scope <scope>] [--effect <read|write|destructive>] [--reason <text>] [--destructive] [--json] -- <gh-args>
      Run GitHub CLI through the same repository coordinator. The broker sets
      GH_REPO from the exact target and never persists command output or
      secret-bearing argument values. After a successful `gh pr merge`, it
      refreshes the tracked target and removes a fully landed integration
      layer; mixed or uncertain work remains unchanged with recovery guidance.
  aethyme broker operations list [--limit <n>] [--before <id>] [--session <id>] [--status <status>] [--repo <canonical-id>] [--provider <git|github>] [--json]
      List a filtered newest-first page of the durable operation journal.
      `operations` without `list` is a compatibility alias during deprecation.
  aethyme broker operations show <id> [--json]
      Show one exact durable operation and its reconciliation state, evidence,
      write barrier, and complete recovery commands when inspection is required.
  aethyme broker operations reconcile --operation <id> --outcome <succeeded|failed> --reason <text> [--json]
      Resolve a crash-ambiguous operation after independently inspecting the
      remote state. Overlapping writes remain blocked until reconciliation.
  aethyme broker advisories list [--all] [--json]
      List outstanding non-blocking advisories newest-first. --all includes
      acknowledged and publication-resolved history. The broker database is authoritative.
  aethyme broker advisories show <id> [--json]
      Show one exact advisory with paths, evidence, integration provenance,
      creation time, and resolution state.
  aethyme broker advisories ack <id> [--json]
      Acknowledge one advisory idempotently and atomically refresh
      .aethyme/broker-advisory.md from the remaining outstanding rows.
      Promotion/lease advisories repeat on session commands, after
      post-commit, and before uncached gates whose cost exceeds 1; they
      remain informational and never alter command or promotion outcomes.
  aethyme broker advisories metrics [--json]
      Inspect bounded, content-free shown-to-action correlation. Metrics
      never retain task text, command arguments, paths, evidence, or secrets.
  aethyme broker exposures plan [--json]
      Read-only reconciliation plan against the remote default branch's
      freshly advertised exact SHA. Normal status never performs this check.
  aethyme broker exposures apply --session <id> --confirm <sha256> [--json]
      Rebuild the plan, journal a second exact remote observation, and resolve
      only contained exposures and advisories without live lease overlap.
  aethyme broker note send --session <sender> --to-session <recipient> --message <text> [--json]
  aethyme broker note list --session <recipient> [--json]
  aethyme broker note ack --session <recipient> --id <note-id> [--json]
      Send, inspect, and acknowledge bounded repository-local coordination
      notes between live sessions. Unread notes surface on the recipient's
      next broker command; event payloads never contain message text.
  aethyme broker gates validate [--json]
      Parse and validate .aethyme/gates.toml.
  aethyme broker gates manifest [--head <ref>] [--json]
      Emit a versioned content-free gate-selection manifest from exact
      committed policy (default head: HEAD). Commands are never included.
  aethyme broker gates scope --base <ref> --head <ref> [--json]
      Evaluate the shared path selector for two exact commits using the
      gates.toml committed at head. Read-only; semantic hints stay advisory.
  aethyme broker gates affected --session <id> [--json]
      Show which gates the session's diff selects and why.
  aethyme broker gates semantic --session <id> [--json]
      Advisory semantic gate-selection report: shows enforced path-triggered
      gates plus caller-edge suggestion status. Never changes what submit,
      CI, or gates run execute.
  aethyme broker gates run --session <id> [--only <gate>] [--no-cache] [--json]
      Run affected gates cheap-first with tree-hash caching; cancels this
      session's obsolete in-flight runs; stops at first failure. Text
      abbreviates each proven tree hash; JSON includes the full hash.
      --no-cache executes fresh, then stores the new result normally.
  aethyme broker gates run --all [--only <gate>] [--no-cache] [--json]
      Run EVERY gate in cost order against the current worktree — no
      diff selection, no session. Same runner, streaming, and tree-hash
      result cache as session runs; stops at first failure and exits
      non-zero if any gate does not pass. The CI entrypoint: gates.toml
      is the single definition of verified for CI and broker alike. Text
      abbreviates each proven tree hash; JSON includes the full hash.
      --no-cache executes fresh, then stores the new result normally.
  aethyme broker gates pre-push <remote-name> [<remote-url>] [--no-cache] [--json]
      Opt-in adapter for a repository-owned Git pre-push hook. Reads Git's
      ref-update protocol from stdin, requires one clean current-HEAD tip,
      then runs every gate with the same cache and host-resource lifecycle.
      Deletion-only pushes need no content gates. Wire this adapter manually
      when a repository wants full pre-push gates in addition to the managed
      publication guard.
  aethyme broker hooks install [--json]
      Explicitly install the three managed git hooks into the shared
      <git-common-dir>/hooks (all worktrees see them): pre-commit runs
      a fail-closed session/upstream guard on protected branches whenever
      local broker state exists, then the cost<=1 gates whose triggers match
      the staged files. Repositories without local broker state remain a
      no-op for contributors who have not deployed Aethyme. Successful
      gates are silent; a failure replays its complete stdout/stderr,
      reports the diagnosis, preserves its exit code, and blocks the
      commit. Post-commit warns when the new commit touches files another
      live session is editing (informational — never blocks). Pre-push blocks
      direct protected/default-branch publication unless it runs inside a
      coordinated broker operation or carries an explicit journaled
      AETHYME_BROKER_BREAK_GLASS_REASON. Refuses to
      touch a hook file it does not own (no aethyme
      marker); with the marker, only the marker block is replaced. The
      hook shims embed this binary's absolute path.
  aethyme broker hooks uninstall [--json]
      Remove the aethyme marker blocks, deleting a hook file only when
      nothing but the shim remained. User content is preserved.
  aethyme broker hooks status [--json]
      Report installed/absent/foreign per managed hook.
      (hooks pre-commit / hooks post-commit / hooks pre-push are internal entry
      points the installed shims call — not for direct use.)
  aethyme broker pr check [--target <branch>] [--pr <number>] [--agent <name>] [--dispatch] [--cmd <command>] [--json]
  aethyme broker watch pr start --session <id> --repo <owner/name> --pr <number> [--events <comments,reviews,checks>] [--seconds <15..3600>] [--json]
  aethyme broker watch pr list [--all] [--json]
  aethyme broker watch pr show|poll|pause|resume|stop --id <watch-id> [--json]
  aethyme broker watch pr batches --id <watch-id> [--all] [--json]
  aethyme broker watch pr ack --id <batch-id> --outcome <addressed|stale|non-actionable|superseded> --reason <text> [--json]
      Persist a metadata-only PR watch and inspect it with one-shot polling.
      Aethyme records provider ids, authors, states, URLs and timestamps, but
      never comment/review bodies. Scheduling and live-agent delivery are
      separate adapter responsibilities.
  aethyme broker deliveries subscribe --watch <id> --adapter <name> --target <opaque-id> [--policy <notify|resume|review-and-push>] [--json]
  aethyme broker deliveries list [--adapter <name>] [--all] [--json]
  aethyme broker deliveries claim --adapter <name> --worker <id> [--seconds <15..900>] [--json]
  aethyme broker deliveries complete --id <delivery-id> --worker <id> --generation <n> --outcome <delivered|retry|failed> [--error-code <code>] [--json]
      Provider-neutral durable outbox. Claiming fences concurrent adapters;
      completion requires the exact worker and generation. Prompts contain
      allowlisted PR metadata, never comment or review bodies.
      Inspect the open PR for the current branch targeting <branch>
      (default: production). A thumbs-up marker in the PR body means all
      good and skips activity checks. A looking-eyes marker or no marker
      checks comments, reviews, and status checks; new actionable
      activity prepares a Push2prod prompt. With --dispatch, the broker
      attaches that prompt to an existing matching session when possible
      or spawns a Codex agent command.
  aethyme broker review register --session <id> --repo <owner/name> --pr <number> [--json]
      Opt an exact live session and open draft PR into the configured review
      lifecycle after verifying repository, base, and full head SHA evidence.
  aethyme broker review show --session <id> [--json]
      Read the exact persisted review state and next action without provider I/O.
  aethyme broker review request --session <id> [--json]
      After a successful broker submission, revalidate GitHub evidence and
      coordinate the idempotent ready-for-review write.
  aethyme broker review unlock --session <id> [--json]
      Poll and revalidate configured review/head/base/open evidence, then
      and run the configured validation-unlock adapter exactly once.
  aethyme broker review reassign --session <closed-id> --to-session <live-id> --reason <text> [--json]
      Move an active lifecycle from its closed owner to a live session whose
      HEAD exactly matches the lifecycle commit. State and evidence remain intact;
      only a SHA-256 digest of the reason is retained.
  aethyme broker review abandon --session <id> --reason <text> [--json]
      Explicitly retire a stuck lifecycle without deleting its audit history,
      freeing the PR for fresh registration. Only the reason digest is stored.
  aethyme broker submit --session <id> [--no-cache] [--json]
      Submit the session's head commit: simulate the merge onto the local
      integration branch, run affected gates on the merged tree, and
      promote when verified (default; set [promote] mode = 'manual' to
      hold verified entries for explicit promote). Conflicts reject
      before any gate runs and write instructions to
      <worktree>/.aethyme/broker-action-required.md. V1 submits the
      whole session head only; --path/--commit scoping is intentionally
      out of scope while worktree identity is the coordination unit.
      Executed and cached gate results identify the proven tree hash.
      --no-cache bypasses merged-tree cache lookup for this submission,
      but stores each fresh result for later normal reuse.
  aethyme broker repair --session <id> [--json]
      Conflict-scoped recovery: apply the documented local rebase path for
      the latest submit conflict, or rebase onto promoted integration work
      when status reports that conflict surface. Checkpoint divergence is
      handled by `broker checkpoint plan`, never by an implicit broad rebase.
      Then refresh leases and show affected gates. Never submits or
      promotes; run submit when the report is clean.
  aethyme broker checkpoint plan --session <id> [--json]
      Read-only recovery plan for a session whose accepted checkpoint is no
      longer an ancestor of its rewritten branch. Reports the old and proposed
      checkpoints, integration relation, pending commit provenance, safety
      refusals, preservation branch, and review digest.
  aethyme broker checkpoint apply --session <id> --confirm <sha256> [--json]
      Rebuild and confirm the exact recovery plan, create the preservation ref
      first, then atomically re-anchor the broker checkpoint. Never rewrites the
      session worktree or hides uncommitted work.
  aethyme broker queue [--json]
  aethyme broker queue history [--limit <n>] [--before <id>] [--json]
      The bare command remains the compatibility inventory. `history` is a
      bounded newest-first terminal page with a stable next_before_id cursor.
      Show the merge queue.
  aethyme broker promote --entry <id> [--json]
      Manual-mode only: advance the local integration branch to a verified
      entry's merge commit; other in-flight entries are re-simulated.
      Promotion stays local; publish through `broker ship plan --entry <id>`.
  aethyme broker ship plan --entry <id> [--json]
      Read-only publication plan through an exact promoted entry: resolve the
      selected prefix SHA, included and excluded later entries, current
      integration tip, remote freshness, proposed push, and local-main safety.
  aethyme broker ship execute --entry <id> --confirm <full-publication-sha> [--sync-main] [--break-glass --reason <authorization>] [--json]
      Fetch and revalidate the planned remote base, publish the exact confirmed
      promoted prefix with a non-force push, then verify the remote default ref.
      --sync-main additionally fast-forwards an unchanged primary checkout.
      A committed review-gated policy requires live exact-review evidence.
      Break-glass is available only when that committed policy opts in; the
      journal retains the reason digest, never the reason text.
      Tracked changes and incoming-path collisions block; unrelated untracked
      files are preserved and reported.
  aethyme broker integration status [--json]
      Focused promoted-but-unmerged view: the local integration branch as
      a pending layer above main, with promoted entries, files changed,
      live sessions conflicting with that layer, and the next action.
  aethyme broker integration wait-stable [--seconds <n>] [--json]
      Sample integration, wait for a quiet window (default: 30s), then
      sample again. Fails if integration moved, printing the old and new
      tips so long checks are not mistaken for current-tip proof.
  aethyme broker integration reconcile --upstream <ref> [--resolution-file <path>] [--write-resolution-template <path>] [--dry-run|--apply --confirm <sha256>] [--json]
      Compare already-fetched upstream with local main and promoted queue
      state. Dry-run is the default. --apply marks externally landed work,
      replays reviewed pending work, and rebuilds integration only when
      --confirm matches the dry-run plan digest. A resolution file binds
      queue attestations and per-SHA unrecorded-commit dispositions. The
      template option atomically writes a no-clobber schema-v2 document with
      exact identifiers and deliberately invalid null operator judgments.
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
      gone, orphaned gate pidfiles, and stale local product binaries when run
      inside the Aethyme source checkout. --fix-version is explicit and
      source-checkout-only: when the running CLI is behind integration, install
      and verify both aethyme and aethyme-engine-cli from that exact revision.
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
  aethyme broker cleanup --all-cleaned [--apply --confirm <sha256>] [--json]
      Remove one session worktree, or inventory all retained broker-owned
      worktrees from already-closed sessions. Bulk cleanup is a read-only plan
      by default; --apply revalidates and removes only clean worktrees whose
      session work is represented on main, integration, or configured upstream.
      Adopted worktrees are never included in the bulk sweep.
  aethyme broker gc plan [--json]
      Report exact retention-eligible rows, runtime files, represented
      worktrees/refs, estimated bytes, blockers, and a stable plan digest.
  aethyme broker gc apply --confirm <sha256> [--json]
      Apply or resume the exact reviewed plan under an exclusive lock. A
      recovery journal makes interrupted row, file, and worktree cleanup safe.
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
    run_with_mode(args, CompatibilityMode::Normal)
}

/// How the router permits this broker invocation to observe broker state.
/// Degraded repository compatibility uses `ReadOnlySnapshot`; ordinary
/// current-repository operation retains reconciliation behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatibilityMode {
    Normal,
    ReadOnlySnapshot,
}

pub fn run_with_mode(args: &[String], mode: CompatibilityMode) -> u8 {
    // Dispatched before the shared parser: the contract check is a CI/gate
    // entry point with its own flags (`--base`, `--pr-body`) and its own
    // exit-code contract (2 = bad invocation), and it deliberately records
    // no command metric — it runs on every submit and would swamp the
    // ledger with noise.
    if args.first().map(String::as_str) == Some("check-contract") {
        return crate::contract_check::run(&args[1..]);
    }
    let started = std::time::Instant::now();
    let (code, record_outcome) = match run_inner(args, mode) {
        Ok(()) => (0, true),
        Err(UsageError::Help) => {
            eprint!("{USAGE}");
            (2, false)
        }
        Err(UsageError::Message(message)) => {
            eprintln!("Error: {message}");
            (1, true)
        }
        Err(UsageError::Exit { message, code }) => {
            eprintln!("Error: {message}");
            (code, true)
        }
        Err(UsageError::SilentExit(code)) => (code, true),
    };
    let internal_hook = args.first().map(String::as_str) == Some("hooks")
        && matches!(
            args.get(1).map(String::as_str),
            Some("pre-commit" | "post-commit")
        );
    if mode == CompatibilityMode::Normal && !internal_hook {
        if record_outcome {
            record_command_outcome(args, code);
        }
        record_command_metric(args, code, started.elapsed().as_millis() as i64);
    }
    code
}

/// Safe-by-construction command telemetry: the label is built ONLY from
/// an allowlist of known subcommand words, so positional values (paths,
/// session ids, task text) can never leak into the metrics file. Best
/// effort — any failure is silently ignored.
const KNOWN_COMMAND_WORDS: &[&str] = &[
    "adopt",
    "start",
    "start-agent",
    "worktree-root",
    "exec",
    "git",
    "gh",
    "operations",
    "advisories",
    "exposures",
    "note",
    "send",
    "list",
    "ack",
    "reconcile",
    "agents",
    "leases",
    "export",
    "resources",
    "claim",
    "release",
    "gates",
    "draft",
    "validate",
    "manifest",
    "scope",
    "affected",
    "semantic",
    "run",
    "pre-push",
    "hooks",
    "install",
    "uninstall",
    "pre-commit",
    "post-commit",
    "pr",
    "check",
    "submit",
    "repair",
    "checkpoint",
    "apply",
    "queue",
    "promote",
    "ship",
    "plan",
    "execute",
    "sync-main",
    "sync-integration",
    "no-cache",
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
    "handoff",
    "report",
    "external-events",
    "deliveries",
    "subscribe",
    "complete",
    "review",
    "register",
    "request",
    "unlock",
    "reassign",
    "abandon",
    "ingest",
    "capture",
    "cleanup",
    "gc",
    "certify",
    "scaffold",
    "init",
];

fn safe_command_surface(args: &[String]) -> Option<String> {
    let first = args.first()?.as_str();
    if !KNOWN_COMMAND_WORDS.contains(&first) {
        return None;
    }
    let mut words = vec![first];
    if let Some(second) = args.get(1).map(String::as_str)
        && KNOWN_COMMAND_WORDS.contains(&second)
    {
        words.push(second);
    }
    Some(words.join("."))
}

fn record_command_outcome(args: &[String], exit: u8) {
    if !command_records_metric(args) {
        return;
    }
    let Some(surface) = safe_command_surface(args) else {
        return;
    };
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    let Ok(repo) = crate::GitRepo::discover(&cwd) else {
        return;
    };
    let Ok(main_root) = repo.main_root() else {
        return;
    };
    let Ok(mut store) = crate::BrokerStore::open_in_repo(&main_root) else {
        return;
    };
    let explicit_session = args
        .windows(2)
        .find(|pair| pair[0] == "--session")
        .and_then(|pair| pair[1].parse::<i64>().ok());
    let session_id = explicit_session.or_else(|| {
        store
            .session_for_worktree(repo.root().to_string_lossy().as_ref())
            .ok()
            .flatten()
            .map(|session| session.id)
    });
    let command_surface = format!("broker.{surface}");
    let failure_class = (exit != 0).then_some(match args.first().map(String::as_str) {
        Some("submit") => "submission_failed",
        Some("repair") => "recovery_failed",
        Some("git" | "gh") => "coordinated_operation_failed",
        _ => "command_failed",
    });
    let payload = crate::events::broker_command_outcome_payload(
        &command_surface,
        exit,
        failure_class,
        None,
        None,
    );
    let kind = if exit == 0 {
        crate::events::BROKER_COMMAND_SUCCEEDED
    } else {
        crate::events::BROKER_COMMAND_FAILED
    };
    let _ = store.append_event(kind, session_id, Some(&payload));
}

fn record_command_metric(args: &[String], exit: u8, duration_ms: i64) {
    if !command_records_metric(args) {
        return;
    }
    let Some(label) = safe_command_surface(args) else {
        return;
    };
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
        label
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
        Some("certify" | "queue" | "metrics" | "handoff" | "worktree-root") => false,
        Some("advisories") => args.get(1).map(String::as_str) == Some("ack"),
        Some("exposures") => args.get(1).map(String::as_str) == Some("apply"),
        Some("report") => args.get(1).map(String::as_str) == Some("file"),
        Some("external-events") => matches!(
            args.get(1).map(String::as_str),
            Some("ingest" | "reconcile")
        ),
        Some("review") => !matches!(args.get(1).map(String::as_str), Some("show")),
        Some("ship") => args.get(1).map(String::as_str) != Some("plan"),
        Some("checkpoint") => args.get(1).map(String::as_str) == Some("apply"),
        Some("gc") => args.get(1).map(String::as_str) == Some("apply"),
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
        Some("leases") => !matches!(args.get(1).map(String::as_str), Some("plan" | "export")),
        Some("resources") => !matches!(args.get(1).map(String::as_str), Some("plan" | "list")),
        Some("events") => args.get(1).map(String::as_str) == Some("prune"),
        Some("gates") => !matches!(
            args.get(1).map(String::as_str),
            Some("validate" | "manifest" | "scope" | "affected" | "semantic")
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
    fn status_queue_projection_separates_current_from_terminal_states() {
        for status in [
            crate::MergeStatus::Submitted,
            crate::MergeStatus::Simulating,
            crate::MergeStatus::Conflict,
            crate::MergeStatus::Verified,
        ] {
            assert!(super::queue_status_is_current(status));
        }
        for status in [
            crate::MergeStatus::Promoted,
            crate::MergeStatus::ExternallyLanded,
            crate::MergeStatus::Rejected,
            crate::MergeStatus::Superseded,
        ] {
            assert!(!super::queue_status_is_current(status));
        }
    }

    #[test]
    fn telemetry_classification_tracks_semantic_mutability() {
        for command in [
            args(&["certify"]),
            args(&["hooks", "status"]),
            args(&["leases", "plan", "src/lib.rs"]),
            args(&["queue"]),
            args(&["events"]),
            args(&["events", "--follow"]),
            args(&["metrics"]),
            args(&["handoff", "--session", "7"]),
            args(&["handoff", "--worktree", "."]),
            args(&["report", "capture"]),
            args(&["report", "list"]),
            args(&["report", "show", "report.json"]),
            args(&["report", "render", "report.json"]),
            args(&["checkpoint", "plan", "--session", "7"]),
            args(&["gates", "validate"]),
            args(&["gates", "affected", "--session", "7"]),
            args(&["gates", "semantic", "--session", "7"]),
            args(&["doctor"]),
            args(&["gc", "plan"]),
            args(&["operations"]),
            args(&["advisories", "list"]),
            args(&["advisories", "show", "1"]),
            args(&["external-events", "list"]),
            args(&["external-events", "show", "1"]),
            args(&["review", "show", "--session", "7"]),
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
            args(&["gc", "apply", "--confirm", "digest"]),
            args(&["report", "file", "reviewed.issue.md"]),
            args(&[
                "checkpoint",
                "apply",
                "--session",
                "7",
                "--confirm",
                "digest",
            ]),
            args(&["status"]),
            args(&["agents"]),
            args(&["leases"]),
            args(&["integration", "status"]),
            args(&["operations", "reconcile", "--operation", "1"]),
            args(&["advisories", "ack", "1"]),
            args(&["external-events", "ingest", "event.json"]),
            args(&[
                "external-events",
                "reconcile",
                "1",
                "--outcome",
                "ignore",
                "--reason",
                "not-applicable",
            ]),
            args(&[
                "review",
                "register",
                "--session",
                "7",
                "--repo",
                "o/r",
                "--pr",
                "1",
            ]),
            args(&["review", "request", "--session", "7"]),
            args(&["review", "unlock", "--session", "7"]),
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
    fn parse_accepts_read_only_exact_gate_scope_evaluation() {
        let parsed = match super::parse(&args(&[
            "gates",
            "scope",
            "--base",
            "refs/heads/main",
            "--head",
            "feature",
            "--json",
        ])) {
            Ok(parsed) => parsed,
            Err(_) => panic!("exact gate scope should parse"),
        };
        assert_eq!(parsed.positional, vec!["gates", "scope"]);
        assert_eq!(parsed.base.as_deref(), Some("refs/heads/main"));
        assert_eq!(parsed.head.as_deref(), Some("feature"));
        assert!(parsed.json);
        assert!(!super::command_records_metric(&args(&[
            "gates", "scope", "--base", "main", "--head", "feature"
        ])));
        assert!(!super::command_records_metric(&args(&[
            "gates", "manifest", "--head", "feature"
        ])));
    }

    #[test]
    fn parse_accepts_gc_plan_and_confirmed_apply() {
        let Ok(plan) = super::parse(&args(&["plan", "--json"])) else {
            panic!("gc plan should parse");
        };
        assert_eq!(plan.positional, vec!["plan"]);
        assert!(plan.json);

        let Ok(apply) = super::parse(&args(&["apply", "--confirm", "aabb"])) else {
            panic!("gc apply should parse");
        };
        assert_eq!(apply.positional, vec!["apply"]);
        assert_eq!(apply.confirm.as_deref(), Some("aabb"));
    }

    #[test]
    fn parse_accepts_checkpoint_recovery_confirmation() {
        let parsed = super::parse(&args(&[
            "checkpoint",
            "apply",
            "--session",
            "7",
            "--confirm",
            "aabb",
            "--json",
        ]))
        .unwrap_or_else(|_| panic!("checkpoint recovery should parse"));
        assert_eq!(parsed.positional, vec!["checkpoint", "apply"]);
        assert_eq!(parsed.session, Some(7));
        assert_eq!(parsed.confirm.as_deref(), Some("aabb"));
        assert!(parsed.json);
    }

    #[test]
    fn parse_accepts_read_only_lease_plan_with_multiple_paths() {
        let parsed = match super::parse(&args(&[
            "leases",
            "plan",
            "src/lib.rs",
            "docs/",
            "--session",
            "7",
            "--json",
        ])) {
            Ok(parsed) => parsed,
            Err(_) => panic!("lease plan should parse"),
        };
        assert_eq!(
            parsed.positional,
            vec!["leases", "plan", "src/lib.rs", "docs/"]
        );
        assert_eq!(parsed.session, Some(7));
        assert!(parsed.json);
        assert!(!super::command_records_metric(&args(&[
            "leases",
            "plan",
            "src/lib.rs"
        ])));
    }

    #[test]
    fn parse_accepts_repeated_planned_session_paths() {
        let parsed = match super::parse(&args(&[
            "--task",
            "rewrite policies",
            "--path",
            "generated/",
            "--path",
            "AGENTS.md",
            "--json",
        ])) {
            Ok(parsed) => parsed,
            Err(_) => panic!("planned paths should parse"),
        };
        assert_eq!(
            parsed.planned_paths,
            vec!["generated/".to_string(), "AGENTS.md".to_string()]
        );
        assert!(parsed.json);
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
    fn parse_accepts_read_only_handoff_selectors() {
        let by_session = match super::parse(&args(&["handoff", "--session", "7", "--json"])) {
            Ok(parsed) => parsed,
            Err(_) => panic!("session handoff should parse"),
        };
        assert_eq!(by_session.session, Some(7));
        assert!(by_session.worktree.is_none());
        assert!(by_session.json);

        let by_worktree =
            match super::parse(&args(&["handoff", "--worktree", ".aethyme/worktrees/task"])) {
                Ok(parsed) => parsed,
                Err(_) => panic!("worktree handoff should parse"),
            };
        assert!(by_worktree.session.is_none());
        assert_eq!(
            by_worktree.worktree.as_deref(),
            Some(std::path::Path::new(".aethyme/worktrees/task"))
        );
        assert!(!super::command_records_metric(&args(&[
            "handoff",
            "--session",
            "7",
        ])));
    }

    #[test]
    fn parse_accepts_offline_report_capture_outputs() {
        let parsed = match super::parse(&args(&[
            "capture",
            "--kind",
            "bug",
            "--title",
            "Gate failed",
            "--session",
            "7",
            "--include-task",
            "--output",
            "reviewed.json",
        ])) {
            Ok(parsed) => parsed,
            Err(_) => panic!("report capture should parse"),
        };
        assert_eq!(parsed.positional, vec!["capture"]);
        assert_eq!(parsed.kind.as_deref(), Some("bug"));
        assert_eq!(parsed.title.as_deref(), Some("Gate failed"));
        assert_eq!(parsed.session, Some(7));
        assert!(parsed.include_task);
        assert_eq!(
            parsed.output.as_deref(),
            Some(std::path::Path::new("reviewed.json"))
        );
        assert!(!super::command_records_metric(&args(&[
            "report",
            "capture",
            "--kind",
            "bug",
            "--title",
            "Gate failed",
        ])));
    }

    #[test]
    fn parse_accepts_confirmed_ship_execution() {
        let sha = "a".repeat(40);
        let parsed = match super::parse(&args(&[
            "ship",
            "execute",
            "--entry",
            "42",
            "--confirm",
            &sha,
            "--sync-main",
            "--break-glass",
            "--reason",
            "approved emergency publication",
        ])) {
            Ok(parsed) => parsed,
            Err(_) => panic!("ship execute should parse"),
        };
        assert_eq!(parsed.positional, vec!["ship", "execute"]);
        assert_eq!(parsed.entry, Some(42));
        assert_eq!(parsed.confirm.as_deref(), Some(sha.as_str()));
        assert!(parsed.sync_main);
        assert!(parsed.break_glass);
        assert_eq!(
            parsed.reason.as_deref(),
            Some("approved emergency publication")
        );
        assert!(super::command_records_metric(&args(&[
            "ship",
            "execute",
            "--entry",
            "42",
            "--confirm",
            &sha,
        ])));
    }

    #[test]
    fn init_next_steps_names_deploy_before_quick_test_start_and_submit() {
        let message = super::init_next_steps_message();
        let deploy = message.find("aethyme deploy --repo .").unwrap();
        let quick_test = message.find("aethyme broker quick-test").unwrap();
        let start = message.find("aethyme broker start").unwrap();
        let submit = message
            .find("aethyme broker submit --session <id>")
            .unwrap();
        assert!(deploy < quick_test);
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
            "--write-resolution-template".to_string(),
            "reconciliation-template.json".to_string(),
            "--apply".to_string(),
            "--confirm".to_string(),
            "a".repeat(64),
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
        assert_eq!(
            parsed.write_resolution_template.as_deref(),
            Some(std::path::Path::new("reconciliation-template.json"))
        );
        assert!(parsed.apply);
        assert_eq!(parsed.confirm, Some("a".repeat(64)));
    }

    #[test]
    fn resolution_template_write_is_atomic_and_never_clobbers() {
        let tmp = tempfile::tempdir().unwrap();
        let output = tmp.path().join("resolution.json");
        let document = crate::IntegrationReconcileResolutionTemplateDocument {
            schema_version: 2,
            upstream_ref: "origin/main".into(),
            upstream_commit: "a".repeat(40),
            old_integration: "b".repeat(40),
            operator: None,
            resolutions: Vec::new(),
            unrecorded_resolutions: Vec::new(),
        };

        assert!(super::write_reconciliation_resolution_template(&output, &document).is_ok());
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
        assert_eq!(value["schema_version"], 2);
        assert!(value["operator"].is_null());
        let error = match super::write_reconciliation_resolution_template(&output, &document) {
            Err(super::UsageError::Message(error)) => error,
            _ => panic!("second write should return a no-clobber usage error"),
        };
        assert!(error.contains("refusing to overwrite"), "{error}");
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

    #[test]
    fn parse_accepts_metadata_only_pull_request_watch_flags() {
        let args = vec![
            "pr".to_string(),
            "start".to_string(),
            "--session".to_string(),
            "17".to_string(),
            "--repo".to_string(),
            "Owner/Repo".to_string(),
            "--pr".to_string(),
            "42".to_string(),
            "--events".to_string(),
            "comments,reviews".to_string(),
            "--seconds".to_string(),
            "90".to_string(),
        ];
        let parsed = match super::parse(&args) {
            Ok(parsed) => parsed,
            Err(_) => panic!("watch flags should parse"),
        };
        assert_eq!(parsed.positional, vec!["pr", "start"]);
        assert_eq!(parsed.session, Some(17));
        assert_eq!(parsed.repository.as_deref(), Some("Owner/Repo"));
        assert_eq!(parsed.pr_number, Some(42));
        assert_eq!(parsed.events.as_deref(), Some("comments,reviews"));
        assert_eq!(parsed.seconds, Some(90));
    }

    #[test]
    fn parse_accepts_provider_neutral_delivery_claim_fence() {
        let args = vec![
            "complete".to_string(),
            "--id".to_string(),
            "12".to_string(),
            "--worker".to_string(),
            "chau7-main".to_string(),
            "--generation".to_string(),
            "3".to_string(),
            "--outcome".to_string(),
            "retry".to_string(),
            "--error-code".to_string(),
            "tab_busy".to_string(),
        ];
        let parsed = match super::parse(&args) {
            Ok(parsed) => parsed,
            Err(_) => panic!("delivery completion fence should parse"),
        };
        assert_eq!(parsed.positional, vec!["complete"]);
        assert_eq!(parsed.note_id, Some(12));
        assert_eq!(parsed.worker.as_deref(), Some("chau7-main"));
        assert_eq!(parsed.generation, Some(3));
        assert_eq!(parsed.outcome.as_deref(), Some("retry"));
        assert_eq!(parsed.error_code.as_deref(), Some("tab_busy"));
    }

    #[test]
    fn upstream_relation_names_both_sides_of_divergence() {
        assert_eq!(
            super::upstream_relation(35, 213),
            "diverged: 35 local-only commits, 213 upstream-only commits"
        );
        assert_eq!(
            super::upstream_relation(0, 1),
            "local main behind by 1 commit"
        );
        assert_eq!(
            super::upstream_relation(2, 0),
            "local main ahead by 2 commits"
        );
        assert_eq!(
            super::upstream_relation(0, 0),
            "fetched upstream matches local main"
        );
    }
}

enum UsageError {
    Help,
    Message(String),
    Exit { message: String, code: u8 },
    SilentExit(u8),
}

impl<E: std::fmt::Display> From<E> for UsageError {
    fn from(err: E) -> Self {
        UsageError::Message(err.to_string())
    }
}

struct Parsed {
    read_only_snapshot: bool,
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
    to_session: Option<i64>,
    note_id: Option<i64>,
    message: Option<String>,
    entry: Option<i64>,
    confirm: Option<String>,
    operation: Option<i64>,
    before: Option<i64>,
    limit: Option<u32>,
    status: Option<String>,
    provider: Option<String>,
    adapter: Option<String>,
    worker: Option<String>,
    policy: Option<String>,
    error_code: Option<String>,
    watch_id: Option<i64>,
    generation: Option<i64>,
    events: Option<String>,
    ttl_seconds: Option<i64>,
    wait: Option<String>,
    since: Option<i64>,
    kind: Option<String>,
    keep_days: Option<i64>,
    seconds: Option<u64>,
    upstream: Option<String>,
    base: Option<String>,
    head: Option<String>,
    resolution_file: Option<PathBuf>,
    write_resolution_template: Option<PathBuf>,
    worktree: Option<PathBuf>,
    title: Option<String>,
    output: Option<PathBuf>,
    grant_out: Option<PathBuf>,
    cleanup_command: Option<String>,
    form: Option<PathBuf>,
    follow: bool,
    json: bool,
    force: bool,
    check: bool,
    dispatch: bool,
    reuse: bool,
    replace_stale: bool,
    all: bool,
    all_cleaned: bool,
    keep_worktree: bool,
    chau7: bool,
    fix_version: bool,
    with_gate: bool,
    apply: bool,
    dry_run: bool,
    destructive: bool,
    break_glass: bool,
    sync_main: bool,
    sync_integration: bool,
    no_cache: bool,
    only: Option<String>,
    stdout: bool,
    include_task: bool,
    planned_paths: Vec<String>,
    exec_command: Vec<String>,
}

fn parse(args: &[String]) -> Result<Parsed, UsageError> {
    let mut parsed = Parsed {
        read_only_snapshot: false,
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
        to_session: None,
        note_id: None,
        message: None,
        entry: None,
        confirm: None,
        operation: None,
        before: None,
        limit: None,
        status: None,
        provider: None,
        adapter: None,
        worker: None,
        policy: None,
        error_code: None,
        watch_id: None,
        generation: None,
        events: None,
        ttl_seconds: None,
        wait: None,
        since: None,
        kind: None,
        keep_days: None,
        seconds: None,
        upstream: None,
        base: None,
        head: None,
        resolution_file: None,
        write_resolution_template: None,
        worktree: None,
        title: None,
        output: None,
        grant_out: None,
        cleanup_command: None,
        form: None,
        follow: false,
        json: false,
        force: false,
        check: false,
        dispatch: false,
        reuse: false,
        replace_stale: false,
        all: false,
        all_cleaned: false,
        keep_worktree: false,
        chau7: false,
        fix_version: false,
        with_gate: false,
        apply: false,
        dry_run: false,
        destructive: false,
        break_glass: false,
        sync_main: false,
        sync_integration: false,
        no_cache: false,
        only: None,
        stdout: false,
        include_task: false,
        planned_paths: Vec::new(),
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
            "--before" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--before requires a value".into()))?;
                parsed.before = Some(value.parse().map_err(|_| {
                    UsageError::Message("--before must be an integer operation id".into())
                })?);
            }
            "--limit" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--limit requires a value".into()))?;
                parsed.limit = Some(value.parse().map_err(|_| {
                    UsageError::Message("--limit must be a positive integer".into())
                })?);
            }
            "--force" => parsed.force = true,
            "--check" => parsed.check = true,
            "--dispatch" => parsed.dispatch = true,
            "--reuse" => parsed.reuse = true,
            "--all" => parsed.all = true,
            "--all-cleaned" => parsed.all_cleaned = true,
            "--keep-worktree" => parsed.keep_worktree = true,
            "--chau7" => parsed.chau7 = true,
            "--fix-version" => parsed.fix_version = true,
            "--with-gate" => parsed.with_gate = true,
            "--apply" => parsed.apply = true,
            "--dry-run" => parsed.dry_run = true,
            "--destructive" => parsed.destructive = true,
            "--break-glass" => parsed.break_glass = true,
            "--sync-main" => parsed.sync_main = true,
            "--sync-integration" => parsed.sync_integration = true,
            "--no-cache" => parsed.no_cache = true,
            "--only" => {
                parsed.only = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--only requires a gate name".into()))?
                        .clone(),
                )
            }
            "--stdout" => parsed.stdout = true,
            "--wait" => {
                parsed.wait = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--wait requires a duration".into()))?
                        .clone(),
                )
            }
            "--grant-out" => {
                parsed.grant_out = Some(PathBuf::from(
                    iter.next()
                        .ok_or(UsageError::Message("--grant-out requires a path".into()))?,
                ))
            }
            "--cleanup-command" => {
                parsed.cleanup_command = Some(
                    iter.next()
                        .ok_or(UsageError::Message(
                            "--cleanup-command requires a shell command".into(),
                        ))?
                        .clone(),
                )
            }
            "--include-task" => parsed.include_task = true,
            "--replace-stale" => parsed.replace_stale = true,
            "--path" => parsed.planned_paths.push(
                iter.next()
                    .ok_or(UsageError::Message(
                        "--path requires a repository-relative path".into(),
                    ))?
                    .clone(),
            ),
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
            "--base" => {
                parsed.base = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--base requires a ref".into()))?
                        .clone(),
                )
            }
            "--head" => {
                parsed.head = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--head requires a ref".into()))?
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
            "--write-resolution-template" => {
                parsed.write_resolution_template = Some(PathBuf::from(iter.next().ok_or(
                    UsageError::Message("--write-resolution-template requires a path".into()),
                )?))
            }
            "--worktree" => {
                parsed.worktree = Some(PathBuf::from(
                    iter.next()
                        .ok_or(UsageError::Message("--worktree requires a path".into()))?
                        .clone(),
                ))
            }
            "--title" => {
                parsed.title = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--title requires a value".into()))?
                        .clone(),
                )
            }
            "--output" => {
                parsed.output = Some(PathBuf::from(
                    iter.next()
                        .ok_or(UsageError::Message("--output requires a path".into()))?
                        .clone(),
                ))
            }
            "--form" => {
                parsed.form = Some(PathBuf::from(
                    iter.next()
                        .ok_or(UsageError::Message("--form requires a path".into()))?
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
            "--status" => {
                parsed.status = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--status requires a value".into()))?
                        .clone(),
                )
            }
            "--provider" => {
                parsed.provider = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--provider requires a value".into()))?
                        .clone(),
                )
            }
            "--adapter" => {
                parsed.adapter = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--adapter requires a value".into()))?
                        .clone(),
                )
            }
            "--worker" => {
                parsed.worker = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--worker requires a value".into()))?
                        .clone(),
                )
            }
            "--policy" => {
                parsed.policy = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--policy requires a value".into()))?
                        .clone(),
                )
            }
            "--error-code" => {
                parsed.error_code = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--error-code requires a value".into()))?
                        .clone(),
                )
            }
            "--watch" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--watch requires a value".into()))?;
                parsed.watch_id = Some(value.parse().map_err(|_| {
                    UsageError::Message("--watch must be an integer watch id".into())
                })?);
            }
            "--generation" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--generation requires a value".into()))?;
                parsed.generation =
                    Some(value.parse().map_err(|_| {
                        UsageError::Message("--generation must be an integer".into())
                    })?);
            }
            "--events" => {
                parsed.events = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--events requires a value".into()))?
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
            "--to-session" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--to-session requires a value".into()))?;
                parsed.to_session = Some(value.parse().map_err(|_| {
                    UsageError::Message("--to-session must be an integer session id".into())
                })?);
            }
            "--id" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--id requires a value".into()))?;
                parsed.note_id =
                    Some(value.parse().map_err(|_| {
                        UsageError::Message("--id must be an integer note id".into())
                    })?);
            }
            "--message" => {
                parsed.message = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--message requires a value".into()))?
                        .clone(),
                );
            }
            "--entry" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--entry requires a value".into()))?;
                parsed.entry = Some(value.parse().map_err(|_| {
                    UsageError::Message("--entry must be an integer queue entry id".into())
                })?);
            }
            "--confirm" => {
                parsed.confirm = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--confirm requires a value".into()))?
                        .clone(),
                )
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

const GATE_FAILURE_TAIL_LINES: usize = 20;
const GATE_FAILURE_TAIL_BYTES: usize = 16 * 1024;

fn render_gate_failure_tail(outcome: &crate::gates::GateRunOutcome) {
    if outcome.status == crate::GateStatus::Pass {
        return;
    }
    let Some(path) = outcome.log_path.as_deref() else {
        return;
    };
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let start = bytes.len().saturating_sub(GATE_FAILURE_TAIL_BYTES);
    let text = String::from_utf8_lossy(&bytes[start..]);
    let mut lines = text
        .lines()
        .rev()
        .take(GATE_FAILURE_TAIL_LINES)
        .collect::<Vec<_>>();
    lines.reverse();
    if lines.is_empty() {
        return;
    }
    eprintln!(
        "gate {} output (last {} line(s)):",
        outcome.gate,
        lines.len()
    );
    for line in lines {
        eprintln!("  {line}");
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

fn render_lease_plan(report: &crate::LeasePlan, json: bool) -> Result<(), UsageError> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }

    for path in &report.paths {
        println!(
            "{} — {}",
            path.path,
            if path.would_conflict {
                "would conflict"
            } else {
                "clear"
            }
        );
        for (label, overlaps) in [("owned", &path.owned), ("conflict", &path.conflicts)] {
            for overlap in overlaps {
                println!(
                    "  {label:<8} {:<9} session {:<4} {:<9} {} (expires {}; owner {} at {})",
                    match overlap.relation {
                        crate::LeaseOverlapRelation::Exact => "exact",
                        crate::LeaseOverlapRelation::Directory => "directory",
                    },
                    overlap.session_id,
                    overlap.kind.as_str(),
                    overlap.path,
                    overlap
                        .expires_at
                        .map(|expiry| expiry.to_string())
                        .unwrap_or_else(|| "never".to_string()),
                    overlap.owner_status.as_str(),
                    overlap.owner_worktree,
                );
                if label == "conflict" {
                    for action in &overlap.safe_next_actions {
                        println!("    next: {action}");
                    }
                }
            }
        }
        if path.owned.is_empty() && path.conflicts.is_empty() {
            println!("  no active overlaps");
        }
    }
    Ok(())
}

fn render_planned_explicit_leases(leases: &[crate::Lease]) {
    if leases.is_empty() {
        return;
    }
    println!("Planned explicit leases:");
    for lease in leases {
        println!("  {}", lease.path);
    }
}

fn render_worktree_placement(placement: &crate::WorktreePlacement) {
    let boundary = if placement.outside_repository {
        "outside the repository"
    } else {
        "inside the repository fallback"
    };
    println!(
        "Worktree root: {} ({}, {boundary})",
        placement.root.display(),
        placement.source.as_str()
    );
    if let Some(reason) = &placement.fallback_reason {
        println!("Warning: external worktree placement was unavailable: {reason}");
    }
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
                "    {} {}{} (tree {})",
                outcome.gate,
                outcome.status.as_str(),
                if outcome.cached { " (cached)" } else { "" },
                short_commit(&outcome.tree_hash),
            );
        }
        println!(
            "  failing entry: q{} ({})",
            gate.failing_entry_id,
            gate.failing_entry_status.as_str()
        );
        for outcome in &gate.failing_outcomes {
            println!(
                "    {} {}{} (tree {})",
                outcome.gate,
                outcome.status.as_str(),
                if outcome.cached { " (cached)" } else { "" },
                short_commit(&outcome.tree_hash),
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
    "First-time flow: install -> `aethyme deploy --repo .` -> `aethyme broker hooks install` -> `aethyme broker quick-test` -> \
     `aethyme broker start --task \"...\"` -> `aethyme broker submit --session <id>`.\n\
     This low-level init configured broker state only. Run `aethyme deploy --repo .` \
     now to install mandatory agent policy and certify the complete deployment."
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

fn queue_status_is_current(status: crate::MergeStatus) -> bool {
    matches!(
        status,
        crate::MergeStatus::Submitted
            | crate::MergeStatus::Simulating
            | crate::MergeStatus::Conflict
            | crate::MergeStatus::Verified
    )
}

fn render_queue_history(page: &crate::MergeQueueHistoryPage) {
    if page.entries.is_empty() {
        println!("No terminal merge-queue entries in this page.");
    } else {
        println!("{:<4} {:<4} {:<17} HEAD", "ID", "SID", "STATUS");
        for entry in &page.entries {
            println!(
                "{:<4} {:<4} {:<17} {}",
                entry.id,
                entry.session_id,
                entry.status.as_str(),
                short_commit(&entry.head_commit)
            );
        }
    }
    let summary = page
        .terminal_counts
        .iter()
        .map(|item| format!("{} {}", item.status.as_str(), item.count))
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "Terminal totals: {}",
        if summary.is_empty() { "none" } else { &summary }
    );
    if let Some(before) = page.next_before_id {
        println!("Next: aethyme broker queue history --before {before}");
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
    if report.pending_commits.is_empty() {
        println!("  pending commits: none");
    } else {
        println!("  pending commits:");
        for commit in &report.pending_commits {
            println!("    - {commit}");
        }
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
    println!(
        "  delivery: submitted={}, promoted={}, published={}",
        if report.delivery.submitted {
            "yes"
        } else {
            "no"
        },
        if report.delivery.promoted {
            "yes"
        } else {
            "no"
        },
        if report.delivery.published {
            "yes"
        } else {
            "no"
        },
    );
    if !report.dirty_paths.is_empty() {
        println!("  dirty paths: {}", capped_join(&report.dirty_paths, 8));
    }
    if report.unsubmitted_commits > 0 {
        println!("  unsubmitted commits: {}", report.unsubmitted_commits);
    }
    println!(
        "  pending work: {} ({} dirty paths, {} unsubmitted commits{})",
        if report.pending_work.present {
            "yes"
        } else {
            "no"
        },
        report.pending_work.dirty_path_count,
        report.pending_work.unsubmitted_commits,
        if report.pending_work.worktree_missing {
            ", worktree missing"
        } else {
            ""
        },
    );
    if report.leases_held.is_empty() {
        println!("  leases held: none recorded");
    } else {
        println!("  leases held:");
        for lease in &report.leases_held {
            println!(
                "    {} {} {} (expires {}, released {})",
                lease.kind.as_str(),
                match lease.state {
                    crate::FinishLeaseState::Active => "active",
                    crate::FinishLeaseState::Released => "released",
                    crate::FinishLeaseState::Expired => "expired",
                },
                lease.path,
                lease
                    .expires_at
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "never".into()),
                lease
                    .released_at
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "never".into()),
            );
        }
    }
    match &report.last_gate {
        Some(gate) => println!(
            "  last gate: {} {} on tree {} at {} ({})",
            gate.gate,
            gate.status.as_str(),
            short_commit(&gate.tree_hash),
            gate.recorded_at,
            match gate.cache_source {
                crate::FinishGateCacheSource::Executed => "executed",
                crate::FinishGateCacheSource::CacheHit => "cache hit",
            }
        ),
        None => println!("  last gate: none recorded"),
    }
    println!(
        "  cleanup safe: {}",
        if report.cleanup_safe { "yes" } else { "no" }
    );
    println!(
        "  physical cleanup: requested={}, kept={}, attempted={}, completed={}, reclaimed={} bytes",
        report.cleanup.requested,
        report.cleanup.kept,
        report.cleanup.attempted,
        report.cleanup.completed,
        report.cleanup.reclaimed_bytes,
    );
    println!(
        "    worktree: {} ({})",
        report.worktree_path,
        if report.cleanup.worktree_removed {
            "removed"
        } else {
            "retained"
        }
    );
    if let Some(branch) = &report.cleanup.branch_ref {
        println!(
            "    branch: {}{} ({})",
            branch,
            report
                .cleanup
                .branch_tip
                .as_deref()
                .map(|tip| format!(" at {tip}"))
                .unwrap_or_default(),
            if report.cleanup.branch_removed {
                "removed"
            } else {
                "retained"
            }
        );
    }
    if let Some(action) = &report.cleanup.recovery_action {
        println!("    recovery: {action}");
    }
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
    println!(
        "  recommended next: {}",
        report.recommended_next_action.as_deref().unwrap_or("none")
    );
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0_usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn render_cleanup_sweep_report(report: &crate::CleanupSweepReport) {
    println!(
        "Cleanup {}: {} retained broker-owned worktrees, {} eligible",
        if report.applied { "apply" } else { "plan" },
        report.plan.retained_worktree_count,
        report.plan.eligible_worktree_count,
    );
    println!(
        "  retained: {}; reclaimable now: {}; branches: {} retained, {} eligible",
        human_bytes(report.plan.estimated_retained_bytes),
        human_bytes(report.plan.estimated_reclaimable_bytes),
        report.plan.retained_branch_count,
        report.plan.eligible_branch_count,
    );
    println!("  reviewed plan digest: {}", report.plan.digest);
    for item in &report.plan.worktrees {
        println!(
            "  session {}: {} ({}) — {}",
            item.session_id,
            item.disposition.as_str(),
            item.estimated_bytes
                .map(human_bytes)
                .unwrap_or_else(|| "size unavailable".into()),
            item.reason,
        );
        println!("    {}", item.worktree_path);
        if let Some(branch_tip) = &item.branch_tip {
            println!("    {} at {}", item.branch_ref, branch_tip);
        }
        for command in &item.inspection_commands {
            println!("    inspect: {command}");
        }
        if !item.eligible() {
            println!("    explicit discard: {}", item.force_cleanup_command);
        }
    }
    if report.applied {
        println!(
            "  removed: {}",
            if report.removed_session_ids.is_empty() {
                "none".into()
            } else {
                report
                    .removed_session_ids
                    .iter()
                    .map(i64::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
        for failure in &report.failures {
            println!(
                "  retained session {} after revalidation: {}",
                failure.session_id, failure.reason
            );
        }
    } else if report.plan.eligible_worktree_count > 0 || report.plan.eligible_branch_count > 0 {
        println!(
            "  apply: aethyme broker cleanup --all-cleaned --apply --confirm {}",
            report.plan.digest
        );
    }
}

fn render_gc_plan(plan: &crate::GcPlan) {
    println!(
        "GC plan {}: {} rows, {} files, {} represented worktrees, {} reclaimable",
        plan.digest,
        plan.rows.len(),
        plan.files.len(),
        plan.worktrees.len(),
        human_bytes(plan.estimated_reclaimable_bytes),
    );
    for row in &plan.rows {
        println!(
            "  row: {:?} {} at {} ({} bytes)",
            row.kind, row.id, row.recorded_at, row.estimated_bytes
        );
    }
    for file in &plan.files {
        println!(
            "  file: {:?} {} ({} -> {} bytes; before {})",
            file.action, file.path, file.bytes_before, file.bytes_after, file.before_sha256
        );
    }
    for worktree in &plan.worktrees {
        println!(
            "  worktree: session {} {} ({} bytes)",
            worktree.session_id, worktree.worktree_path, worktree.estimated_bytes
        );
        println!(
            "    ref: {} at {}",
            worktree.branch_ref,
            worktree.branch_tip.as_deref().unwrap_or("missing")
        );
    }
    for blocker in &plan.blockers {
        println!(
            "  protected: {}{} — {}",
            blocker.kind,
            blocker.id.map(|id| format!(" {id}")).unwrap_or_default(),
            blocker.reason
        );
    }
    if plan.rows.is_empty() && plan.files.is_empty() && plan.worktrees.is_empty() {
        println!("  apply: nothing eligible");
    } else {
        println!("  apply: aethyme broker gc apply --confirm {}", plan.digest);
    }
}

fn render_gc_apply(report: &crate::GcApplyReport) {
    println!(
        "GC apply {}: {} rows, {} files, {} worktrees, {} reclaimed",
        if report.complete {
            "complete"
        } else {
            "paused"
        },
        report.rows_removed,
        report.files_completed.len(),
        report.sessions_cleaned.len(),
        human_bytes(report.reclaimed_bytes),
    );
    for failure in &report.failures {
        println!("  retained: {failure}");
    }
    if let Some(action) = &report.recovery_action {
        println!("  recovery: {action}");
    }
}

fn render_handoff_report(report: &crate::SessionHandoffReport) {
    let handoff = &report.handoff;
    println!(
        "Session {} handoff: {} (event {} at {})",
        handoff.session_id,
        handoff.status.as_str(),
        report.event_id,
        report.recorded_at
    );
    if let Some(entry_id) = handoff.latest_queue_entry_id {
        let status = handoff
            .latest_queue_status
            .map(|status| status.as_str())
            .unwrap_or("unknown");
        println!("  latest queue: qid {entry_id} ({status})");
    }
    println!(
        "  delivery: submitted={}, promoted={}, published={}",
        if handoff.delivery.submitted {
            "yes"
        } else {
            "no"
        },
        if handoff.delivery.promoted {
            "yes"
        } else {
            "no"
        },
        if handoff.delivery.published {
            "yes"
        } else {
            "no"
        },
    );
    println!(
        "  pending work: {} ({} dirty paths, {} unsubmitted commits{})",
        if handoff.pending_work.present {
            "yes"
        } else {
            "no"
        },
        handoff.pending_work.dirty_path_count,
        handoff.pending_work.unsubmitted_commits,
        if handoff.pending_work.worktree_missing {
            ", worktree missing"
        } else {
            ""
        },
    );
    let active = handoff
        .leases_held
        .iter()
        .filter(|lease| lease.state == crate::FinishLeaseState::Active)
        .count();
    let released = handoff
        .leases_held
        .iter()
        .filter(|lease| lease.state == crate::FinishLeaseState::Released)
        .count();
    let expired = handoff
        .leases_held
        .iter()
        .filter(|lease| lease.state == crate::FinishLeaseState::Expired)
        .count();
    println!(
        "  leases: {} recorded ({} active, {} released, {} expired)",
        handoff.leases_held.len(),
        active,
        released,
        expired
    );
    match &handoff.last_gate {
        Some(gate) => println!(
            "  last gate: {} {} on tree {} at {} ({})",
            gate.gate,
            gate.status.as_str(),
            short_commit(&gate.tree_hash),
            gate.recorded_at,
            match gate.cache_source {
                crate::FinishGateCacheSource::Executed => "executed",
                crate::FinishGateCacheSource::CacheHit => "cache hit",
            }
        ),
        None => println!("  last gate: none recorded"),
    }
    println!(
        "  cleanup safe: {}",
        if handoff.cleanup_safe { "yes" } else { "no" }
    );
    println!(
        "  next: {}",
        handoff.recommended_next_action.as_deref().unwrap_or("none")
    );
}

fn resolve_handoff_worktree(path: &std::path::Path) -> Result<PathBuf, UsageError> {
    if path.exists() {
        return Ok(crate::GitRepo::discover(path)?.root().to_path_buf());
    }
    let mut existing = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| UsageError::Message(format!("cannot resolve cwd: {error}")))?
            .join(path)
    };
    let mut missing_tail = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name() else {
            return Ok(existing);
        };
        missing_tail.push(name.to_os_string());
        if !existing.pop() {
            return Ok(existing);
        }
    }
    let mut resolved = existing.canonicalize().unwrap_or(existing);
    for name in missing_tail.into_iter().rev() {
        resolved.push(name);
    }
    Ok(resolved)
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
    if !report.semantic.impacted_paths.is_empty() {
        println!(
            "  semantic impact paths: {}",
            capped_join(&report.semantic.impacted_paths, 8)
        );
    }
    if report.semantic.truncated {
        println!(
            "  semantic impact result: truncated at {} paths",
            report.semantic.result_limit
        );
    }

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
            match &gate.chain {
                Some(chain) => println!(
                    "    - {} ({} -> {} -> {})",
                    gate.gate, chain.changed_file, chain.caller_file, chain.suggested_gate
                ),
                None => match &gate.triggered_by {
                    Some(path) => println!("    - {} (via {})", gate.gate, path),
                    None => println!("    - {} ({})", gate.gate, gate.reason),
                },
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

fn upstream_relation(local_only: u64, upstream_only: u64) -> String {
    match (local_only, upstream_only) {
        (0, 0) => "fetched upstream matches local main".into(),
        (local, 0) => format!(
            "local main ahead by {local} {}",
            plural(local as usize, "commit", "commits")
        ),
        (0, upstream) => format!(
            "local main behind by {upstream} {}",
            plural(upstream as usize, "commit", "commits")
        ),
        (local, upstream) => format!(
            "diverged: {local} local-only {}, {upstream} upstream-only {}",
            plural(local as usize, "commit", "commits"),
            plural(upstream as usize, "commit", "commits")
        ),
    }
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
        let relation = upstream_relation(
            report.main_ahead_upstream_commits,
            report.main_behind_upstream_commits,
        );
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

    if let Some(reconciliation) = &report.reconciliation {
        println!(
            "Reconciliation evidence: {} landed, {} ambiguous, {} unresolved, {} unrecorded",
            reconciliation.landed_entry_count,
            reconciliation.ambiguous_entry_count,
            reconciliation.unresolved_entry_count,
            reconciliation.unrecorded_commits.len()
        );
        println!("  {}", reconciliation.explanation);
    }

    println!("Delivery state: {}", report.next_action.state.as_str());
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
    println!("Publication prefix: {}", report.publication_sha);
    println!(
        "Included entries: {}",
        report
            .included_entries
            .iter()
            .map(|entry| format!("q{}@{}", entry.queue_entry_id, entry.promotion_sha))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if !report.excluded_entries.is_empty() {
        println!(
            "Excluded later entries: {}",
            report
                .excluded_entries
                .iter()
                .map(|entry| format!("q{}@{}", entry.queue_entry_id, entry.promotion_sha))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    println!(
        "Local default:  {} @ {}",
        report.local_default_branch_ref, report.local_default_branch_sha
    );
    println!(
        "Remote default: {}/{} @ {}",
        report.target.remote_name,
        report.remote_default_branch_ref,
        report.remote_default_branch_sha
    );
    println!(
        "Target: {} ({})",
        report.target.display_slug, report.target.normalized_host
    );
    println!("Freshness: {:?}", report.freshness.result);
    println!("Proposed push: {}", report.proposed_push.command.join(" "));
    println!(
        "Publication policy: {:?} (evidence {})",
        report.publication_policy.policy.mode,
        if report.publication_policy.satisfied {
            "satisfied"
        } else {
            "missing or stale"
        }
    );
    for evidence in &report.publication_policy.evidence {
        println!(
            "  q{} session {}: {} ({})",
            evidence.queue_entry_id,
            evidence.session_id,
            if evidence.covered {
                "covered"
            } else {
                "not covered"
            },
            evidence.reason
        );
    }
    if let Some(remediation) = &report.publication_policy.remediation {
        println!("Publication remediation: {remediation}");
    }
    println!(
        "Local-main synchronization safe now: {}",
        if report.local_main_sync_safe {
            "yes"
        } else {
            "no"
        }
    );
    let assessment = &report.local_main_sync_assessment;
    if !assessment.tracked_dirty_paths.is_empty() {
        println!(
            "Blocking tracked paths: {}",
            assessment.tracked_dirty_paths.join(", ")
        );
    }
    if !assessment.conflicting_untracked_paths.is_empty() {
        println!(
            "Blocking untracked collisions: {}",
            assessment.conflicting_untracked_paths.join(", ")
        );
    } else if !assessment.untracked_paths.is_empty() {
        println!(
            "Unrelated untracked paths preserved: {}",
            assessment.untracked_paths.join(", ")
        );
    }
    println!(
        "Confirm with: aethyme broker ship execute --entry {} --confirm {}",
        report.queue_entry.id, report.publication_sha
    );
    Ok(())
}

fn render_ship_execution(
    report: &crate::ShipExecutionReport,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    println!(
        "Published {} to {}/{}.",
        report.published_sha, report.plan.target.remote_name, report.plan.remote_default_branch_ref
    );
    println!("Verified remote SHA: {}", report.verified_remote_sha);
    println!(
        "Publication authorization: {:?}",
        report.publication_authorization.kind
    );
    if let Some(digest) = &report.publication_authorization.reason_digest {
        println!("Break-glass reason SHA-256: {digest}");
    }
    println!(
        "Operations: fetch {}, push {}, verify {}",
        report.fetch_operation.id, report.push_operation.id, report.verify_operation.id
    );
    if report.local_main_sync.synchronized {
        println!(
            "Local main synchronized: {} -> {}",
            report.local_main_sync.before_sha, report.local_main_sync.after_sha
        );
    } else if let Some(command) = &report.local_main_sync.follow_up_command {
        println!("Local main unchanged. To synchronize it explicitly:");
        println!("  {command}");
    }
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
    if let Some(digest) = &report.plan_digest {
        println!("Plan digest: {digest}");
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
    if let Some(template) = &report.resolution_template {
        println!(
            "Resolution template: {} recorded, {} unrecorded ({})",
            template.document.resolutions.len(),
            template.document.unrecorded_resolutions.len(),
            if template.complete {
                "complete"
            } else {
                "operator input required"
            }
        );
        println!(
            "  recorded classification: {}",
            template
                .field_contract
                .recorded_classification_allowed_values
                .join(", ")
        );
        for rule in &template.field_contract.unrecorded_dispositions {
            println!(
                "  {}: upstream_commit {}; {}",
                rule.value, rule.upstream_commit, rule.condition
            );
        }
        println!("  operator: {}", template.field_contract.operator);
        println!("  reason: {}", template.field_contract.reason);
    }
    for warning in &report.warnings {
        println!("Warning: {warning}");
    }
    println!("Next action: {}", report.next_action);
    Ok(())
}

fn write_reconciliation_resolution_template(
    path: &std::path::Path,
    document: &crate::IntegrationReconcileResolutionTemplateDocument,
) -> Result<(), UsageError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let parent = parent.unwrap_or_else(|| std::path::Path::new("."));
    if !parent.is_dir() {
        return Err(UsageError::Message(format!(
            "resolution template parent directory does not exist: {}",
            parent.display()
        )));
    }
    let mut bytes = serde_json::to_vec_pretty(document)?;
    bytes.push(b'\n');
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        UsageError::Message(format!(
            "cannot create resolution template beside {}: {error}",
            path.display()
        ))
    })?;
    temporary.write_all(&bytes).map_err(|error| {
        UsageError::Message(format!(
            "cannot write resolution template {}: {error}",
            path.display()
        ))
    })?;
    temporary.as_file().sync_all().map_err(|error| {
        UsageError::Message(format!(
            "cannot sync resolution template {}: {error}",
            path.display()
        ))
    })?;
    temporary.persist_noclobber(path).map_err(|error| {
        UsageError::Message(format!(
            "refusing to overwrite resolution template {}: {}",
            path.display(),
            error.error
        ))
    })?;
    Ok(())
}

fn open_broker(read_only_snapshot: bool) -> Result<Broker, UsageError> {
    let cwd = std::env::current_dir()
        .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
    if read_only_snapshot {
        Ok(Broker::open_snapshot(&cwd)?)
    } else {
        Ok(Broker::open(&cwd)?)
    }
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

fn operation_history_query(parsed: &Parsed) -> Result<crate::OperationHistoryQuery, UsageError> {
    let limit = parsed
        .limit
        .unwrap_or(crate::DEFAULT_OPERATION_HISTORY_LIMIT);
    if limit == 0 || limit > crate::MAX_OPERATION_HISTORY_LIMIT {
        return Err(UsageError::Message(format!(
            "--limit must be between 1 and {}",
            crate::MAX_OPERATION_HISTORY_LIMIT
        )));
    }
    if parsed.before.is_some_and(|id| id <= 0) {
        return Err(UsageError::Message(
            "--before must be a positive operation id".into(),
        ));
    }
    let status = parsed
        .status
        .as_deref()
        .map(|value| {
            crate::OperationStatus::parse(value).map_err(|_| {
                UsageError::Message(
                    "--status must be prepared, running, succeeded, failed, outcome_unknown, reconciled_succeeded, or reconciled_failed".into(),
                )
            })
        })
        .transpose()?;
    let provider = parsed
        .provider
        .as_deref()
        .map(|value| {
            crate::OperationProvider::parse(value)
                .map_err(|_| UsageError::Message("--provider must be git or github".into()))
        })
        .transpose()?;
    Ok(crate::OperationHistoryQuery {
        limit,
        before_id: parsed.before,
        session_id: parsed.session,
        status,
        repository: parsed.repository.clone(),
        provider,
    })
}

fn operations_reconcile_error(detail: impl std::fmt::Display) -> UsageError {
    UsageError::Message(format!(
        "{detail}\noperations reconcile requires every field: --operation <id>, --outcome <succeeded|failed>, and --reason <text>.\n{OPERATIONS_RECONCILE_USAGE}"
    ))
}

fn advisory_text(value: &str) -> String {
    serde_json::to_string(value).expect("serializing advisory text cannot fail")
}

fn render_advisory(advisory: &crate::Advisory) {
    println!(
        "Advisory {}: {} [{} / {}]",
        advisory.id,
        advisory_text(&advisory.identity),
        advisory.severity.as_str(),
        advisory.resolution_state.as_str(),
    );
    println!(
        "Session: {}",
        advisory
            .session_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".into())
    );
    println!(
        "Queue entry: {}",
        advisory
            .queue_entry_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".into())
    );
    println!(
        "Integration SHA: {}",
        advisory.integration_sha.as_deref().unwrap_or("none")
    );
    println!("Created: {}", advisory.created_at);
    println!(
        "Acknowledged: {}",
        advisory
            .acknowledged_at
            .map(|time| time.to_string())
            .unwrap_or_else(|| "no".into())
    );
    println!(
        "Resolved: {}",
        advisory
            .resolved_at
            .map(|time| time.to_string())
            .unwrap_or_else(|| "no".into())
    );
    if let Some(evidence) = advisory.resolution_evidence.as_deref() {
        println!("Resolution evidence: {}", advisory_text(evidence));
    }
    if !advisory.paths.is_empty() {
        println!("Paths:");
        for path in &advisory.paths {
            println!("  - {}", advisory_text(path));
        }
    }
    if !advisory.evidence.is_empty() {
        println!("Evidence:");
        for evidence in &advisory.evidence {
            println!(
                "  - {}: {}",
                advisory_text(&evidence.kind),
                advisory_text(&evidence.summary)
            );
        }
    }
    if advisory.resolution_state == crate::AdvisoryResolutionState::Outstanding {
        println!("Acknowledge: aethyme broker advisories ack {}", advisory.id);
    }
}

fn parse_advisory_id(value: Option<&String>, usage: &str) -> Result<i64, UsageError> {
    let id = value
        .ok_or_else(|| UsageError::Message(usage.into()))?
        .parse::<i64>()
        .map_err(|_| {
            UsageError::Message(format!("advisory id must be a positive integer; {usage}"))
        })?;
    if id <= 0 {
        return Err(UsageError::Message(format!(
            "advisory id must be a positive integer; {usage}"
        )));
    }
    Ok(id)
}

fn run_pull_request_watch(parsed: Parsed) -> Result<(), UsageError> {
    if parsed.positional.first().map(String::as_str) != Some("pr") {
        return Err(UsageError::Message(
            "watch requires `pr` followed by start, list, show, poll, batches, ack, pause, resume, or stop"
                .into(),
        ));
    }
    let action = parsed
        .positional
        .get(1)
        .map(String::as_str)
        .ok_or_else(|| {
            UsageError::Message(
                "watch pr requires start, list, show, poll, batches, ack, pause, resume, or stop"
                    .into(),
            )
        })?;
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    match action {
        "start" => {
            let session = parsed.session.ok_or_else(|| {
                UsageError::Message("watch pr start requires --session <id>".into())
            })?;
            let repository = parsed.repository.as_deref().ok_or_else(|| {
                UsageError::Message("watch pr start requires --repo <owner/name>".into())
            })?;
            let pr_number = parsed.pr_number.ok_or_else(|| {
                UsageError::Message("watch pr start requires --pr <number>".into())
            })?;
            let event_kinds = parse_pull_request_event_kinds(parsed.events.as_deref())?;
            let watch = broker.start_pull_request_watch(
                session,
                repository,
                pr_number,
                event_kinds,
                parsed
                    .seconds
                    .unwrap_or(crate::DEFAULT_PR_WATCH_INTERVAL_SECONDS),
                &crate::GithubCliPullRequestWatchProvider,
                now_ms(),
            )?;
            render_pull_request_watch(&watch, parsed.json)?;
        }
        "list" => {
            let watches = broker.pull_request_watches(parsed.all)?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&watches)?);
            } else if watches.is_empty() {
                println!("No pull request watches.");
            } else {
                for watch in watches {
                    render_pull_request_watch(&watch, false)?;
                }
            }
        }
        "show" => {
            let id = parsed.note_id.ok_or_else(|| {
                UsageError::Message("watch pr show requires --id <watch-id>".into())
            })?;
            render_pull_request_watch(&broker.pull_request_watch(id)?, parsed.json)?;
        }
        "poll" => {
            let id = parsed.note_id.ok_or_else(|| {
                UsageError::Message("watch pr poll requires --id <watch-id>".into())
            })?;
            let report = broker.poll_pull_request_watch(
                id,
                &crate::GithubCliPullRequestWatchProvider,
                now_ms(),
            )?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "Watch {} polled {}#{} at {}: {} metadata item(s), {}.",
                    report.watch.id,
                    report.watch.display_repository,
                    report.watch.pr_number,
                    short_commit(&report.watch.head_sha),
                    report.activity_count,
                    if report.changed {
                        if report.new_activity_count > 0 {
                            "new activity batched"
                        } else {
                            "metadata changed"
                        }
                    } else {
                        "no change"
                    },
                );
            }
        }
        "batches" => {
            let watch_id = parsed.note_id.ok_or_else(|| {
                UsageError::Message("watch pr batches requires --id <watch-id>".into())
            })?;
            let batches = broker.pull_request_activity_batches(watch_id, parsed.all)?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&batches)?);
            } else if batches.is_empty() {
                println!("No pull request activity batches.");
            } else {
                for batch in batches {
                    println!(
                        "Batch {}: watch {}, {} metadata item(s), {} at {}",
                        batch.id,
                        batch.watch_id,
                        batch.activities.len(),
                        batch.status.as_str(),
                        short_commit(&batch.head_sha),
                    );
                }
            }
        }
        "ack" => {
            let batch_id = parsed.note_id.ok_or_else(|| {
                UsageError::Message("watch pr ack requires --id <batch-id>".into())
            })?;
            let outcome = match parsed.outcome.as_deref() {
                Some("addressed") => crate::PullRequestBatchAckOutcome::Addressed,
                Some("stale") => crate::PullRequestBatchAckOutcome::Stale,
                Some("non-actionable" | "non_actionable") => {
                    crate::PullRequestBatchAckOutcome::NonActionable
                }
                Some("superseded") => crate::PullRequestBatchAckOutcome::Superseded,
                _ => {
                    return Err(UsageError::Message(
                        "watch pr ack requires --outcome addressed|stale|non-actionable|superseded"
                            .into(),
                    ));
                }
            };
            let reason = parsed.reason.as_deref().ok_or_else(|| {
                UsageError::Message("watch pr ack requires --reason <text>".into())
            })?;
            let batch = broker.acknowledge_pull_request_activity_batch(
                batch_id,
                outcome,
                reason,
                now_ms(),
            )?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&batch)?);
            } else {
                println!("Batch {} acknowledged as {}.", batch.id, outcome.as_str());
            }
        }
        "pause" | "resume" | "stop" => {
            let id = parsed.note_id.ok_or_else(|| {
                UsageError::Message(format!("watch pr {action} requires --id <watch-id>"))
            })?;
            let status = match action {
                "pause" => crate::PullRequestWatchStatus::Paused,
                "resume" => crate::PullRequestWatchStatus::Active,
                "stop" => crate::PullRequestWatchStatus::Stopped,
                _ => unreachable!(),
            };
            let watch = broker.set_pull_request_watch_status(id, status, now_ms())?;
            render_pull_request_watch(&watch, parsed.json)?;
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown watch pr action {other:?} — expected start, list, show, poll, batches, ack, pause, resume, or stop"
            )));
        }
    }
    Ok(())
}

fn run_deliveries(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .ok_or_else(|| {
            UsageError::Message("deliveries requires subscribe, list, claim, or complete".into())
        })?;
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    match action {
        "subscribe" => {
            let watch_id = parsed.watch_id.ok_or_else(|| {
                UsageError::Message("deliveries subscribe requires --watch <id>".into())
            })?;
            let adapter = parsed.adapter.as_deref().ok_or_else(|| {
                UsageError::Message("deliveries subscribe requires --adapter <name>".into())
            })?;
            let target = parsed.target.as_deref().ok_or_else(|| {
                UsageError::Message("deliveries subscribe requires --target <opaque-id>".into())
            })?;
            let policy = parse_delivery_policy(parsed.policy.as_deref())?;
            let subscription = broker.subscribe_pull_request_delivery(
                watch_id,
                adapter,
                target,
                policy,
                now_ms(),
            )?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&subscription)?);
            } else {
                println!(
                    "Delivery subscription {}: watch {}, adapter {}, target {}, policy {}.",
                    subscription.id,
                    subscription.watch_id,
                    subscription.adapter,
                    subscription.target,
                    subscription.policy.as_str(),
                );
            }
        }
        "list" => {
            let items = broker.delivery_outbox(parsed.adapter.as_deref(), parsed.all)?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else if items.is_empty() {
                println!("No delivery outbox items.");
            } else {
                for item in items {
                    println!(
                        "Delivery {}: batch {}, subscription {}, {}, generation {}, attempts {}",
                        item.id,
                        item.batch_id,
                        item.subscription_id,
                        item.status.as_str(),
                        item.generation,
                        item.attempt_count,
                    );
                }
            }
        }
        "claim" => {
            let adapter = parsed.adapter.as_deref().ok_or_else(|| {
                UsageError::Message("deliveries claim requires --adapter <name>".into())
            })?;
            let worker = parsed.worker.as_deref().ok_or_else(|| {
                UsageError::Message("deliveries claim requires --worker <id>".into())
            })?;
            let report = broker.claim_next_delivery(
                adapter,
                worker,
                parsed
                    .seconds
                    .unwrap_or(crate::DEFAULT_DELIVERY_CLAIM_SECONDS),
                now_ms(),
            )?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else if let Some(delivery) = report.delivery {
                println!(
                    "Claimed delivery {} generation {} for {}. Use --json to read its structured envelope and prompt.",
                    delivery.item.id, delivery.item.generation, delivery.subscription.target,
                );
            } else {
                println!("No pending delivery for adapter {adapter}.");
            }
        }
        "complete" => {
            let id = parsed.note_id.ok_or_else(|| {
                UsageError::Message("deliveries complete requires --id <delivery-id>".into())
            })?;
            let worker = parsed.worker.as_deref().ok_or_else(|| {
                UsageError::Message("deliveries complete requires --worker <id>".into())
            })?;
            let generation = parsed.generation.ok_or_else(|| {
                UsageError::Message("deliveries complete requires --generation <n>".into())
            })?;
            let completion = match parsed.outcome.as_deref() {
                Some("delivered") => crate::DeliveryCompletion::Delivered,
                Some("retry") => crate::DeliveryCompletion::Retry,
                Some("failed") => crate::DeliveryCompletion::Failed,
                _ => {
                    return Err(UsageError::Message(
                        "deliveries complete requires --outcome delivered|retry|failed".into(),
                    ));
                }
            };
            let item = broker.complete_delivery(
                id,
                worker,
                generation,
                completion,
                parsed.error_code.as_deref(),
                now_ms(),
            )?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&item)?);
            } else {
                println!("Delivery {} is {}.", item.id, item.status.as_str());
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown deliveries action {other:?} — expected subscribe, list, claim, or complete"
            )));
        }
    }
    Ok(())
}

fn parse_delivery_policy(value: Option<&str>) -> Result<crate::DeliveryPolicy, UsageError> {
    match value.unwrap_or("notify") {
        "notify" => Ok(crate::DeliveryPolicy::Notify),
        "resume" => Ok(crate::DeliveryPolicy::Resume),
        "review-and-push" | "review_and_push" => Ok(crate::DeliveryPolicy::ReviewAndPush),
        value => Err(UsageError::Message(format!(
            "unknown delivery policy {value:?}; expected notify, resume, or review-and-push"
        ))),
    }
}

fn parse_pull_request_event_kinds(
    value: Option<&str>,
) -> Result<Vec<crate::PullRequestActivityKind>, UsageError> {
    let value = value.unwrap_or("comments,reviews,checks");
    let mut kinds = Vec::new();
    for item in value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let kind = match item {
            "comment" | "comments" => crate::PullRequestActivityKind::Comment,
            "review" | "reviews" => crate::PullRequestActivityKind::Review,
            "check" | "checks" => crate::PullRequestActivityKind::Check,
            _ => {
                return Err(UsageError::Message(format!(
                    "unknown pull request event kind {item:?}; expected comments, reviews, or checks"
                )));
            }
        };
        kinds.push(kind);
    }
    if kinds.is_empty() {
        return Err(UsageError::Message(
            "--events must select comments, reviews, or checks".into(),
        ));
    }
    kinds.sort();
    kinds.dedup();
    Ok(kinds)
}

fn render_pull_request_watch(
    watch: &crate::PullRequestWatch,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        println!("{}", serde_json::to_string_pretty(watch)?);
    } else {
        println!(
            "Watch {}: {}#{} {} at {} (session {}, every {}s)",
            watch.id,
            watch.display_repository,
            watch.pr_number,
            watch.status.as_str(),
            short_commit(&watch.head_sha),
            watch.session_id,
            watch.poll_interval_seconds,
        );
    }
    Ok(())
}

fn render_operation_show(report: &crate::OperationShowReport) {
    let operation = &report.operation;
    println!("Operation:      {}", operation.id);
    println!("Session:        {}", operation.session_id);
    println!("Provider:       {}", operation.provider.as_str());
    println!("Repository:     {}", operation.repository);
    println!("Scope:          {}", operation.scope);
    println!("Effect:         {}", operation.effect.as_str());
    println!("Status:         {}", operation.status.as_str());
    println!("Identity:       {}", operation.identity_provenance.as_str());
    println!("Command:        {}", operation.command_json);
    println!(
        "Host operation: {}",
        operation.host_operation_id.as_deref().unwrap_or("none")
    );
    println!("Reconciliation: {}", report.reconciliation.state.as_str());
    println!(
        "Write blocked:  {}",
        if report.reconciliation.write_blocked {
            "yes"
        } else {
            "no"
        }
    );
    println!("Automatic retry: forbidden");
    if let Some(evidence) = &report.reconciliation.evidence {
        println!("Evidence:       {evidence}");
    }
    if let Some(reason) = &report.reconciliation.operator_reason {
        println!("Operator reason: {reason}");
    }
    if let Some(recovery) = &report.reconciliation.recovery {
        println!("Inspect:        {}", recovery.inspection);
        println!("If succeeded:   {}", recovery.succeeded_command);
        println!("If failed:      {}", recovery.failed_command);
        println!("Blind retry is forbidden until reconciliation is recorded.");
    }
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
        if let Some(cleanup) = &report.post_merge_cleanup {
            println!(
                "post-merge integration cleanup: {} — {}",
                cleanup.state.as_str(),
                cleanup.explanation
            );
            if let Some(operation_id) = cleanup.fetch_operation_id {
                println!("  upstream refresh operation: {operation_id}");
            }
            if let Some(command) = &cleanup.next_action {
                println!("  next: {command}");
            }
        }
    }
    Ok(())
}

fn run_review(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .ok_or_else(|| {
            UsageError::Message(
                "review requires register, show, request, unlock, reassign, or abandon".into(),
            )
        })?;
    if parsed.positional.len() != 1 {
        return Err(UsageError::Message(format!(
            "review {action} accepts no positional arguments"
        )));
    }
    let session_id = parsed
        .session
        .ok_or_else(|| UsageError::Message(format!("review {action} requires --session <id>")))?;
    match action {
        "register" => {
            let repository = parsed.repository.as_deref().ok_or_else(|| {
                UsageError::Message(
                    "review register requires --session <id> --repo <owner/name> --pr <number>"
                        .into(),
                )
            })?;
            let pr_number = parsed.pr_number.ok_or_else(|| {
                UsageError::Message(
                    "review register requires --session <id> --repo <owner/name> --pr <number>"
                        .into(),
                )
            })?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let policy = crate::ReviewPolicy::load(broker.main_root())?;
            let session = broker.store().session(session_id)?;
            let snapshot = crate::load_review_provider_snapshot(
                Path::new(&session.worktree_path),
                repository,
                pr_number,
                &policy,
            )?;
            let report =
                broker.register_review_lifecycle(session_id, repository, &snapshot, now_ms())?;
            render_review_report(&report, parsed.json)?;
        }
        "show" => {
            let mut broker = open_broker(true)?;
            let lifecycle = broker
                .store()
                .review_lifecycle_for_session(session_id)?
                .ok_or(crate::BrokerError::ReviewLifecycleNotFound(session_id))?;
            let policy = crate::ReviewPolicy::load(broker.main_root())?;
            let report = crate::ReviewLifecycleReport {
                next_action: review_next_action(&lifecycle),
                policy,
                lifecycle,
                changed: false,
                operation_id: None,
                non_blocking_feedback: true,
            };
            render_review_report(&report, parsed.json)?;
        }
        "request" | "unlock" => {
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let lifecycle = broker
                .store()
                .review_lifecycle_for_session(session_id)?
                .ok_or(crate::BrokerError::ReviewLifecycleNotFound(session_id))?;
            let session = broker.store().session(session_id)?;
            if session.status.is_closed() {
                return Err(UsageError::Message(format!(
                    "session {session_id} is closed; `review show` remains available for diagnostics, but review mutations require `aethyme broker review reassign --session {session_id} --to-session <live-id> --reason <text>` or `aethyme broker review abandon --session {session_id} --reason <text>`"
                )));
            }
            let policy = crate::ReviewPolicy::load(broker.main_root())?;
            let repository = lifecycle
                .repository
                .strip_prefix("github.com/")
                .unwrap_or(&lifecycle.repository);
            let snapshot = crate::load_review_provider_snapshot(
                Path::new(&session.worktree_path),
                repository,
                lifecycle.pr_number,
                &policy,
            )?;
            let report = if action == "request" {
                broker.request_review(session_id, &snapshot, now_ms())?
            } else {
                broker.unlock_review_validation(session_id, &snapshot, now_ms())?
            };
            render_review_report(&report, parsed.json)?;
        }
        "reassign" => {
            let to_session_id = parsed.to_session.ok_or_else(|| {
                UsageError::Message(
                    "review reassign requires --session <closed-id> --to-session <live-id> --reason <text>"
                        .into(),
                )
            })?;
            let reason = parsed.reason.as_deref().ok_or_else(|| {
                UsageError::Message(
                    "review reassign requires --session <closed-id> --to-session <live-id> --reason <text>"
                        .into(),
                )
            })?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report =
                broker.reassign_review_lifecycle(session_id, to_session_id, reason, now_ms())?;
            render_review_report(&report, parsed.json)?;
        }
        "abandon" => {
            let reason = parsed.reason.as_deref().ok_or_else(|| {
                UsageError::Message("review abandon requires --session <id> --reason <text>".into())
            })?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report = broker.abandon_review_lifecycle(session_id, reason, now_ms())?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "Review lifecycle {} abandoned; {}",
                    report.lifecycle.id, report.next_action
                );
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown review action {other:?}; expected register, show, request, unlock, reassign, or abandon"
            )));
        }
    }
    Ok(())
}

fn render_review_report(
    report: &crate::ReviewLifecycleReport,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
    } else {
        println!(
            "Review lifecycle {}: {}{}",
            report.lifecycle.id,
            report.lifecycle.state.as_str(),
            if report.changed { " (advanced)" } else { "" }
        );
        println!(
            "  session/queue: {} / {}",
            report.lifecycle.session_id,
            report
                .lifecycle
                .queue_entry_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "not yet verified".into())
        );
        println!(
            "  repository/PR: {} / #{}",
            report.lifecycle.repository, report.lifecycle.pr_number
        );
        println!("  commit: {}", report.lifecycle.commit_sha);
        if let Some(operation_id) = report.operation_id {
            println!("  coordinated operation: {operation_id}");
        }
        println!("  next: {}", report.next_action);
    }
    Ok(())
}

fn review_next_action(lifecycle: &crate::ReviewLifecycle) -> String {
    match lifecycle.state {
        crate::ReviewLifecycleState::DraftOpened => {
            format!("aethyme broker submit --session {}", lifecycle.session_id)
        }
        crate::ReviewLifecycleState::LocalSubmissionVerified
        | crate::ReviewLifecycleState::ReplacementCommitSubmitted => {
            format!(
                "aethyme broker review request --session {}",
                lifecycle.session_id
            )
        }
        crate::ReviewLifecycleState::ReviewRequested => {
            format!(
                "aethyme broker review show --session {}",
                lifecycle.session_id
            )
        }
        crate::ReviewLifecycleState::ChangesRequested => {
            "commit the replacement through the accepted session, then submit it".into()
        }
        crate::ReviewLifecycleState::ReviewSatisfied => {
            format!(
                "aethyme broker review unlock --session {}",
                lifecycle.session_id
            )
        }
        crate::ReviewLifecycleState::ValidationUnlocked => {
            "validation is explicitly unlocked".into()
        }
    }
}

fn run_external_events(parsed: Parsed) -> Result<(), UsageError> {
    const MAX_INPUT_BYTES: u64 = 64 * 1024;
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .ok_or_else(|| {
            UsageError::Message("external-events requires ingest, list, show, or reconcile".into())
        })?;
    match action {
        "ingest" => {
            let path = parsed.positional.get(1).ok_or_else(|| {
                UsageError::Message("external-events ingest requires <normalized.json>".into())
            })?;
            if parsed.positional.len() != 2 {
                return Err(UsageError::Message(
                    "external-events ingest accepts exactly one normalized JSON path".into(),
                ));
            }
            let metadata = std::fs::symlink_metadata(path)?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(UsageError::Message(
                    "external event input must be a regular non-symlink file".into(),
                ));
            }
            if metadata.len() > MAX_INPUT_BYTES {
                return Err(UsageError::Message(format!(
                    "external event input exceeds {MAX_INPUT_BYTES} bytes"
                )));
            }
            let bytes = std::fs::read(path)?;
            let envelope: crate::ExternalEventEnvelope =
                serde_json::from_slice(&bytes).map_err(|error| {
                    UsageError::Message(format!("invalid external event JSON: {error}"))
                })?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report = broker.ingest_external_event(envelope, now_ms())?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "External event {}: {}{}",
                    report.event.id,
                    report.event.status.as_str(),
                    if report.deduplicated {
                        " (idempotent redelivery)"
                    } else {
                        ""
                    }
                );
                if let Some(session_id) = report.event.session_id {
                    println!("  owner session: {session_id}");
                }
                if let Some(remediation) = report.remediation {
                    println!("  reconcile: {remediation}");
                }
                println!("  policy effect: advisory only; no gate or submit state changed");
            }
        }
        "list" => {
            if parsed.positional.len() != 1 {
                return Err(UsageError::Message(
                    "external-events list accepts no positional arguments".into(),
                ));
            }
            let mut broker = open_broker(true)?;
            let events = broker.store().external_events(parsed.all)?;
            if parsed.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version": crate::EXTERNAL_EVENT_SCHEMA_VERSION,
                        "events": events,
                        "includes_terminal": parsed.all,
                        "limit": 500,
                    }))?
                );
            } else if events.is_empty() {
                println!("No matching external coordination events.");
            } else {
                println!("{:<5} {:<24} {:<22} OWNER", "ID", "TYPE", "STATUS");
                for event in events {
                    println!(
                        "{:<5} {:<24} {:<22} {}",
                        event.id,
                        event.event_type,
                        event.status.as_str(),
                        event
                            .session_id
                            .map(|session| format!("session {session}"))
                            .unwrap_or_else(|| "unresolved".into())
                    );
                }
            }
        }
        "show" => {
            let id = external_event_positional_id(&parsed, "show")?;
            let mut broker = open_broker(true)?;
            let event = broker
                .store()
                .external_event(id)?
                .ok_or(crate::BrokerError::ExternalEventNotFound(id))?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&event)?);
            } else {
                println!("External event {}:", event.id);
                println!(
                    "  type/status: {} / {}",
                    event.event_type,
                    event.status.as_str()
                );
                println!("  repository: {}", event.repository);
                println!("  PR/commit: #{} / {}", event.pr_number, event.commit_sha);
                println!(
                    "  owner: {}",
                    event
                        .session_id
                        .map(|session| format!("session {session}"))
                        .unwrap_or_else(|| "unresolved".into())
                );
                println!("  policy effect: advisory only");
            }
        }
        "reconcile" => {
            let id = external_event_positional_id(&parsed, "reconcile")?;
            let outcome = parsed.outcome.as_deref().ok_or_else(|| {
                UsageError::Message(
                    "external-events reconcile requires --outcome <assign|ignore> --reason <text> and --session <id> when assigning"
                        .into(),
                )
            })?;
            let reason = parsed.reason.as_deref().ok_or_else(|| {
                UsageError::Message(
                    "external-events reconcile requires --outcome <assign|ignore> --reason <text> and --session <id> when assigning"
                        .into(),
                )
            })?;
            let resolution = match outcome {
                "assign" => crate::ExternalEventReconciliation::Assign {
                    session_id: parsed.session.ok_or_else(|| {
                        UsageError::Message(
                            "external-events reconcile --outcome assign requires --session <id>"
                                .into(),
                        )
                    })?,
                },
                "ignore" if parsed.session.is_none() => crate::ExternalEventReconciliation::Ignore,
                "ignore" => {
                    return Err(UsageError::Message(
                        "external-events reconcile --outcome ignore does not accept --session"
                            .into(),
                    ));
                }
                _ => {
                    return Err(UsageError::Message(
                        "external-events reconcile --outcome must be assign or ignore".into(),
                    ));
                }
            };
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report = broker.reconcile_external_event(id, resolution, reason, now_ms())?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "External event {} reconciled as {} (reason stored as SHA-256 only).",
                    report.event.id,
                    report.event.status.as_str()
                );
                println!("Policy effect: advisory only; no gate or submit state changed.");
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown external-events action {other:?}; expected ingest, list, show, or reconcile"
            )));
        }
    }
    Ok(())
}

fn external_event_positional_id(parsed: &Parsed, action: &str) -> Result<i64, UsageError> {
    if parsed.positional.len() != 2 {
        return Err(UsageError::Message(format!(
            "external-events {action} requires exactly one event id"
        )));
    }
    parsed.positional[1]
        .parse()
        .map_err(|_| UsageError::Message("external event id must be an integer".into()))
}

fn run_report(parsed: Parsed) -> Result<(), UsageError> {
    match parsed.positional.first().map(String::as_str) {
        Some("capture") if parsed.positional.len() == 1 => {
            if parsed.stdout && parsed.output.is_some() {
                return Err(UsageError::Message(
                    "--stdout and --output are mutually exclusive".into(),
                ));
            }
            if parsed.stdout && parsed.json {
                return Err(UsageError::Message(
                    "--stdout already emits the JSON report; do not combine it with --json".into(),
                ));
            }
            let kind = crate::ReportKind::parse(
                parsed
                    .kind
                    .as_deref()
                    .ok_or(UsageError::Message("report capture requires --kind".into()))?,
            )?;
            let title = parsed.title.as_deref().ok_or(UsageError::Message(
                "report capture requires --title".into(),
            ))?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let selected_session = if let Some(session_id) = parsed.session {
                Some(session_id)
            } else {
                let cwd = std::env::current_dir()
                    .map_err(|error| UsageError::Message(error.to_string()))?;
                let checkout = crate::GitRepo::discover(&cwd)?;
                let worktree = checkout.root().to_string_lossy();
                broker
                    .store()
                    .session_for_worktree(&worktree)?
                    .map(|session| session.id)
            };
            let prepared = crate::prepare_report(
                &mut broker,
                kind,
                title,
                selected_session,
                parsed.include_task,
                now_ms(),
            )?;
            if parsed.stdout {
                use std::io::Write;
                std::io::stdout()
                    .lock()
                    .write_all(&prepared.bytes)
                    .map_err(|error| UsageError::Message(error.to_string()))?;
                eprintln!("SHA-256: {}", prepared.sha256);
            } else {
                let result = crate::write_report_atomic(
                    broker.main_root(),
                    parsed.output.as_deref(),
                    &prepared,
                )?;
                if parsed.json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!(
                        "Captured {} report: {}",
                        kind.as_str(),
                        result.path.as_deref().unwrap_or("-")
                    );
                    println!("SHA-256: {}", result.sha256);
                    println!("Review this local report before any later filing step.");
                }
            }
            Ok(())
        }
        Some("list") if parsed.positional.len() == 1 => {
            let main_root = report_main_root()?;
            let report = crate::list_reports(&main_root)?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else if report.reports.is_empty() && report.invalid.is_empty() {
                println!("No captured reports.");
            } else {
                if report.reports.is_empty() {
                    println!("No valid captured reports.");
                } else {
                    println!(
                        "CAPTURED_AT    KIND          STATE     VERSION          DIGEST       PATH"
                    );
                    for item in &report.reports {
                        println!(
                            "{:<14} {:<13} {:<9} {:<16} {:<12} {}",
                            item.captured_at,
                            item.kind.as_str(),
                            match item.filing_state {
                                crate::ReportFilingState::Unfiled => "unfiled",
                                crate::ReportFilingState::Filed => "filed",
                            },
                            item.version,
                            &item.digest[..12],
                            item.path,
                        );
                    }
                }
                for invalid in &report.invalid {
                    eprintln!("Invalid report {}: {}", invalid.path, invalid.error);
                }
            }
            Ok(())
        }
        Some("show") if parsed.positional.len() == 2 => {
            let main_root = report_main_root()?;
            let inspection =
                crate::show_report(&main_root, PathBuf::from(&parsed.positional[1]).as_path())?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&inspection)?);
            } else {
                println!("Report: {}", inspection.summary.path);
                println!("  title: {}", inspection.summary.title);
                println!("  captured at: {}", inspection.summary.captured_at);
                println!("  kind: {}", inspection.summary.kind.as_str());
                println!("  version: {}", inspection.summary.version);
                println!("  digest: {}", inspection.summary.digest);
                println!(
                    "  filing state: {}",
                    match inspection.summary.filing_state {
                        crate::ReportFilingState::Unfiled => "unfiled",
                        crate::ReportFilingState::Filed => "filed",
                    }
                );
                println!("\n{}", serde_json::to_string_pretty(&inspection.report)?);
            }
            Ok(())
        }
        Some("render") if parsed.positional.len() == 2 => {
            let main_root = report_main_root()?;
            let form = parsed.form.as_deref().ok_or(UsageError::Message(
                "report render requires --form <form.yml>".into(),
            ))?;
            let rendered = crate::render_issue_form(
                &main_root,
                PathBuf::from(&parsed.positional[1]).as_path(),
                form,
            )?;
            if let Some(output) = parsed.output.as_deref() {
                let written = crate::write_issue_form_render_atomic(&main_root, output, &rendered)?;
                if parsed.json {
                    println!("{}", serde_json::to_string_pretty(&written)?);
                } else {
                    println!("Rendered reviewed report: {}", written.path);
                    println!("SHA-256: {}", written.sha256);
                    if !written.valid {
                        println!(
                            "Edit the required unfilled sections before filing: {}",
                            written.missing_required.join(", ")
                        );
                    }
                }
            } else if parsed.json {
                println!("{}", serde_json::to_string_pretty(&rendered)?);
            } else {
                print!("{}", rendered.markdown);
                eprintln!("Issue title: {}", rendered.issue_title);
                eprintln!("Report SHA-256: {}", rendered.report_digest);
            }
            if rendered.valid {
                Ok(())
            } else {
                Err(UsageError::Exit {
                    message: format!(
                        "required issue-form fields remain unfilled: {}",
                        rendered.missing_required.join(", ")
                    ),
                    code: 1,
                })
            }
        }
        Some("file") if parsed.positional.len() == 2 => {
            let repository = parsed.repository.as_deref().ok_or(UsageError::Message(
                "report file requires --repo <owner/name>".into(),
            ))?;
            let confirmation = parsed.confirm.as_deref().ok_or(UsageError::Message(
                "report file requires --confirm <sha256>".into(),
            ))?;
            let cwd = std::env::current_dir()
                .map_err(|error| UsageError::Message(error.to_string()))?;
            let checkout = crate::GitRepo::discover(&cwd)?;
            let worktree = checkout.root().to_string_lossy();
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let session = broker
                .store()
                .session_for_worktree(&worktree)?
                .ok_or(UsageError::Message(
                    "report file requires a broker session for the current worktree; run `aethyme broker adopt --task \"File reviewed report\"` first".into(),
                ))?;
            let filed = crate::file_reviewed_report(
                &mut broker,
                session.id,
                PathBuf::from(&parsed.positional[1]).as_path(),
                repository,
                confirmation,
            )?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&filed)?);
            } else {
                match filed.state {
                    crate::ReportFileState::Filed => {
                        println!(
                            "Filed {} as {}#{}",
                            filed.path,
                            filed.repository,
                            filed.issue_number.unwrap_or_default()
                        );
                        if let Some(url) = filed.issue_url.as_deref() {
                            println!("Issue: {url}");
                        }
                        println!("Operation: {}", filed.operation_id);
                    }
                    crate::ReportFileState::ReconciliationRequired => {
                        println!(
                            "Report filing outcome is unknown (operation {}).",
                            filed.operation_id
                        );
                    }
                }
            }
            if filed.state == crate::ReportFileState::ReconciliationRequired {
                let operation = broker
                    .store()
                    .coordinated_operation(filed.operation_id)?
                    .ok_or_else(|| {
                        UsageError::Message(format!(
                            "coordinated operation {} disappeared before recovery guidance could be rendered",
                            filed.operation_id
                        ))
                    })?;
                return Err(UsageError::Exit {
                    message: crate::UnknownOutcomeRecovery::from_operation(&operation).to_string(),
                    code: 1,
                });
            }
            Ok(())
        }
        Some("capture" | "list" | "show" | "render" | "file") => Err(UsageError::Message(
            "invalid report arguments; expected capture, list, show <filename>, render <filename> --form <form.yml> [--output <name>.issue.md], or file <path> --repo <owner/name> --confirm <sha256>".into(),
        )),
        Some(other) => Err(UsageError::Message(format!(
            "unknown report action {other:?}; expected capture, list, show, render, or file"
        ))),
        None => Err(UsageError::Message(
            "report requires an action: capture, list, show, render, or file".into(),
        )),
    }
}

fn report_main_root() -> Result<PathBuf, UsageError> {
    let cwd = std::env::current_dir().map_err(|error| UsageError::Message(error.to_string()))?;
    let repo = crate::GitRepo::discover(&cwd)?;
    Ok(repo.main_root()?)
}

const HOST_RESOURCE_INPUT_MAX_BYTES: u64 = 1024 * 1024;

fn read_resource_json<T: serde::de::DeserializeOwned>(
    path: &std::path::Path,
) -> Result<T, UsageError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        UsageError::Message(format!("cannot inspect {}: {error}", path.display()))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(UsageError::Message(format!(
            "resource input must be a regular, non-symlink file: {}",
            path.display()
        )));
    }
    if metadata.len() > HOST_RESOURCE_INPUT_MAX_BYTES {
        return Err(UsageError::Message(format!(
            "resource input exceeds {HOST_RESOURCE_INPUT_MAX_BYTES} bytes"
        )));
    }
    let bytes = std::fs::read(path)
        .map_err(|error| UsageError::Message(format!("cannot read {}: {error}", path.display())))?;
    serde_json::from_slice(&bytes).map_err(|error| {
        UsageError::Message(format!("invalid JSON in {}: {error}", path.display()))
    })
}

fn parse_resource_duration(value: &str) -> Result<std::time::Duration, UsageError> {
    let (digits, multiplier) = if let Some(value) = value.strip_suffix("ms") {
        (value, 1_u64)
    } else if let Some(value) = value.strip_suffix('s') {
        (value, 1_000)
    } else if let Some(value) = value.strip_suffix('m') {
        (value, 60_000)
    } else if let Some(value) = value.strip_suffix('h') {
        (value, 3_600_000)
    } else {
        return Err(UsageError::Message(
            "duration must use ms, s, m, or h (for example 30m)".into(),
        ));
    };
    let amount = digits.parse::<u64>().map_err(|_| {
        UsageError::Message("duration must be a non-negative integer plus ms, s, m, or h".into())
    })?;
    let millis = amount
        .checked_mul(multiplier)
        .ok_or_else(|| UsageError::Message("duration is too large".into()))?;
    Ok(std::time::Duration::from_millis(millis))
}

fn write_private_grant(
    path: &std::path::Path,
    grant: &crate::HostResourceGrant,
) -> Result<(), UsageError> {
    use std::io::Write as _;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    if path.exists() {
        return Err(UsageError::Message(format!(
            "refusing to overwrite existing grant file {}",
            path.display()
        )));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    if !parent.is_dir() {
        return Err(UsageError::Message(format!(
            "grant parent directory does not exist: {}",
            parent.display()
        )));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        UsageError::Message(format!(
            "cannot create private grant beside {}: {error}",
            path.display()
        ))
    })?;
    #[cfg(unix)]
    temporary
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| UsageError::Message(format!("cannot protect grant file: {error}")))?;
    serde_json::to_writer_pretty(&mut temporary, grant)?;
    temporary
        .write_all(b"\n")
        .map_err(|error| UsageError::Message(format!("cannot finish private grant: {error}")))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| UsageError::Message(format!("cannot sync private grant: {error}")))?;
    temporary.persist_noclobber(path).map_err(|error| {
        UsageError::Message(format!(
            "cannot publish private grant {}: {}",
            path.display(),
            error.error
        ))
    })?;
    Ok(())
}

#[derive(serde::Serialize)]
struct ResourceAcquireFailure<'a> {
    code: &'a str,
    request_id: &'a str,
    retryable: bool,
    waited_ms: u128,
    conflicts: &'a [crate::HostResourceConflict],
}

fn render_host_lease(lease: &crate::HostResourceLease) {
    println!(
        "{} generation {} — {} until {}",
        lease.lease_id,
        lease.generation,
        lease.state.as_str(),
        lease.expires_at
    );
    for allocation in &lease.allocations {
        println!(
            "  {:<20} {:<14} {}",
            allocation.key, allocation.kind, allocation.value
        );
    }
}

fn render_submission_plan(plan: &crate::SubmissionPlan, checkout: &crate::GitRepo) {
    println!(
        "Submitting session {} — HEAD {} onto integration {}",
        plan.session_id,
        short_sha(&plan.session_head),
        short_sha(&plan.integration_head)
    );
    println!(
        "  recorded baseline: {}",
        plan.recorded_baseline
            .as_deref()
            .map(short_sha)
            .unwrap_or("missing")
    );

    render_submission_group(
        "session-owned commits",
        plan.commits
            .iter()
            .filter(|commit| commit.ownership == crate::SubmissionCommitOwnership::SessionOwned),
        checkout,
    );
    render_submission_group(
        "inherited baseline history (not replayed)",
        plan.commits.iter().filter(|commit| {
            commit.ownership == crate::SubmissionCommitOwnership::InheritedFromRecordedBaseline
        }),
        checkout,
    );
    render_submission_group(
        "ambiguous commits (submission refused)",
        plan.commits.iter().filter(|commit| {
            commit.ownership == crate::SubmissionCommitOwnership::Ambiguous
                || commit.integration_state == crate::SubmissionIntegrationState::Ambiguous
        }),
        checkout,
    );

    println!(
        "  merged-tree delta: {} file(s)",
        plan.merged_tree_paths.len()
    );
    for path in plan.merged_tree_paths.iter().take(10) {
        println!("    {path}");
    }
    if plan.merged_tree_paths.len() > 10 {
        println!("    ... and {} more", plan.merged_tree_paths.len() - 10);
    }
    for warning in &plan.warnings {
        println!("  warning: {warning}");
    }
}

fn render_submission_group<'a>(
    label: &str,
    commits: impl Iterator<Item = &'a crate::SubmissionCommitProvenance>,
    checkout: &crate::GitRepo,
) {
    let commits = commits.collect::<Vec<_>>();
    println!("  {label}: {}", commits.len());
    for commit in commits.iter().take(10) {
        let subject = checkout
            .commit_message(&commit.commit)
            .ok()
            .and_then(|message| message.lines().next().map(str::to_string))
            .unwrap_or_else(|| "<subject unavailable>".into());
        let state = match commit.integration_state {
            crate::SubmissionIntegrationState::Pending => "pending replay",
            crate::SubmissionIntegrationState::AlreadyIntegratedByAncestry => {
                "already integrated by ancestry"
            }
            crate::SubmissionIntegrationState::AlreadyIntegratedByStablePatchIdentity => {
                "already integrated by patch identity"
            }
            crate::SubmissionIntegrationState::Ambiguous => "ambiguous integration identity",
        };
        println!("    {} {subject} [{state}]", short_sha(&commit.commit));
    }
    if commits.len() > 10 {
        println!("    ... and {} more", commits.len() - 10);
    }
}

fn short_sha(value: &str) -> &str {
    &value[..12.min(value.len())]
}

fn run_resources(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .ok_or_else(|| {
            UsageError::Message(
                "resources requires plan, acquire, run, renew, release, list, or reconcile".into(),
            )
        })?;
    match action {
        "plan" | "acquire" => {
            let path = parsed.positional.get(1).map(PathBuf::from).ok_or_else(|| {
                UsageError::Message(format!("resources {action} requires <request.json>"))
            })?;
            let request: crate::HostResourceRequest = read_resource_json(&path)?;
            if action == "plan" {
                let coordinator = crate::HostResourceCoordinator::open_read_only_default()?;
                let plan = coordinator.plan(&request)?;
                if parsed.json {
                    println!("{}", serde_json::to_string_pretty(&plan)?);
                } else {
                    println!(
                        "Request {} — {} (advisory; acquire is authoritative)",
                        plan.request_id,
                        if plan.available {
                            "available"
                        } else {
                            "blocked"
                        }
                    );
                    for allocation in &plan.proposed {
                        println!(
                            "  proposed {:<20} {:<14} {}",
                            allocation.key, allocation.kind, allocation.value
                        );
                    }
                    for conflict in &plan.conflicts {
                        println!(
                            "  conflict {:<20} {}",
                            conflict.resource_key, conflict.reason
                        );
                    }
                }
            } else {
                let mut coordinator = crate::HostResourceCoordinator::open_default()?;
                let wait = parsed
                    .wait
                    .as_deref()
                    .map(parse_resource_duration)
                    .transpose()?
                    .unwrap_or_default();
                let started = std::time::Instant::now();
                let acquired = if wait.is_zero() {
                    coordinator.acquire(&request)
                } else {
                    coordinator.acquire_with_wait(&request, wait, |_| {})
                };
                let grant = match acquired {
                    Ok(grant) => grant,
                    Err(crate::HostResourceError::Conflict {
                        code, conflicts, ..
                    }) if parsed.json => {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&ResourceAcquireFailure {
                                retryable: code == "resource_contention",
                                code: &code,
                                request_id: &request.request_id,
                                waited_ms: started.elapsed().as_millis(),
                                conflicts: &conflicts,
                            })?
                        );
                        return Err(UsageError::SilentExit(75));
                    }
                    Err(error) => return Err(error.into()),
                };
                if let Some(path) = parsed.grant_out.as_deref() {
                    write_private_grant(path, &grant)?;
                }
                if parsed.json && parsed.grant_out.is_some() {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "lease": grant.lease,
                            "grant_path": parsed.grant_out,
                        }))?
                    );
                } else if parsed.json {
                    println!("{}", serde_json::to_string_pretty(&grant)?);
                } else {
                    render_host_lease(&grant.lease);
                    if let Some(path) = parsed.grant_out {
                        println!("Private grant: {}", path.display());
                    } else {
                        println!("Ownership token: {}", grant.ownership_token);
                        println!("Store the complete JSON grant privately for renew/release.");
                    }
                }
            }
        }
        "renew" | "release" => {
            let mut coordinator = crate::HostResourceCoordinator::open_default()?;
            let path = parsed.positional.get(1).map(PathBuf::from).ok_or_else(|| {
                UsageError::Message(format!("resources {action} requires <grant.json>"))
            })?;
            let mut grant: crate::HostResourceGrant = read_resource_json(&path)?;
            grant.lease = if action == "renew" {
                let ttl = parsed.ttl_seconds.ok_or_else(|| {
                    UsageError::Message("resources renew requires --ttl <seconds>".into())
                })?;
                let ttl = u64::try_from(ttl)
                    .map_err(|_| UsageError::Message("--ttl must be positive".into()))?;
                coordinator.renew(
                    &grant.lease.lease_id,
                    grant.lease.generation,
                    &grant.ownership_token,
                    ttl,
                )?
            } else {
                coordinator.release(
                    &grant.lease.lease_id,
                    grant.lease.generation,
                    &grant.ownership_token,
                )?
            };
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&grant)?);
            } else {
                render_host_lease(&grant.lease);
            }
        }
        "run" => {
            let path = parsed.positional.get(1).map(PathBuf::from).ok_or_else(|| {
                UsageError::Message("resources run requires <request.json>".into())
            })?;
            if parsed.exec_command.is_empty() {
                return Err(UsageError::Message(
                    "resources run requires -- <command> [args...]".into(),
                ));
            }
            let request: crate::HostResourceRequest = read_resource_json(&path)?;
            let wait = parsed
                .wait
                .as_deref()
                .map(parse_resource_duration)
                .transpose()?
                .unwrap_or_default();
            let cwd =
                std::env::current_dir().map_err(|error| UsageError::Message(error.to_string()))?;
            let mut coordinator = crate::HostResourceCoordinator::open_default()?;
            let json = parsed.json;
            let report = coordinator.run_supervised(
                &request,
                wait,
                &parsed.exec_command,
                parsed.cleanup_command.as_deref(),
                &cwd,
                |message| {
                    if json {
                        eprintln!(
                            "{}",
                            serde_json::json!({
                                "type": "resource_run_event",
                                "request_id": request.request_id,
                                "message": message,
                            })
                        );
                    } else {
                        eprintln!("resource run: {message}");
                    }
                },
            );
            let report = match report {
                Ok(report) => report,
                Err(crate::HostResourceRunError::Resource(
                    crate::HostResourceError::Conflict {
                        code, conflicts, ..
                    },
                )) if json => {
                    eprintln!(
                        "{}",
                        serde_json::to_string(&ResourceAcquireFailure {
                            retryable: code == "resource_contention",
                            code: &code,
                            request_id: &request.request_id,
                            waited_ms: wait.as_millis(),
                            conflicts: &conflicts,
                        })?
                    );
                    return Err(UsageError::SilentExit(75));
                }
                Err(error) => return Err(error.into()),
            };
            if json {
                eprintln!("{}", serde_json::to_string(&report)?);
            } else {
                eprintln!(
                    "resource run: child={} cleanup={} final={}",
                    report.child_exit_code,
                    report
                        .cleanup_exit_code
                        .map_or_else(|| "not-requested".into(), |code| code.to_string()),
                    report.final_lease_state.as_str()
                );
            }
            let lifecycle_failed = report.authority_lost
                || report.cleanup_exit_code.is_some_and(|code| code != 0)
                || report.final_lease_state != crate::HostLeaseState::Released;
            let exit = if report.child_exit_code != 0 {
                report.child_exit_code
            } else if lifecycle_failed {
                70
            } else {
                0
            };
            if exit != 0 {
                return Err(UsageError::SilentExit(exit));
            }
        }
        "list" => {
            let coordinator = crate::HostResourceCoordinator::open_read_only_default()?;
            let leases = coordinator.list(parsed.all)?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&leases)?);
            } else if leases.is_empty() {
                println!("No active or quarantined host resource leases.");
            } else {
                for lease in &leases {
                    render_host_lease(lease);
                }
            }
        }
        "reconcile" => {
            let mut coordinator = crate::HostResourceCoordinator::open_default()?;
            let lease_id = parsed
                .positional
                .get(1)
                .ok_or_else(|| UsageError::Message(RESOURCES_RECONCILE_USAGE.into()))?;
            let generation = parsed
                .confirm
                .as_deref()
                .ok_or_else(|| UsageError::Message(RESOURCES_RECONCILE_USAGE.into()))?
                .parse::<u64>()
                .map_err(|_| {
                    UsageError::Message(format!(
                        "--confirm must be the full numeric generation; {RESOURCES_RECONCILE_USAGE}"
                    ))
                })?;
            let lease = coordinator.reconcile(lease_id, generation)?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&lease)?);
            } else {
                render_host_lease(&lease);
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown resources action {other:?}; expected plan, acquire, run, renew, release, list, or reconcile"
            )));
        }
    }
    Ok(())
}

fn run_inner(args: &[String], mode: CompatibilityMode) -> Result<(), UsageError> {
    let Some(subcommand) = args.first() else {
        return Err(UsageError::Help);
    };
    let mut parsed = parse(&args[1..]).map_err(|error| {
        if subcommand == "operations" && args.get(1).map(String::as_str) == Some("reconcile") {
            match error {
                UsageError::Message(message) if !message.contains(OPERATIONS_RECONCILE_USAGE) => {
                    operations_reconcile_error(message)
                }
                other => other,
            }
        } else {
            error
        }
    })?;
    parsed.read_only_snapshot = mode == CompatibilityMode::ReadOnlySnapshot;
    if !parsed.planned_paths.is_empty() && !matches!(subcommand.as_str(), "start" | "adopt") {
        return Err(UsageError::Message(
            "--path is valid only with broker start or broker adopt".into(),
        ));
    }
    surface_command_advisories(subcommand, &parsed);

    match subcommand.as_str() {
        "worktree-root" => {
            if !parsed.positional.is_empty() {
                return Err(UsageError::Message(
                    "worktree-root does not accept positional arguments".into(),
                ));
            }
            let broker = open_broker(parsed.read_only_snapshot)?;
            let plan = broker.worktree_root_plan()?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                println!("Repository: {}", plan.repository_root.display());
                println!("Repository key: {}", plan.repository_key);
                if let (Some(root), Some(source)) = (&plan.preferred_root, plan.preferred_source) {
                    println!(
                        "Preferred worktree root: {} ({})",
                        root.display(),
                        source.as_str()
                    );
                    println!(
                        "Scanner boundary: {}",
                        if plan.preferred_outside_repository {
                            "outside the repository"
                        } else {
                            "invalid: preferred root resolves inside the repository"
                        }
                    );
                } else {
                    println!("Preferred worktree root: unavailable");
                }
                println!(
                    "Legacy fallback: {} (used only when host state is unavailable)",
                    plan.legacy_fallback_root.display()
                );
            }
        }
        "adopt" => {
            let mut broker = open_broker(parsed.read_only_snapshot)?;
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
            if parsed.sync_integration && mode != crate::AdoptMode::Reuse {
                return Err(UsageError::Message(
                    "--sync-integration requires --reuse".into(),
                ));
            }
            let report = broker.adopt_with_options(
                &path,
                parsed.task.as_deref(),
                crate::AdoptOptions {
                    mode,
                    sync_integration: parsed.sync_integration,
                    planned_paths: parsed.planned_paths,
                },
            )?;
            let session = &report.session;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                match report.outcome {
                    crate::AdoptOutcome::Created => println!(
                        "Created session {} on the existing worktree — {} on branch {}",
                        session.id, session.worktree_path, session.branch
                    ),
                    crate::AdoptOutcome::Reused => println!(
                        "Reusing session {} — worktree {} on branch {}",
                        session.id, session.worktree_path, session.branch
                    ),
                    crate::AdoptOutcome::Replaced => println!(
                        "Replaced the prior session with session {} on the existing worktree — {} on branch {}",
                        session.id, session.worktree_path, session.branch
                    ),
                }
                if std::path::Path::new(&session.worktree_path) == broker.main_root() {
                    println!(
                        "note: main-checkout session — verification is advisory here \
                         (commits land on main before gates run); use a worktree \
                         session for enforced verification."
                    );
                }
                if let Some(sync) = &report.integration_sync {
                    let summary = match sync.outcome {
                        crate::AdoptIntegrationSyncOutcome::AlreadyCurrent => "already current",
                        crate::AdoptIntegrationSyncOutcome::FastForwarded => "fast-forwarded",
                    };
                    println!(
                        "Integration synchronization: {summary} ({} -> {}, {} at {})",
                        short_commit(&sync.before_head),
                        short_commit(&sync.after_head),
                        sync.integration_branch,
                        short_commit(&sync.integration_head),
                    );
                }
                if let Some(drift) = &report.integration_drift {
                    println!(
                        "Integration drift: {} (session HEAD {}, {} HEAD {}; {} ahead, {} behind)",
                        drift.relation.as_str(),
                        short_commit(&drift.session_head),
                        drift.integration_branch,
                        short_commit(&drift.integration_head),
                        drift.ahead_commits,
                        drift.behind_commits,
                    );
                    if !drift.overlapping_changed_paths.is_empty() {
                        println!("Overlapping changed paths:");
                        for path in &drift.overlapping_changed_paths {
                            println!("  {path}");
                        }
                    }
                    if let Some(warning) = &drift.warning {
                        println!("Warning: {warning}");
                    }
                    println!("Safe next action: {}", drift.safe_next_action);
                }
                render_planned_explicit_leases(&report.planned_explicit_leases);
            }
        }
        "start" => {
            let task = parsed
                .task
                .ok_or(UsageError::Message("start requires --task".into()))?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report = broker.start_worktree_with_planned_paths(&task, &parsed.planned_paths)?;
            let session = &report.session;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "Started session {} — worktree {} on branch {}",
                    session.id, session.worktree_path, session.branch
                );
                println!(
                    "Start base: {} at {} ({})",
                    report.start_base.ref_name,
                    short_commit(&report.start_base.commit),
                    report.start_base.evidence.as_str()
                );
                render_worktree_placement(&report.worktree_placement);
                render_planned_explicit_leases(&report.planned_explicit_leases);
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
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report = broker.start_agent_report(&task, &cmd)?;
            let session = &report.session;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "Started session {} (pid {}) — worktree {} on branch {}\nLog: {}",
                    session.id,
                    session.pid.unwrap_or(-1),
                    session.worktree_path,
                    session.branch,
                    session.log_path.as_deref().unwrap_or("-"),
                );
                render_worktree_placement(&report.worktree_placement);
            }
        }
        "report" => run_report(parsed)?,
        "external-events" => run_external_events(parsed)?,
        "deliveries" => run_deliveries(parsed)?,
        "review" => run_review(parsed)?,
        "resources" => run_resources(parsed)?,
        "agents" => {
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let (overlaps, views) = if parsed.read_only_snapshot {
                (
                    broker.lease_overlaps_snapshot()?,
                    broker.agents_snapshot(now_ms())?,
                )
            } else {
                (broker.refresh_leases()?, broker.agents(now_ms())?)
            };
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
            let export = parsed.positional.first().map(String::as_str) == Some("export");
            let mut broker = open_broker(parsed.read_only_snapshot || export)?;
            match parsed.positional.first().map(String::as_str) {
                None => {
                    let overlaps = if parsed.read_only_snapshot {
                        broker.lease_overlaps_snapshot()?
                    } else {
                        broker.refresh_leases()?
                    };
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
                Some("plan") => {
                    let paths = parsed.positional.get(1..).unwrap_or_default();
                    if paths.is_empty() {
                        return Err(UsageError::Message(
                            "plan requires at least one path".into(),
                        ));
                    }
                    let report = broker.plan_leases(paths, parsed.session)?;
                    render_lease_plan(&report, parsed.json)?;
                }
                Some("export") => {
                    let limit = parsed
                        .limit
                        .map(|value| value as usize)
                        .unwrap_or(crate::DEFAULT_LEASE_ROUTING_EXPORT_LIMIT);
                    let report = broker.export_lease_routing(
                        crate::LeaseRoutingExportOptions {
                            session_id: parsed.session,
                            queue_entry_id: parsed.entry,
                            limit,
                        },
                        now_ms(),
                    )?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        println!(
                            "Lease routing for {} (session {}, {} of {} rows):",
                            report.repository.display_slug,
                            report.selector.session_id,
                            report.leases.len(),
                            report.total_matching
                        );
                        for lease in &report.leases {
                            let routes = if lease.routing_categories.is_empty() {
                                "unrouted".into()
                            } else {
                                lease.routing_categories.join(",")
                            };
                            println!(
                                "  {} [{} / {} / {}] routes={}{}",
                                lease.path,
                                lease.path_kind.as_str(),
                                lease.lease_kind.as_str(),
                                lease.state.as_str(),
                                routes,
                                if lease.conflicting_session_ids.is_empty() {
                                    String::new()
                                } else {
                                    format!(
                                        " conflicts=s{}",
                                        lease
                                            .conflicting_session_ids
                                            .iter()
                                            .map(i64::to_string)
                                            .collect::<Vec<_>>()
                                            .join(",s")
                                    )
                                }
                            );
                        }
                        if report.truncated {
                            println!(
                                "  truncated: increase --limit up to {}",
                                crate::MAX_LEASE_ROUTING_EXPORT_LIMIT
                            );
                        }
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
                        "unknown leases action {other:?} — expected claim, plan, export, or release"
                    )));
                }
            }
        }
        "exec" => {
            let session = parsed
                .session
                .ok_or(UsageError::Message("exec requires --session <id>".into()))?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
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
                if !report.newly_dirty_paths.is_empty() {
                    println!(
                        "  newly dirty: {}",
                        capped_join(&report.newly_dirty_paths, 8)
                    );
                }
                if !report.modified_preexisting_dirty_paths.is_empty() {
                    println!(
                        "  changed while already dirty: {}",
                        capped_join(&report.modified_preexisting_dirty_paths, 8)
                    );
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
                resolved_target: None,
                scope: parsed.scope,
                declared_effect: parse_operation_effect(parsed.effect.as_deref())?,
                destructive_confirmed: parsed.destructive,
                authorization_reason: parsed.reason,
                args: parsed.exec_command,
            };
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report = broker.run_coordinated_operation(request)?;
            render_coordinated_operation(&report, parsed.json)?;
            if !report.ok() {
                if let Some(recovery) = report.unknown_outcome_recovery() {
                    return Err(UsageError::Exit {
                        message: recovery.to_string(),
                        code: 1,
                    });
                }
                return Err(UsageError::Message(format!(
                    "coordinated {subcommand} operation {} failed",
                    report.operation.id
                )));
            }
        }
        "advisories" => {
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            match parsed.positional.first().map(String::as_str) {
                Some("list") => {
                    if parsed.positional.len() != 1 {
                        return Err(UsageError::Message(
                            "usage: aethyme broker advisories list [--all] [--json]".into(),
                        ));
                    }
                    let report = broker.advisory_list(parsed.all)?;
                    if !parsed.read_only_snapshot {
                        broker.record_advisories_shown(
                            &report.advisories,
                            crate::AdvisoryDeliverySurface::Inventory,
                        )?;
                    }
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else if report.advisories.is_empty() {
                        println!("No outstanding advisories.");
                    } else {
                        println!("{:<5} {:<9} {:<14} IDENTITY", "ID", "SEVERITY", "STATE");
                        for advisory in &report.advisories {
                            println!(
                                "{:<5} {:<9} {:<14} {}",
                                advisory.id,
                                advisory.severity.as_str(),
                                advisory.resolution_state.as_str(),
                                advisory_text(&advisory.identity),
                            );
                        }
                        println!("Outstanding: {}", report.outstanding_count);
                    }
                }
                Some("show") => {
                    if parsed.positional.len() != 2 {
                        return Err(UsageError::Message(ADVISORIES_SHOW_USAGE.into()));
                    }
                    let id = parse_advisory_id(parsed.positional.get(1), ADVISORIES_SHOW_USAGE)?;
                    let advisory = broker.advisory(id)?;
                    if !parsed.read_only_snapshot {
                        broker.record_advisories_shown(
                            std::slice::from_ref(&advisory),
                            crate::AdvisoryDeliverySurface::Inventory,
                        )?;
                    }
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&advisory)?);
                    } else {
                        render_advisory(&advisory);
                    }
                }
                Some("ack") => {
                    if parsed.positional.len() != 2 {
                        return Err(UsageError::Message(ADVISORIES_ACK_USAGE.into()));
                    }
                    let id = parse_advisory_id(parsed.positional.get(1), ADVISORIES_ACK_USAGE)?;
                    let advisory = broker.acknowledge_advisory(id)?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&advisory)?);
                    } else {
                        println!(
                            "Acknowledged advisory {}: {}",
                            advisory.id,
                            advisory_text(&advisory.identity)
                        );
                        println!("Projection refreshed: {}", crate::BROKER_ADVISORY_RELPATH);
                    }
                }
                Some("metrics") => {
                    if parsed.positional.len() != 1 {
                        return Err(UsageError::Message(
                            "usage: aethyme broker advisories metrics [--json]".into(),
                        ));
                    }
                    let summary = broker.advisory_delivery_summary()?;
                    let metrics = broker.advisory_delivery_metrics()?;
                    if parsed.json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "schema_version": 1,
                                "summary": summary,
                                "metrics": metrics,
                            }))?
                        );
                    } else {
                        println!(
                            "Advisory delivery: {} shown, {} actioned, {} displays across {} surfaces.",
                            summary.shown_advisories,
                            summary.actioned_advisories,
                            summary.total_shows,
                            summary.surface_rows,
                        );
                        for metric in metrics {
                            println!(
                                "  advisory {} / {}: {} display{}{}",
                                metric.advisory_id,
                                metric.surface.as_str(),
                                metric.show_count,
                                if metric.show_count == 1 { "" } else { "s" },
                                metric
                                    .action
                                    .map(|action| format!("; {}", action.as_str()))
                                    .unwrap_or_default(),
                            );
                        }
                    }
                }
                Some(other) => {
                    return Err(UsageError::Message(format!(
                        "unknown advisories action {other:?} — expected list, show, ack, or metrics"
                    )));
                }
                None => {
                    return Err(UsageError::Message(
                        "advisories requires an action: list, show, ack, or metrics".into(),
                    ));
                }
            }
        }
        "exposures" => {
            let action = parsed
                .positional
                .first()
                .map(String::as_str)
                .ok_or_else(|| {
                    UsageError::Message("exposures requires an action: plan or apply".into())
                })?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            match action {
                "plan" => {
                    let plan = broker.exposure_reconciliation_plan()?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&plan)?);
                    } else {
                        println!(
                            "Remote: {} @ {}",
                            plan.remote_default_branch_ref,
                            short_commit(&plan.remote_default_branch_sha)
                        );
                        println!(
                            "Tracking: {} @ {} ({})",
                            plan.tracking_ref,
                            plan.tracking_sha
                                .as_deref()
                                .map(short_commit)
                                .unwrap_or("missing"),
                            if plan.tracking_matches_remote {
                                "current"
                            } else {
                                "stale or missing"
                            }
                        );
                        println!(
                            "Exposures: {} contained, {} remaining",
                            plan.contained_exposures.len(),
                            plan.remaining_exposures.len()
                        );
                        let eligible = plan
                            .advisories
                            .iter()
                            .filter(|advisory| advisory.eligible)
                            .count();
                        println!(
                            "Advisories: {} eligible, {} blocked by live leases",
                            eligible,
                            plan.advisories.len().saturating_sub(eligible)
                        );
                        for refusal in &plan.refusals {
                            println!("Refusal: {refusal}");
                        }
                        println!("Plan digest: {}", plan.digest);
                        if plan.safe {
                            println!(
                                "Apply with: aethyme broker exposures apply --session <id> --confirm {}",
                                plan.digest
                            );
                        }
                    }
                }
                "apply" => {
                    let session = parsed.session.ok_or_else(|| {
                        UsageError::Message("exposures apply requires --session <id>".into())
                    })?;
                    let confirm = parsed.confirm.as_deref().ok_or_else(|| {
                        UsageError::Message("exposures apply requires --confirm <sha256>".into())
                    })?;
                    let report = broker.apply_exposure_reconciliation(session, confirm)?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        println!(
                            "Verified {} at {} via operation {}.",
                            report.plan.remote_default_branch_ref,
                            report.plan.remote_default_branch_sha,
                            report.verification_operation.id
                        );
                        println!(
                            "Resolved {} exposure(s) and {} advisory record(s).",
                            report.resolved_exposures.len(),
                            report.resolved_advisories.len()
                        );
                    }
                }
                other => {
                    return Err(UsageError::Message(format!(
                        "unknown exposures action {other:?} — expected plan or apply"
                    )));
                }
            }
        }
        "note" => {
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            match parsed.positional.first().map(String::as_str) {
                Some("send") if parsed.positional.len() == 1 => {
                    let sender = parsed.session.ok_or_else(|| {
                        UsageError::Message("note send requires --session <sender>".into())
                    })?;
                    let recipient = parsed.to_session.ok_or_else(|| {
                        UsageError::Message("note send requires --to-session <recipient>".into())
                    })?;
                    let message = parsed.message.as_deref().ok_or_else(|| {
                        UsageError::Message("note send requires --message <text>".into())
                    })?;
                    let note = broker.send_session_note(sender, recipient, message)?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&note)?);
                    } else {
                        println!(
                            "Sent broker note {} from session {} to session {}.",
                            note.id, note.sender_session_id, note.recipient_session_id
                        );
                    }
                }
                Some("list") if parsed.positional.len() == 1 => {
                    let recipient = parsed.session.ok_or_else(|| {
                        UsageError::Message("note list requires --session <recipient>".into())
                    })?;
                    let list = broker.session_note_list(recipient)?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&list)?);
                    } else if list.notes.is_empty() {
                        println!("No broker notes for session {recipient}.");
                    } else {
                        println!("{:<5} {:<8} {:<14} MESSAGE", "ID", "FROM", "STATE");
                        for note in &list.notes {
                            println!(
                                "{:<5} {:<8} {:<14} {}",
                                note.id,
                                note.sender_session_id,
                                if note.acknowledged_at.is_some() {
                                    "acknowledged"
                                } else {
                                    "unread"
                                },
                                note.message
                            );
                        }
                        println!("Unread: {}", list.unread_count);
                    }
                }
                Some("ack") if parsed.positional.len() == 1 => {
                    let recipient = parsed.session.ok_or_else(|| {
                        UsageError::Message("note ack requires --session <recipient>".into())
                    })?;
                    let note_id = parsed.note_id.ok_or_else(|| {
                        UsageError::Message("note ack requires --id <note-id>".into())
                    })?;
                    let note = broker.acknowledge_session_note(recipient, note_id)?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&note)?);
                    } else {
                        println!("Acknowledged broker note {}.", note.id);
                    }
                }
                Some(other) => {
                    return Err(UsageError::Message(format!(
                        "unknown note action {other:?} — expected send, list, or ack"
                    )));
                }
                None => {
                    return Err(UsageError::Message(
                        "note requires an action: send, list, or ack".into(),
                    ));
                }
            }
        }
        "operations" => {
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            match parsed.positional.first().map(String::as_str) {
                None | Some("list") => {
                    if parsed.positional.len() > 1 {
                        return Err(UsageError::Message(
                            "operations list does not accept positional arguments".into(),
                        ));
                    }
                    let query = operation_history_query(&parsed)?;
                    let page = broker.store().operation_history(&query)?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&page)?);
                    } else if page.operations.is_empty() {
                        println!("No coordinated operations recorded.");
                    } else {
                        println!(
                            "{:<5} {:<8} {:<21} {:<22} SCOPE",
                            "ID", "TOOL", "STATUS", "REPOSITORY"
                        );
                        for operation in page.operations {
                            println!(
                                "{:<5} {:<8} {:<21} {:<22} {}",
                                operation.id,
                                operation.provider.as_str(),
                                operation.status.as_str(),
                                operation.repository,
                                operation.scope,
                            );
                        }
                        if let Some(before_id) = page.next_before_id {
                            println!("More operations: pass --before {before_id}.");
                        }
                    }
                }
                Some("show") => {
                    if parsed.positional.len() != 2 {
                        return Err(UsageError::Message(OPERATIONS_SHOW_USAGE.into()));
                    }
                    let operation_id = parsed.positional[1].parse::<i64>().map_err(|_| {
                        UsageError::Message(format!(
                            "operation id must be a positive integer; {OPERATIONS_SHOW_USAGE}"
                        ))
                    })?;
                    if operation_id <= 0 {
                        return Err(UsageError::Message(format!(
                            "operation id must be a positive integer; {OPERATIONS_SHOW_USAGE}"
                        )));
                    }
                    let report = broker.show_coordinated_operation(operation_id)?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        render_operation_show(&report);
                    }
                }
                Some("reconcile") => {
                    if parsed.operation.is_none()
                        || parsed.outcome.is_none()
                        || parsed.reason.as_deref().is_none_or(str::is_empty)
                    {
                        return Err(operations_reconcile_error(
                            "incomplete operation reconciliation request",
                        ));
                    }
                    let operation = parsed.operation.expect("validated operation id");
                    let outcome = parsed.outcome.as_deref().expect("validated outcome");
                    let succeeded = match outcome {
                        "succeeded" => true,
                        "failed" => false,
                        _ => {
                            return Err(operations_reconcile_error(
                                "--outcome must be succeeded or failed",
                            ));
                        }
                    };
                    let reason = parsed.reason.as_deref().expect("validated reason");
                    let report = broker
                        .reconcile_coordinated_operation(operation, succeeded, reason)
                        .map_err(operations_reconcile_error)?;
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
                        "unknown operations action {other:?} — expected list, show, or reconcile"
                    )));
                }
            }
        }
        "gates" => {
            let action = parsed
                .positional
                .first()
                .map(String::as_str)
                .ok_or(UsageError::Message(
                    "gates requires an action: draft, validate, manifest, scope, affected, semantic, run, or pre-push"
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
                    let cwd = std::env::current_dir()
                        .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
                    let checkout = crate::GitRepo::discover(&cwd)?;
                    let gates = aethyme_gates_load(checkout.root())?;
                    if parsed.json {
                        let summary: Vec<_> = gates
                            .iter()
                            .map(|g| {
                                serde_json::json!({
                                    "name": g.name, "command": g.command,
                                    "cost": g.cost, "triggers": g.triggers,
                                    "cache": g.cache,
                                    "resources": g.resources,
                                    "resource_ttl_seconds": g.resource_ttl_seconds,
                                    "resource_wait_seconds": g.resource_wait_seconds,
                                    "managed_cache": g.managed_cache,
                                    "definition_hash": g.definition_hash,
                                })
                            })
                            .collect();
                        println!("{}", serde_json::to_string_pretty(&summary)?);
                    } else {
                        println!("gates.toml OK — {} gate(s), cheap-first:", gates.len());
                        for gate in gates {
                            println!(
                                "  [{}] {} — {} (triggers: {}{}; resources: {}; definition: {})",
                                gate.cost,
                                gate.name,
                                gate.command,
                                if gate.triggers.is_empty() {
                                    "always".to_string()
                                } else {
                                    gate.triggers.join(", ")
                                },
                                if gate.cache { "" } else { "; cache: off" },
                                gate.resources.len(),
                                &gate.definition_hash[..12],
                            );
                        }
                    }
                }
                "manifest" => {
                    let cwd = std::env::current_dir()
                        .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
                    let checkout = crate::GitRepo::discover(&cwd)?;
                    let head = parsed.head.as_deref().unwrap_or("HEAD");
                    let (head_sha, gates) = crate::load_gates_at_commit(&checkout, head)?;
                    let manifest = crate::gate_scope_manifest(&gates);
                    if parsed.json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "policy_head_sha": head_sha,
                                "manifest": manifest,
                            }))?
                        );
                    } else {
                        println!(
                            "Gate scope manifest {} at {}",
                            &manifest.manifest_sha256[..12],
                            &head_sha[..12]
                        );
                        println!("  schema: {}", manifest.schema_version);
                        println!("  gates: {}", manifest.gates.len());
                        println!("  semantic suggestions enforced: false");
                        for gate in manifest.gates {
                            println!(
                                "  [{}] {} (triggers: {}; cache: {}; resources: {})",
                                gate.cost,
                                gate.name,
                                if gate.triggers.is_empty() {
                                    "always".into()
                                } else {
                                    gate.triggers.join(", ")
                                },
                                if gate.cache { "use" } else { "disabled" },
                                gate.resources.len()
                            );
                        }
                    }
                }
                "scope" => {
                    let base = parsed.base.as_deref().ok_or(UsageError::Message(
                        "gates scope requires --base <ref> and --head <ref>".into(),
                    ))?;
                    let head = parsed.head.as_deref().ok_or(UsageError::Message(
                        "gates scope requires --base <ref> and --head <ref>".into(),
                    ))?;
                    let cwd = std::env::current_dir()
                        .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
                    let checkout = crate::GitRepo::discover(&cwd)?;
                    let (_, gates) = crate::load_gates_at_commit(&checkout, head)?;
                    let report = crate::evaluate_gate_scope(&checkout, &gates, base, head)?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        println!(
                            "Gate scope {}..{} (manifest {})",
                            &report.base_sha[..12],
                            &report.head_sha[..12],
                            &report.manifest_sha256[..12]
                        );
                        println!("  changed paths: {}", report.changed_paths.len());
                        if report.selected_gates.is_empty() {
                            println!("  selected gates: none");
                        } else {
                            println!("  selected gates:");
                            for selection in report.selected_gates {
                                match selection.triggered_by {
                                    Some(path) => println!("    {} ({path})", selection.gate),
                                    None => println!("    {} (always)", selection.gate),
                                }
                            }
                        }
                        println!("  semantic suggestions: advisory, not included");
                    }
                }
                "affected" => {
                    let session = parsed.session.ok_or(UsageError::Message(
                        "gates affected requires --session <id>".into(),
                    ))?;
                    let mut broker = open_broker(parsed.read_only_snapshot)?;
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
                    let mut broker = open_broker(parsed.read_only_snapshot)?;
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
                    let mut broker = open_broker(parsed.read_only_snapshot)?;
                    let policy = if parsed.no_cache {
                        crate::CachePolicy::Bypass
                    } else {
                        crate::CachePolicy::Use
                    };
                    let outcomes = if let Some(gate) = parsed.only.as_deref() {
                        broker.run_named_gate_for_checkout_with_policy(&cwd, gate, policy)?
                    } else {
                        broker.run_all_gates_with_policy(&cwd, policy)?
                    };
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&outcomes)?);
                    } else {
                        for outcome in &outcomes {
                            println!(
                                "{:<20} {:<10} {}{} (tree {})",
                                outcome.gate,
                                gate_status_label(outcome.status, outcome.failure_class),
                                if outcome.cached { "(cached) " } else { "" },
                                outcome
                                    .duration_ms
                                    .map(|ms| format!("{ms}ms"))
                                    .unwrap_or_default(),
                                short_commit(&outcome.tree_hash),
                            );
                            render_gate_failure_tail(outcome);
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
                    let mut broker = open_broker(parsed.read_only_snapshot)?;
                    let policy = if parsed.no_cache {
                        crate::CachePolicy::Bypass
                    } else {
                        crate::CachePolicy::Use
                    };
                    let outcomes = if let Some(gate) = parsed.only.as_deref() {
                        broker.run_named_gate_with_policy(session, gate, policy)?
                    } else {
                        broker.run_gates_with_policy(session, policy)?
                    };
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&outcomes)?);
                    } else if outcomes.is_empty() {
                        println!("No gates affected — nothing to run.");
                    } else {
                        let mut failed = false;
                        for outcome in &outcomes {
                            println!(
                                "{:<20} {:<10} {}{} (tree {})",
                                outcome.gate,
                                gate_status_label(outcome.status, outcome.failure_class),
                                if outcome.cached { "(cached) " } else { "" },
                                outcome
                                    .duration_ms
                                    .map(|ms| format!("{ms}ms"))
                                    .unwrap_or_default(),
                                short_commit(&outcome.tree_hash),
                            );
                            render_gate_failure_tail(outcome);
                            failed |= outcome.status.as_str() != "pass";
                        }
                        if failed {
                            return Err(UsageError::Message(
                                "one or more gates did not pass".into(),
                            ));
                        }
                    }
                }
                "pre-push" => {
                    if parsed.session.is_some() || parsed.all {
                        return Err(UsageError::Message(
                            "gates pre-push does not take --session or --all; it always validates the complete pushed tree".into(),
                        ));
                    }
                    let remote = parsed.positional.get(1).ok_or(UsageError::Message(
                        "gates pre-push requires Git's <remote-name> argument".into(),
                    ))?;
                    if parsed.positional.len() > 3 {
                        return Err(UsageError::Message(
                            "gates pre-push takes only Git's <remote-name> and optional <remote-url> arguments".into(),
                        ));
                    }
                    let mut hook_input = String::new();
                    std::io::stdin()
                        .read_to_string(&mut hook_input)
                        .map_err(|error| {
                            UsageError::Message(format!("cannot read pre-push stdin: {error}"))
                        })?;
                    let cwd = std::env::current_dir().map_err(|error| {
                        UsageError::Message(format!("cannot resolve cwd: {error}"))
                    })?;
                    let mut broker = open_broker(parsed.read_only_snapshot)?;
                    let report = broker.run_pre_push_gates(
                        &cwd,
                        remote,
                        &hook_input,
                        if parsed.no_cache {
                            crate::CachePolicy::Bypass
                        } else {
                            crate::CachePolicy::Use
                        },
                    )?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else if report.plan.pushed_sha.is_none() {
                        println!("Pre-push: deletion-only update; no content gates required.");
                    } else {
                        for outcome in &report.gate_outcomes {
                            println!(
                                "{:<20} {:<10} {}{} (tree {})",
                                outcome.gate,
                                gate_status_label(outcome.status, outcome.failure_class),
                                if outcome.cached { "(cached) " } else { "" },
                                outcome
                                    .duration_ms
                                    .map(|ms| format!("{ms}ms"))
                                    .unwrap_or_default(),
                                short_commit(&outcome.tree_hash),
                            );
                        }
                        println!(
                            "Pre-push: verified {} for {} ref update(s) to {}.",
                            short_commit(report.plan.pushed_sha.as_deref().unwrap_or_default()),
                            report.plan.updates.len(),
                            report.plan.remote,
                        );
                    }
                    if report
                        .gate_outcomes
                        .iter()
                        .any(|outcome| outcome.status != crate::GateStatus::Pass)
                    {
                        return Err(UsageError::Message(
                            "one or more pre-push gates did not pass".into(),
                        ));
                    }
                }
                other => {
                    return Err(UsageError::Message(format!(
                        "unknown gates action {other:?} — expected draft, validate, manifest, scope, affected, semantic, run, or pre-push"
                    )));
                }
            }
        }
        "watch" => run_pull_request_watch(parsed)?,
        "pr" => {
            let action = parsed
                .positional
                .first()
                .map(String::as_str)
                .ok_or(UsageError::Message("pr requires an action: check".into()))?;
            match action {
                "check" => {
                    let mut broker = open_broker(parsed.read_only_snapshot)?;
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
                "pre-commit" => {
                    if let Err(err) = crate::hooks::run_pre_commit(&cwd) {
                        if let Some(code) = err.exit_code() {
                            return Err(UsageError::Exit {
                                message: err.to_string(),
                                code,
                            });
                        }
                        return Err(err.into());
                    }
                }
                "post-commit" => crate::hooks::run_post_commit(&cwd),
                "pre-push" => {
                    if parsed.positional.len() > 3 {
                        return Err(UsageError::Message(
                            "hooks pre-push takes Git's remote name and optional URL only".into(),
                        ));
                    }
                    let mut updates = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut updates).map_err(
                        |error| {
                            UsageError::Message(format!(
                                "cannot read pre-push ref updates: {error}"
                            ))
                        },
                    )?;
                    crate::hooks::run_pre_push(&cwd, &updates)?;
                }
                other => {
                    return Err(UsageError::Message(format!(
                        "unknown hooks action {other:?} — expected install, uninstall, status, pre-commit, post-commit, or pre-push"
                    )));
                }
            }
        }
        "submit" => {
            let session = parsed
                .session
                .ok_or(UsageError::Message("submit requires --session <id>".into()))?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            // Preflight (dogfood feedback 2026-07-14): show exactly what
            // will be submitted before anything runs — and warn about
            // uncommitted work, which never integrates.
            if !parsed.json
                && let Ok(info) = broker.store().session(session)
                && let Ok(checkout) =
                    crate::GitRepo::discover(std::path::Path::new(&info.worktree_path))
            {
                let plan = broker.submission_plan(session)?;
                render_submission_plan(&plan, &checkout);
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
            let outcome = broker.submit_with_policy(
                session,
                if parsed.no_cache {
                    crate::CachePolicy::Bypass
                } else {
                    crate::CachePolicy::Use
                },
            )?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&outcome)?);
            } else if !outcome.conflicts.is_empty() {
                eprintln!("✗ conflict — rejected before any gate ran. Conflicting files:");
                for conflict in &outcome.conflict_details {
                    eprintln!(
                        "  - {} from session commit {} ({})",
                        conflict.path,
                        conflict.originating_commit,
                        conflict.ownership.as_str()
                    );
                    if !conflict.integration_side_commits.is_empty() {
                        eprintln!(
                            "    integration side: {}",
                            conflict.integration_side_commits.join(", ")
                        );
                    }
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
                if let Some(graph) = &outcome.graph_integrity
                    && graph.enforced
                {
                    println!(
                        "graph integrity: {:?} (tree {}, policy {}) — {}",
                        graph.status,
                        short_commit(&graph.tree_hash),
                        short_commit(&graph.policy_digest),
                        graph.reason
                    );
                    if !graph.changed_paths.is_empty() {
                        println!("  stale graph paths: {}", graph.changed_paths.join(", "));
                    }
                }
                let gate_wall_ms: i64 = outcome
                    .gate_outcomes
                    .iter()
                    .filter(|gate| !gate.cached)
                    .filter_map(|gate| gate.duration_ms)
                    .sum();
                for gate in &outcome.gate_outcomes {
                    if gate.cached {
                        println!(
                            "gate {:<20} {} (cached, tree {}, saved {})",
                            gate.gate,
                            gate_status_label(gate.status, gate.failure_class),
                            short_commit(&gate.tree_hash),
                            duration_label(gate.duration_ms)
                        );
                    } else {
                        println!(
                            "gate {:<20} {} in {} (tree {})",
                            gate.gate,
                            gate_status_label(gate.status, gate.failure_class),
                            duration_label(gate.duration_ms),
                            short_commit(&gate.tree_hash),
                        );
                    }
                    render_gate_failure_tail(gate);
                }
                match outcome.gate_verification.status {
                    crate::SubmissionGateVerificationStatus::NotRun => {}
                    crate::SubmissionGateVerificationStatus::NoConfiguration => println!(
                        "verification: conflict-only — no .aethyme/gates.toml exists in the submitted tree; 0 gates selected"
                    ),
                    crate::SubmissionGateVerificationStatus::NoGatesTriggered => println!(
                        "verification: no gate matched this diff ({} configured, 0 selected); review triggers with `aethyme broker gates affected --session {}`",
                        outcome.gate_verification.configured_gates, outcome.entry.session_id
                    ),
                    crate::SubmissionGateVerificationStatus::Passed => println!(
                        "verification: {} selected gate(s) passed ({} executed, {} cached)",
                        outcome.gate_verification.selected_gates,
                        outcome.gate_verification.executed_gates,
                        outcome.gate_verification.cached_gates
                    ),
                    crate::SubmissionGateVerificationStatus::Failed => println!(
                        "verification: {} selected gate(s) did not all pass",
                        outcome.gate_verification.selected_gates
                    ),
                }
                if !outcome.no_changes {
                    println!("gate wall time: {}ms", gate_wall_ms);
                }
                if outcome.entry.status.as_str() == "verified"
                    && matches!(
                        outcome.gate_verification.status,
                        crate::SubmissionGateVerificationStatus::NoConfiguration
                            | crate::SubmissionGateVerificationStatus::NoGatesTriggered
                    )
                {
                    println!(
                        "entry {} → conflict-checked (eligible for manual promotion; no gate verification)",
                        outcome.entry.id
                    );
                } else {
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
                }
                if outcome.no_changes {
                    println!(
                        "What now: no pending session-owned content remains to integrate; \
                         aethyme/integration was not moved and no gates ran."
                    );
                    return Ok(());
                }
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
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report = broker.repair(session)?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                render_repair_report(&report);
            }
        }
        "checkpoint" => {
            let action =
                parsed
                    .positional
                    .first()
                    .map(String::as_str)
                    .ok_or(UsageError::Message(
                        "checkpoint requires plan or apply".into(),
                    ))?;
            let session = parsed.session.ok_or(UsageError::Message(
                "checkpoint plan/apply requires --session <id>".into(),
            ))?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            match action {
                "plan" => {
                    let report = broker.plan_session_checkpoint_recovery(session)?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        println!(
                            "Checkpoint recovery for session {}: {}",
                            session,
                            if report.safe { "safe" } else { "refused" }
                        );
                        println!(
                            "  old: {}",
                            report.old_checkpoint.as_deref().unwrap_or("missing")
                        );
                        println!(
                            "  proposed: {}",
                            report.proposed_checkpoint.as_deref().unwrap_or("missing")
                        );
                        println!(
                            "  session HEAD: {} ({}; {} ahead, {} behind)",
                            report.session_head,
                            report
                                .integration_relation
                                .map(|relation| relation.as_str())
                                .unwrap_or("unknown"),
                            report.ahead_commits,
                            report.behind_commits
                        );
                        println!("  pending commits: {}", report.pending_commits.len());
                        println!("  preservation branch: {}", report.preservation_branch);
                        for refusal in &report.refusals {
                            println!("  refusal: {refusal}");
                        }
                        if !report.next_actions.is_empty() {
                            println!("  recovery actions:");
                            for action in &report.next_actions {
                                println!("    {}: {}", action.kind, action.command);
                                println!("      {}", action.description);
                            }
                        }
                        println!("Plan digest: {}", report.digest);
                        if report.safe {
                            println!(
                                "Apply with: aethyme broker checkpoint apply --session {} --confirm {}",
                                session, report.digest
                            );
                        }
                    }
                }
                "apply" => {
                    let confirm = parsed.confirm.as_deref().ok_or(UsageError::Message(
                        "checkpoint apply requires --confirm <sha256>".into(),
                    ))?;
                    let report = broker.apply_session_checkpoint_recovery(session, confirm)?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        println!(
                            "Re-anchored session {} at {} after preserving {}.",
                            session, report.accepted_session_head, report.preservation_ref
                        );
                        println!("Next: aethyme broker submit --session {session}");
                    }
                }
                other => {
                    return Err(UsageError::Message(format!(
                        "unknown checkpoint action {other:?} — expected plan or apply"
                    )));
                }
            }
        }
        "queue" => {
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            if parsed.positional.first().map(String::as_str) == Some("history") {
                if parsed.positional.len() != 1 {
                    return Err(UsageError::Message(
                        "queue history accepts no positional arguments".into(),
                    ));
                }
                let page = broker
                    .store()
                    .merge_queue_history_page(parsed.limit.unwrap_or(50), parsed.before)?;
                if parsed.json {
                    println!("{}", serde_json::to_string_pretty(&page)?);
                } else {
                    render_queue_history(&page);
                }
                return Ok(());
            }
            if !parsed.positional.is_empty() || parsed.limit.is_some() || parsed.before.is_some() {
                return Err(UsageError::Message(
                    "queue accepts no selectors; use `queue history [--limit <n>] [--before <id>]`"
                        .into(),
                ));
            }
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
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            broker.promote(entry)?;
            if parsed.json {
                println!("{{\"promoted\":{entry}}}");
            } else {
                println!("Promoted entry {entry} to the local integration branch.");
                println!("Next: aethyme broker ship plan --entry {entry}");
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
                    let mut broker = open_broker(parsed.read_only_snapshot)?;
                    let report = broker.ship_plan(entry)?;
                    render_ship_plan(&report, parsed.json)?;
                }
                "execute" => {
                    let entry = parsed.entry.ok_or(UsageError::Message(
                        "ship execute requires --entry <id>".into(),
                    ))?;
                    let confirm = parsed.confirm.as_deref().ok_or(UsageError::Message(
                        "ship execute requires --confirm <full-integration-sha>".into(),
                    ))?;
                    let mut broker = open_broker(parsed.read_only_snapshot)?;
                    let report = broker.ship_execute_with_policy(
                        entry,
                        confirm,
                        parsed.sync_main,
                        parsed.break_glass,
                        parsed.reason.as_deref(),
                    )?;
                    render_ship_execution(&report, parsed.json)?;
                }
                other => {
                    return Err(UsageError::Message(format!(
                        "unknown ship action {other:?} — expected plan or execute"
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
                    let mut broker = open_broker(parsed.read_only_snapshot)?;
                    let report = if parsed.read_only_snapshot {
                        broker.integration_status_snapshot()?
                    } else {
                        broker.integration_status(now_ms())?
                    };
                    render_integration_status(&report, parsed.json)?;
                }
                "wait-stable" => {
                    let seconds = parsed.seconds.unwrap_or(30);
                    let mut broker = open_broker(parsed.read_only_snapshot)?;
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
                        return Err(UsageError::Message(format!(
                            "choose either --dry-run or --apply, not both; {INTEGRATION_RECONCILE_USAGE}"
                        )));
                    }
                    let upstream = parsed
                        .upstream
                        .clone()
                        .ok_or(UsageError::Message(INTEGRATION_RECONCILE_USAGE.into()))?;
                    if parsed.apply && parsed.confirm.is_none() {
                        return Err(UsageError::Message(INTEGRATION_RECONCILE_USAGE.into()));
                    }
                    if parsed.apply && parsed.write_resolution_template.is_some() {
                        return Err(UsageError::Message(format!(
                            "--write-resolution-template is a dry-run aid and cannot be combined with --apply; {INTEGRATION_RECONCILE_USAGE}"
                        )));
                    }
                    let mut broker = open_broker(parsed.read_only_snapshot)?;
                    let report =
                        broker.reconcile_integration(crate::IntegrationReconcileOptions {
                            upstream,
                            apply: parsed.apply,
                            resolution_file: parsed.resolution_file.clone(),
                            confirm: parsed.confirm.clone(),
                        })?;
                    if let Some(path) = parsed.write_resolution_template.as_deref() {
                        let template = report.resolution_template.as_ref().ok_or_else(|| {
                            UsageError::Message(
                                "no reconciliation resolution template is required for this plan"
                                    .into(),
                            )
                        })?;
                        write_reconciliation_resolution_template(path, &template.document)?;
                        eprintln!("Wrote resolution template to {}", path.display());
                    }
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
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let status = if parsed.read_only_snapshot {
                broker.status_snapshot(now_ms())?
            } else {
                broker.status(now_ms())?
            };
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
                        "Upstream:    {} @ {} ({})",
                        upstream_ref,
                        short_commit(upstream_head),
                        upstream_relation(
                            status.main_ahead_upstream_commits,
                            status.main_behind_upstream_commits,
                        )
                    );
                }
                println!("Summary: {}", status.summary.message);
                println!();
                render_status_advice(&status.advice);
                if !status.outstanding_advisories.is_empty() {
                    println!();
                    println!(
                        "Outstanding advisories: {}",
                        status.outstanding_advisories.len()
                    );
                    for advisory in status.outstanding_advisories.iter().take(10) {
                        println!(
                            "  {} [{}]: {}",
                            advisory.id,
                            advisory.severity.as_str(),
                            advisory_text(&advisory.identity),
                        );
                        println!(
                            "    inspect: aethyme broker advisories show {}",
                            advisory.id
                        );
                        println!(
                            "    acknowledge: aethyme broker advisories ack {}",
                            advisory.id
                        );
                    }
                    if status.outstanding_advisories.len() > 10 {
                        println!(
                            "  and {} more; inspect: aethyme broker advisories list",
                            status.outstanding_advisories.len() - 10
                        );
                    }
                }
                if !status.outstanding_entry_exposures.is_empty() {
                    println!();
                    println!(
                        "Publication exposures: {} promoted {} not yet verified on remote main",
                        status.outstanding_entry_exposures.len(),
                        plural(status.outstanding_entry_exposures.len(), "entry", "entries")
                    );
                    for exposure in status.outstanding_entry_exposures.iter().take(10) {
                        println!(
                            "  qid {} @ {}: {} {}",
                            exposure.queue_entry_id,
                            short_commit(&exposure.promotion_sha),
                            exposure.paths.len(),
                            plural(exposure.paths.len(), "path", "paths")
                        );
                    }
                    if status.outstanding_entry_exposures.len() > 10 {
                        println!(
                            "  and {} more",
                            status.outstanding_entry_exposures.len() - 10
                        );
                    }
                    println!("  inspect: aethyme broker exposures plan");
                }
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
                let explicit_leases = status
                    .leases
                    .iter()
                    .filter(|lease| lease.kind == crate::LeaseKind::Explicit)
                    .collect::<Vec<_>>();
                if !explicit_leases.is_empty() {
                    println!();
                    println!("Planned explicit leases:");
                    for lease in explicit_leases {
                        println!("  session {}: {}", lease.session_id, lease.path);
                    }
                }
                let current_queue = status
                    .queue
                    .iter()
                    .filter(|entry| queue_status_is_current(entry.status))
                    .collect::<Vec<_>>();
                if !current_queue.is_empty() {
                    println!();
                    println!("Current merge queue:");
                    println!("{:<4} {:<4} {:<11} HEAD", "QID", "SID", "QSTATUS");
                    for entry in current_queue {
                        println!(
                            "{:<4} {:<4} {:<11} {}",
                            entry.id,
                            entry.session_id,
                            entry.status.as_str(),
                            &entry.head_commit[..12.min(entry.head_commit.len())]
                        );
                    }
                }
                let terminal_counts = &status.queue_history.terminal_counts;
                if !terminal_counts.is_empty() {
                    let total = terminal_counts.iter().map(|item| item.count).sum::<usize>();
                    let summary = terminal_counts
                        .iter()
                        .map(|item| format!("{} {}", item.status.as_str(), item.count))
                        .collect::<Vec<_>>()
                        .join(", ");
                    println!();
                    println!(
                        "Queue history: {total} terminal {} ({summary}).",
                        plural(total, "entry", "entries")
                    );
                    println!("  inspect: {}", status.queue_history.command);
                }
                if status.advisory_delivery.shown_advisories > 0 {
                    println!();
                    println!(
                        "Advisory delivery: {} shown, {} actioned, {} displays.",
                        status.advisory_delivery.shown_advisories,
                        status.advisory_delivery.actioned_advisories,
                        status.advisory_delivery.total_shows,
                    );
                    println!("  inspect: aethyme broker advisories metrics");
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
                let mut broker = open_broker(parsed.read_only_snapshot)?;
                let cutoff = now_ms() - keep_days * 24 * 60 * 60 * 1000;
                let removed = broker.store().prune_events_before(cutoff)?;
                if parsed.json {
                    println!("{{\"pruned\":{removed}}}");
                } else {
                    println!("Pruned {removed} event(s) older than {keep_days} day(s).");
                }
                return Ok(());
            }
            let mut broker = open_broker(parsed.read_only_snapshot)?;
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
            let mut broker = open_broker(parsed.read_only_snapshot)?;
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
            let mut broker = open_broker(parsed.read_only_snapshot)?;
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
                        println!("  duration: {}ms", repair.duration_ms);
                        if let Some(code) = repair.exit_code {
                            println!("  exit: {code}");
                        }
                        for step in &repair.steps {
                            println!(
                                "  {} {}: {}",
                                step.component,
                                step.action,
                                if step.success { "pass" } else { "fail" }
                            );
                            println!("    command: {}", step.command.join(" "));
                            if let Some(code) = step.exit_code {
                                println!("    exit: {code}");
                            }
                        }
                        if repair.steps.is_empty() {
                            println!("  command: {}", repair.command.join(" "));
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
                println!(
                    "retention: {} rows, {} files, {} worktrees, {} reclaimable; {} protected findings",
                    report.retention.candidate_rows,
                    report.retention.candidate_files,
                    report.retention.candidate_worktrees,
                    human_bytes(report.retention.estimated_reclaimable_bytes),
                    report.retention.blockers,
                );
                if let Some(digest) = &report.retention.pending_recovery_digest {
                    println!("  recovery pending: aethyme broker gc apply --confirm {digest}");
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
            let mut broker = open_broker(parsed.read_only_snapshot)?;
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
        "handoff" => {
            let broker = open_broker(parsed.read_only_snapshot)?;
            let report = match (parsed.session, parsed.worktree.as_deref()) {
                (Some(session), None) => broker.latest_handoff_for_session(session)?,
                (None, Some(worktree)) => {
                    let worktree = resolve_handoff_worktree(worktree)?;
                    broker.latest_handoff_for_worktree(&worktree)?
                }
                (Some(_), Some(_)) => {
                    return Err(UsageError::Message(
                        "handoff takes either --session <id> or --worktree <path>, not both".into(),
                    ));
                }
                (None, None) => {
                    return Err(UsageError::Message(
                        "handoff requires --session <id> or --worktree <path>".into(),
                    ));
                }
            };
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                render_handoff_report(&report);
            }
        }
        "finish" => {
            let session = parsed
                .session
                .ok_or(UsageError::Message("finish requires --session <id>".into()))?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report = broker.finish_with_options(
                session,
                crate::FinishOptions {
                    keep_worktree: parsed.keep_worktree,
                },
            )?;
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
            let mut broker = open_broker(parsed.read_only_snapshot)?;
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
        "gc" => {
            let action = parsed
                .positional
                .first()
                .map(String::as_str)
                .ok_or_else(|| {
                    UsageError::Message("gc requires `plan` or `apply --confirm <sha256>`".into())
                })?;
            if parsed.positional.len() != 1 {
                return Err(UsageError::Message(
                    "gc accepts exactly one action: `plan` or `apply --confirm <sha256>`".into(),
                ));
            }
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            match action {
                "plan" => {
                    if parsed.confirm.is_some() {
                        return Err(UsageError::Message(
                            "gc plan does not accept --confirm; review its emitted digest".into(),
                        ));
                    }
                    let plan = broker.gc_plan()?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&plan)?);
                    } else {
                        render_gc_plan(&plan);
                    }
                }
                "apply" => {
                    let confirm = parsed.confirm.as_deref().ok_or_else(|| {
                        UsageError::Message("gc apply requires --confirm <sha256>".into())
                    })?;
                    let report = broker.gc_apply(confirm)?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        render_gc_apply(&report);
                    }
                    if !report.complete {
                        return Err(UsageError::Message(
                            report.recovery_action.unwrap_or_else(|| {
                                format!(
                                    "GC paused; resume with `aethyme broker gc apply --confirm {confirm}`"
                                )
                            }),
                        ));
                    }
                }
                other => {
                    return Err(UsageError::Message(format!(
                        "unknown gc action {other:?}; expected `plan` or `apply`"
                    )));
                }
            }
        }
        "cleanup" => {
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            if parsed.all_cleaned {
                if !parsed.positional.is_empty() || parsed.force || parsed.dry_run {
                    return Err(UsageError::Message(
                        "cleanup --all-cleaned takes no session id, --force, or --dry-run; planning is already the default and --apply removes only revalidated eligible worktrees"
                            .into(),
                    ));
                }
                if !parsed.apply && parsed.confirm.is_some() {
                    return Err(UsageError::Message(
                        "cleanup --all-cleaned --confirm requires --apply; review the current plan first"
                            .into(),
                    ));
                }
                let report =
                    broker.cleanup_cleaned_worktrees(parsed.apply, parsed.confirm.as_deref())?;
                if parsed.json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    render_cleanup_sweep_report(&report);
                }
            } else {
                if parsed.apply {
                    return Err(UsageError::Message(
                        "cleanup <session-id> does not take --apply; use the exact command after finish reports cleanup safe"
                            .into(),
                    ));
                }
                let id: i64 = parsed
                    .positional
                    .first()
                    .ok_or(UsageError::Message(
                        "cleanup requires a session id or --all-cleaned".into(),
                    ))?
                    .parse()
                    .map_err(|_| UsageError::Message("session id must be an integer".into()))?;
                broker.cleanup(id, parsed.force)?;
                if parsed.json {
                    println!("{{\"cleaned\":{id}}}");
                } else {
                    println!("Cleaned session {id}.");
                }
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

/// Every broker invocation associated with a live session surfaces its
/// outstanding durable advisories and unread local notes on stderr. Stdout
/// remains untouched so all existing `--json` contracts stay parseable. This
/// is best effort: notification failure never changes command behavior, and
/// the delivery metric path never creates broker state for an offline read.
fn surface_command_advisories(subcommand: &str, parsed: &Parsed) {
    // Internal hooks are not interactive broker commands. Pre-commit remains
    // quiet on success, while post-commit surfaces through run_post_commit
    // after its conflict radar and therefore does not print twice.
    if subcommand == "hooks" {
        return;
    }
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    let Ok(checkout) = crate::GitRepo::discover(&cwd) else {
        return;
    };
    let Ok(main_root) = checkout.main_root() else {
        return;
    };
    if !main_root.join(crate::BROKER_DB_RELPATH).is_file() {
        return;
    }
    let store = if parsed.read_only_snapshot {
        crate::BrokerStore::open_snapshot_in_repo(&main_root)
    } else {
        crate::BrokerStore::open_in_repo(&main_root)
    };
    let Ok(mut store) = store else {
        return;
    };
    let session_id = parsed.session.or_else(|| {
        store
            .session_for_worktree(checkout.root().to_string_lossy().as_ref())
            .ok()
            .flatten()
            .map(|session| session.id)
    });
    let Some(session_id) = session_id else {
        return;
    };
    let Ok(advisories) = store.outstanding_advisories_for_session(session_id) else {
        return;
    };
    if !parsed.read_only_snapshot {
        let _ = store.record_advisories_shown(&advisories, crate::AdvisoryDeliverySurface::Command);
    }
    for line in crate::advisories::session_notice_lines(&advisories) {
        eprintln!("{line}");
    }
    let Ok(notes) = store.unread_session_notes(session_id) else {
        return;
    };
    for note in notes {
        eprintln!(
            "Unread broker note {} from session {}: {}",
            note.id, note.sender_session_id, note.message
        );
        eprintln!(
            "  acknowledge: aethyme broker note ack --session {} --id {}",
            session_id, note.id
        );
    }
}
